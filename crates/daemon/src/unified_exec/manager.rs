use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::head_tail_buffer::HeadTailBuffer;
use super::process::UnifiedExecProcess;
use super::{
    clamp_yield_time, resolve_max_tokens, ExecOutput, OpenProcessSpec, ProcessInfo, ReadBudget,
    DEFAULT_MAX_BACKGROUND_TERMINAL_TIMEOUT_MS, EARLY_EXIT_GRACE_PERIOD_MS,
    MAX_UNIFIED_EXEC_PROCESSES, UNIFIED_EXEC_OUTPUT_MAX_BYTES,
};
use crate::unified_exec::UnifiedExecError;
use codypendent_protocol::{RunId, SessionId};

struct ProcessEntry {
    process: Arc<UnifiedExecProcess>,
    session_id: SessionId,
    run_id: RunId,
    command: String,
    cwd: PathBuf,
    transcript: Arc<Mutex<HeadTailBuffer>>,
    last_used: Instant,
}

#[derive(Default)]
struct ProcessStore {
    processes: HashMap<i32, ProcessEntry>,
    reserved_ids: HashSet<i32>,
}

pub struct UnifiedExecManager {
    store: Mutex<ProcessStore>,
    max_poll_yield_time_ms: u64,
    deterministic_ids: AtomicBool,
    /// How long a freshly spawned process is given to exit before it is
    /// treated as long-running and stored. Overridable so a test can assert
    /// the BRANCH rather than the host's fork latency — see
    /// [`Self::set_early_exit_grace_for_tests`].
    early_exit_grace_ms: AtomicU64,
}

impl Default for UnifiedExecManager {
    fn default() -> Self {
        Self::new()
    }
}

impl UnifiedExecManager {
    pub fn new() -> Self {
        Self {
            store: Mutex::new(ProcessStore::default()),
            max_poll_yield_time_ms: DEFAULT_MAX_BACKGROUND_TERMINAL_TIMEOUT_MS,
            deterministic_ids: AtomicBool::new(false),
            early_exit_grace_ms: AtomicU64::new(EARLY_EXIT_GRACE_PERIOD_MS),
        }
    }

    pub fn set_deterministic_process_ids_for_tests(&self, enabled: bool) {
        self.deterministic_ids.store(enabled, Ordering::SeqCst);
    }

    /// Widen the early-exit grace period for a test.
    ///
    /// The production window is 150 ms, which is a judgement about what counts
    /// as "returned immediately" — not a fact a test can rely on a short-lived
    /// process beating. Under a loaded machine `/bin/echo` can take longer than
    /// that to fork, run and exit, and a test asserting the early-exit branch
    /// then measures the host rather than the branch.
    #[cfg(test)]
    pub fn set_early_exit_grace_for_tests(&self, millis: u64) {
        self.early_exit_grace_ms.store(millis, Ordering::SeqCst);
    }

    pub async fn allocate_process_id(&self) -> i32 {
        let mut store = self.store.lock().await;
        if self.deterministic_ids.load(Ordering::SeqCst) {
            let max_id = store
                .processes
                .keys()
                .chain(store.reserved_ids.iter())
                .copied()
                .max()
                .unwrap_or(999);
            let next = max_id.max(999) + 1;
            store.reserved_ids.insert(next);
            next
        } else {
            use rand::Rng;
            let mut rng = rand::thread_rng();
            loop {
                let id = rng.gen_range(1_000..100_000);
                if !store.processes.contains_key(&id) && !store.reserved_ids.contains(&id) {
                    store.reserved_ids.insert(id);
                    return id;
                }
            }
        }
    }

    pub async fn release_process_id(&self, process_id: i32) {
        let mut store = self.store.lock().await;
        store.reserved_ids.remove(&process_id);
    }

    /// Prune stored processes if capacity exceeded (MAX_UNIFIED_EXEC_PROCESSES = 64).
    /// Protects the 8 most recently used.
    /// Prefers exited entries, else live entries whose interaction lock can be acquired.
    async fn prune_processes_if_needed(store: &mut ProcessStore) {
        if store.processes.len() < MAX_UNIFIED_EXEC_PROCESSES {
            return;
        }

        // Sort process entries by last_used descending
        let mut entries: Vec<(i32, Instant, bool)> = store
            .processes
            .iter()
            .map(|(&id, entry)| (id, entry.last_used, entry.process.has_exited()))
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.1));

        // Protect top 8 most recently used
        let candidates: Vec<i32> = entries.iter().skip(8).map(|e| e.0).collect();

        // 1. Try finding an exited candidate
        let mut victim = None;
        for &id in &candidates {
            if let Some(entry) = store.processes.get(&id) {
                if entry.process.has_exited() {
                    victim = Some(id);
                    break;
                }
            }
        }

        // 2. If no exited candidate, find least recently used candidate whose lock can be acquired
        if victim.is_none() {
            for &id in candidates.iter().rev() {
                if let Some(entry) = store.processes.get(&id) {
                    if entry.process.interaction_lock().try_lock().is_ok() {
                        victim = Some(id);
                        break;
                    }
                }
            }
        }

        if let Some(vid) = victim {
            if let Some(removed) = store.processes.remove(&vid) {
                store.reserved_ids.remove(&vid);
                removed.process.kill();
            }
        }
    }

    /// Spawn a command on a PTY, observe early exit or store in process table, and collect initial yield output.
    pub async fn exec(
        &self,
        spec: OpenProcessSpec,
        read: ReadBudget,
    ) -> Result<ExecOutput, UnifiedExecError> {
        let start = Instant::now();
        let cmd_display = format!(
            "{} {}",
            spec.program.display(),
            spec.args.as_slice().join(" ")
        );

        let process = Arc::new(UnifiedExecProcess::spawn(&spec)?);
        let handles = process.output_handles();

        // Check the early-exit grace period (150ms in production).
        let early_exit = tokio::time::timeout(
            Duration::from_millis(self.early_exit_grace_ms.load(Ordering::SeqCst)),
            handles.exit_token.cancelled(),
        )
        .await
        .is_ok();

        let yield_time =
            clamp_yield_time(Some(read.yield_time_ms), false, self.max_poll_yield_time_ms);
        let max_bytes = resolve_max_tokens(Some(read.max_output_tokens)) * 4;

        if early_exit {
            // Process exited within 150ms: collect output and return without storing
            let deadline = start + yield_time;
            let collected = process
                .collect_output_until_deadline(deadline, max_bytes)
                .await;
            let wall_time = start.elapsed();
            let exit_code = process.exit_code().unwrap_or(0);
            let raw_bytes = collected.to_bytes_with_omission_marker();
            let output_str = String::from_utf8_lossy(&raw_bytes).to_string();

            return Ok(ExecOutput {
                process_id: None,
                exit_code: Some(exit_code),
                wall_time,
                output: output_str,
                original_token_count: collected.total_bytes() / 4,
                omitted_bytes: collected.omitted_bytes(),
            });
        }

        // Process is alive: store in manager
        let process_id = self.allocate_process_id().await;
        let transcript = Arc::new(Mutex::new(HeadTailBuffer::new(
            UNIFIED_EXEC_OUTPUT_MAX_BYTES,
        )));

        {
            let mut store = self.store.lock().await;
            Self::prune_processes_if_needed(&mut store).await;
            store.processes.insert(
                process_id,
                ProcessEntry {
                    process: process.clone(),
                    session_id: spec.session_id,
                    run_id: spec.run_id,
                    command: cmd_display,
                    cwd: spec.cwd,
                    transcript: transcript.clone(),
                    last_used: Instant::now(),
                },
            );
        }

        // Collect output until deadline
        let deadline = start + yield_time;
        let collected = process
            .collect_output_until_deadline(deadline, max_bytes)
            .await;
        let wall_time = start.elapsed();

        transcript.lock().await.push_buffer(collected.clone());

        if process.has_exited() {
            // Exited during the initial yield window: remove from store
            let mut store = self.store.lock().await;
            store.processes.remove(&process_id);
            store.reserved_ids.remove(&process_id);

            let exit_code = process.exit_code().unwrap_or(0);
            let raw_bytes = collected.to_bytes_with_omission_marker();
            let output_str = String::from_utf8_lossy(&raw_bytes).to_string();

            Ok(ExecOutput {
                process_id: None,
                exit_code: Some(exit_code),
                wall_time,
                output: output_str,
                original_token_count: collected.total_bytes() / 4,
                omitted_bytes: collected.omitted_bytes(),
            })
        } else {
            let raw_bytes = collected.to_bytes_with_omission_marker();
            let output_str = String::from_utf8_lossy(&raw_bytes).to_string();

            Ok(ExecOutput {
                process_id: Some(process_id),
                exit_code: None,
                wall_time,
                output: output_str,
                original_token_count: collected.total_bytes() / 4,
                omitted_bytes: collected.omitted_bytes(),
            })
        }
    }

    /// Write input to an open process (or poll if input is empty) and collect output.
    pub async fn write_stdin(
        &self,
        session: SessionId,
        process_id: i32,
        input: &str,
        read: ReadBudget,
    ) -> Result<ExecOutput, UnifiedExecError> {
        let (process, transcript) = {
            let mut store = self.store.lock().await;
            let entry = store
                .processes
                .get_mut(&process_id)
                .ok_or(UnifiedExecError::UnknownProcessId { process_id })?;
            if entry.session_id != session {
                return Err(UnifiedExecError::UnknownProcessId { process_id });
            }
            entry.last_used = Instant::now();
            (entry.process.clone(), entry.transcript.clone())
        };

        let start = Instant::now();
        let interaction_lock = process.interaction_lock();
        let _guard = interaction_lock.lock().await;

        if !input.is_empty() {
            if input == "\u{0003}" {
                process.kill();
            } else {
                process.write(input.as_bytes()).await?;
            }
        }

        let yield_time = clamp_yield_time(
            Some(read.yield_time_ms),
            input.is_empty(),
            self.max_poll_yield_time_ms,
        );
        let max_bytes = resolve_max_tokens(Some(read.max_output_tokens)) * 4;
        let deadline = start + yield_time;

        let collected = process
            .collect_output_until_deadline(deadline, max_bytes)
            .await;
        let wall_time = start.elapsed();

        transcript.lock().await.push_buffer(collected.clone());

        if process.has_exited() {
            let mut store = self.store.lock().await;
            store.processes.remove(&process_id);
            store.reserved_ids.remove(&process_id);

            let exit_code = process.exit_code().unwrap_or(0);
            let raw_bytes = collected.to_bytes_with_omission_marker();
            let output_str = String::from_utf8_lossy(&raw_bytes).to_string();

            Ok(ExecOutput {
                process_id: None,
                exit_code: Some(exit_code),
                wall_time,
                output: output_str,
                original_token_count: collected.total_bytes() / 4,
                omitted_bytes: collected.omitted_bytes(),
            })
        } else {
            let raw_bytes = collected.to_bytes_with_omission_marker();
            let output_str = String::from_utf8_lossy(&raw_bytes).to_string();

            Ok(ExecOutput {
                process_id: Some(process_id),
                exit_code: None,
                wall_time,
                output: output_str,
                original_token_count: collected.total_bytes() / 4,
                omitted_bytes: collected.omitted_bytes(),
            })
        }
    }

    /// Terminate a specific process by id.
    pub async fn terminate_process(&self, session: SessionId, process_id: i32) -> bool {
        let mut store = self.store.lock().await;
        if let Some(entry) = store.processes.get(&process_id) {
            if entry.session_id == session {
                if let Some(removed) = store.processes.remove(&process_id) {
                    store.reserved_ids.remove(&process_id);
                    removed.process.kill();
                    return true;
                }
            }
        }
        false
    }

    /// Kill every process whose cwd is under `root` (worktree release hook).
    pub async fn terminate_under(&self, root: &Path) {
        let mut store = self.store.lock().await;
        let to_remove: Vec<i32> = store
            .processes
            .iter()
            .filter(|(_, entry)| entry.cwd.starts_with(root))
            .map(|(&id, _)| id)
            .collect();

        for id in to_remove {
            if let Some(removed) = store.processes.remove(&id) {
                store.reserved_ids.remove(&id);
                removed.process.kill();
            }
        }
    }

    /// Kill every process owned by `session` (session-close hook).
    pub async fn terminate_session(&self, session: SessionId) {
        let mut store = self.store.lock().await;
        let to_remove: Vec<i32> = store
            .processes
            .iter()
            .filter(|(_, entry)| entry.session_id == session)
            .map(|(&id, _)| id)
            .collect();

        for id in to_remove {
            if let Some(removed) = store.processes.remove(&id) {
                store.reserved_ids.remove(&id);
                removed.process.kill();
            }
        }
    }

    /// Kill all open processes.
    pub async fn terminate_all(&self) {
        let mut store = self.store.lock().await;
        for (_, entry) in store.processes.drain() {
            entry.process.kill();
        }
        store.reserved_ids.clear();
    }

    /// List active processes owned by `session`.
    pub async fn list(&self, session: SessionId) -> Vec<ProcessInfo> {
        let store = self.store.lock().await;
        store
            .processes
            .iter()
            .filter(|(_, entry)| entry.session_id == session)
            .map(|(&id, entry)| ProcessInfo {
                process_id: id,
                session_id: entry.session_id,
                run_id: entry.run_id,
                command: entry.command.clone(),
                cwd: entry.cwd.clone(),
                running: !entry.process.has_exited(),
                last_used: chrono::Utc::now(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn manager_allocates_and_releases_ids() {
        let mgr = UnifiedExecManager::new();
        mgr.set_deterministic_process_ids_for_tests(true);

        let id1 = mgr.allocate_process_id().await;
        let id2 = mgr.allocate_process_id().await;
        assert_eq!(id1, 1000);
        assert_eq!(id2, 1001);

        mgr.release_process_id(id1).await;
        mgr.release_process_id(id2).await;
    }

    /// A process that finishes on its own yields its output and NO handle.
    ///
    /// Note what this does and does not pin. Two code paths satisfy it: the
    /// early-exit branch, and the store-then-release branch that removes the
    /// entry when `has_exited()` is true after the yield window. Forcing the
    /// first branch off does not fail this test, because the second one
    /// upholds the same contract deliberately — which is the contract callers
    /// actually depend on, and the reason it is asserted here rather than the
    /// branch.
    ///
    /// The grace period is widened because the failure mode otherwise is a
    /// fact about the host: under a full `cargo test --workspace` neither
    /// window is reliably long enough for `/bin/echo` to fork, run and exit,
    /// and the test then reports a stored process it never meant to measure.
    #[tokio::test]
    async fn a_process_that_finishes_immediately_yields_output_and_no_handle() {
        let mgr = UnifiedExecManager::new();
        mgr.set_deterministic_process_ids_for_tests(true);
        mgr.set_early_exit_grace_for_tests(30_000);

        let spec = OpenProcessSpec {
            session_id: SessionId::new(),
            run_id: RunId::new(),
            program: PathBuf::from("/bin/echo"),
            args: vec!["hello".to_string()],
            cwd: PathBuf::from("."),
            environment: Vec::new(),
        };

        let out = mgr.exec(spec, ReadBudget::default()).await.unwrap();
        assert!(out.process_id.is_none());
        assert_eq!(out.exit_code, Some(0));
        assert!(out.output.contains("hello"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn interactive_process_lifecycle_and_session_isolation() {
        let mgr = UnifiedExecManager::new();
        mgr.set_deterministic_process_ids_for_tests(true);

        let session1 = SessionId::new();
        let session2 = SessionId::new();
        let run_id = RunId::new();

        let spec = OpenProcessSpec {
            session_id: session1,
            run_id,
            program: PathBuf::from("/bin/cat"),
            args: Vec::new(),
            cwd: PathBuf::from("."),
            environment: Vec::new(),
        };

        let out = mgr
            .exec(
                spec,
                ReadBudget {
                    yield_time_ms: 250,
                    max_output_tokens: 1000,
                },
            )
            .await
            .unwrap();
        let pid = out.process_id.expect("cat should remain running");

        // Cross-session write should fail with UnknownProcessId
        let err = mgr
            .write_stdin(
                session2,
                pid,
                "hi\n",
                ReadBudget {
                    yield_time_ms: 250,
                    max_output_tokens: 1000,
                },
            )
            .await
            .unwrap_err();
        match err {
            UnifiedExecError::UnknownProcessId { process_id } => assert_eq!(process_id, pid),
            _ => panic!("expected UnknownProcessId"),
        }

        // Same-session write should succeed and echo
        let out2 = mgr
            .write_stdin(
                session1,
                pid,
                "hello cat\n",
                ReadBudget {
                    yield_time_ms: 300,
                    max_output_tokens: 1000,
                },
            )
            .await
            .unwrap();
        assert!(out2.output.contains("hello cat"));

        // Terminate
        assert!(mgr.terminate_process(session1, pid).await);
    }
}
