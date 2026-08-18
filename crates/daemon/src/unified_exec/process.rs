use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch, Mutex, Notify};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::head_tail_buffer::HeadTailBuffer;
use super::process_state::ProcessState;
use super::{
    OpenProcessSpec, POST_EXIT_CLOSE_WAIT_CAP_MS, UNIFIED_EXEC_ENV, UNIFIED_EXEC_OUTPUT_MAX_BYTES,
};
use crate::unified_exec::UnifiedExecError;

#[derive(Clone)]
pub struct OutputHandles {
    pub output_buffer: Arc<Mutex<HeadTailBuffer>>,
    pub output_notify: Arc<Notify>,
    pub output_closed: Arc<AtomicBool>,
    pub output_closed_notify: Arc<Notify>,
    pub exit_token: CancellationToken,
}

pub struct UnifiedExecProcess {
    killer: Arc<std::sync::Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
    /// Whether `pid` is still *ours*: true until the exit watcher reaps the
    /// child. Reaping releases the pid, and the pid is the pgid [`Self::kill`]
    /// sweeps — see that method.
    pid_owned: Arc<std::sync::Mutex<bool>>,
    writer_tx: mpsc::Sender<Vec<u8>>,
    output: OutputHandles,
    _state_tx: watch::Sender<ProcessState>,
    state_rx: watch::Receiver<ProcessState>,
    interaction_lock: Arc<Mutex<()>>,
    pid: Option<u32>,
}

impl UnifiedExecProcess {
    /// Spawn an interactive process attached to a native PTY.
    pub fn spawn(spec: &OpenProcessSpec) -> Result<Self, UnifiedExecError> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(portable_pty::PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| UnifiedExecError::CreateProcess {
                message: format!("could not open PTY: {e}"),
            })?;

        let mut cmd = portable_pty::CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        cmd.cwd(&spec.cwd);

        // Environment: clear and apply unified exec env overlaid by spec bindings
        cmd.env_clear();
        for (k, v) in UNIFIED_EXEC_ENV {
            cmd.env(k, v);
        }
        for (k, v) in &spec.environment {
            cmd.env(k, v);
        }

        let mut child =
            pair.slave
                .spawn_command(cmd)
                .map_err(|e| UnifiedExecError::CreateProcess {
                    message: format!("could not spawn process on PTY: {e}"),
                })?;

        let pid = child.process_id();
        let killer = Arc::new(std::sync::Mutex::new(child.clone_killer()));
        let pid_owned = Arc::new(std::sync::Mutex::new(true));

        let output_buffer = Arc::new(Mutex::new(HeadTailBuffer::new(
            UNIFIED_EXEC_OUTPUT_MAX_BYTES,
        )));
        let output_notify = Arc::new(Notify::new());
        let output_closed = Arc::new(AtomicBool::new(false));
        let output_closed_notify = Arc::new(Notify::new());
        let exit_token = CancellationToken::new();

        let output = OutputHandles {
            output_buffer: output_buffer.clone(),
            output_notify: output_notify.clone(),
            output_closed: output_closed.clone(),
            output_closed_notify: output_closed_notify.clone(),
            exit_token: exit_token.clone(),
        };

        // Master Reader Thread
        let mut reader =
            pair.master
                .try_clone_reader()
                .map_err(|e| UnifiedExecError::CreateProcess {
                    message: format!("could not clone PTY master reader: {e}"),
                })?;
        let reader_buf = output_buffer.clone();
        let reader_notify = output_notify.clone();
        let reader_closed = output_closed.clone();
        let reader_closed_notify = output_closed_notify.clone();

        std::thread::Builder::new()
            .name("unified-exec-reader".to_string())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match std::io::Read::read(&mut reader, &mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let chunk = &buf[..n];
                            // This closure runs on a dedicated `std::thread`, never
                            // a tokio worker, so `blocking_lock` is safe here and —
                            // unlike the old `try_lock` — NEVER drops a chunk under
                            // contention: it blocks the reader thread until the
                            // buffer lock is free. The former
                            // `Handle::try_current()` branch was dead (a std thread
                            // is not a tokio worker, so `try_current` always errs),
                            // which meant every chunk went through the lossy
                            // `try_lock` and was silently dropped whenever a reader
                            // held the lock.
                            let mut g = reader_buf.blocking_lock();
                            g.push_chunk(chunk);
                            drop(g);
                            reader_notify.notify_waiters();
                        }
                        Err(_) => break,
                    }
                }
                reader_closed.store(true, Ordering::Release);
                reader_closed_notify.notify_waiters();
            })
            .map_err(|e| UnifiedExecError::CreateProcess {
                message: format!("could not spawn PTY reader thread: {e}"),
            })?;

        // Stdin Writer Task
        let (writer_tx, mut writer_rx) = mpsc::channel::<Vec<u8>>(128);
        let mut writer =
            pair.master
                .take_writer()
                .map_err(|e| UnifiedExecError::CreateProcess {
                    message: format!("could not take PTY master writer: {e}"),
                })?;

        std::thread::Builder::new()
            .name("unified-exec-writer".to_string())
            .spawn(move || {
                while let Some(bytes) = writer_rx.blocking_recv() {
                    if std::io::Write::write_all(&mut writer, &bytes).is_err() {
                        break;
                    }
                    let _ = std::io::Write::flush(&mut writer);
                }
            })
            .map_err(|e| UnifiedExecError::CreateProcess {
                message: format!("could not spawn PTY writer thread: {e}"),
            })?;

        // Exit Watcher Thread
        let (state_tx, state_rx) = watch::channel(ProcessState::default());
        let exit_state_tx = state_tx.clone();
        let exit_tok = exit_token.clone();
        let exit_not = output_notify.clone();
        let exit_closed_not = output_closed_notify.clone();

        let watch_pid = pid;
        let watch_owned = pid_owned.clone();

        std::thread::Builder::new()
            .name("unified-exec-exit-watcher".to_string())
            .spawn(move || {
                // Reaping releases the pid, and that pid is the pgid `kill`
                // sweeps: a sweep issued after the reap can SIGKILL whatever
                // unrelated process group the kernel has since handed the
                // recycled pid to. So: block until the child terminates
                // *without* reaping it (`waitid(WNOWAIT)` — a real wait, not a
                // poll), then reap while holding `pid_owned`, the same lock
                // `kill` takes. No sweep can straddle the reap.
                #[cfg(unix)]
                let observed = watch_pid
                    .is_some_and(codypendent_sandbox::executor::await_child_terminated_unreaped);
                #[cfg(not(unix))]
                let observed = {
                    let _ = watch_pid;
                    false
                };
                let wait_res = if observed {
                    let mut owned = watch_owned
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    // The child is already a zombie, so this returns at once —
                    // the lock is never held for the process's lifetime.
                    let res = child.wait();
                    *owned = false;
                    res
                } else {
                    // Termination could not be observed without reaping (no pid,
                    // or a platform without `waitid`). Fall back to the blocking
                    // reap, deliberately *not* under the lock: holding it here
                    // would block `kill` for as long as the process lives. On
                    // that path there is no pid to sweep a group with anyway.
                    let res = child.wait();
                    *watch_owned
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
                    res
                };
                let exit_code = match wait_res {
                    Ok(status) => {
                        if status.success() {
                            Some(0)
                        } else {
                            Some(status.exit_code() as i32)
                        }
                    }
                    Err(e) => {
                        exit_state_tx.send_replace(ProcessState::default().failed(e.to_string()));
                        None
                    }
                };
                if let Some(code) = exit_code {
                    exit_state_tx.send_replace(ProcessState::default().exited(Some(code)));
                }
                exit_tok.cancel();
                exit_not.notify_waiters();
                exit_closed_not.notify_waiters();
            })
            .map_err(|e| UnifiedExecError::CreateProcess {
                message: format!("could not spawn PTY exit watcher: {e}"),
            })?;

        Ok(Self {
            killer,
            pid_owned,
            writer_tx,
            output,
            _state_tx: state_tx,
            state_rx,
            interaction_lock: Arc::new(Mutex::new(())),
            pid,
        })
    }

    /// Interaction lock to serialize operations per process.
    pub fn interaction_lock(&self) -> Arc<Mutex<()>> {
        self.interaction_lock.clone()
    }

    pub fn has_exited(&self) -> bool {
        self.state_rx.borrow().has_exited
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.state_rx.borrow().exit_code
    }

    pub fn failure_message(&self) -> Option<String> {
        self.state_rx.borrow().failure_message.clone()
    }

    pub fn output_handles(&self) -> OutputHandles {
        self.output.clone()
    }

    /// Write raw bytes to stdin.
    pub async fn write(&self, data: &[u8]) -> Result<(), UnifiedExecError> {
        if self.has_exited() {
            return Err(UnifiedExecError::StdinClosed);
        }
        self.writer_tx
            .send(data.to_vec())
            .await
            .map_err(|_| UnifiedExecError::StdinClosed)
    }

    /// Kill the process and its process group.
    ///
    /// Both signals go out under `pid_owned`, which the exit watcher holds while
    /// it reaps: once the child has been reaped this is a no-op by design. The
    /// pid is the pgid, a reaped pid can be recycled, and the previous shape —
    /// signalling unconditionally, after the watcher may already have reaped —
    /// could SIGKILL an unrelated process group.
    ///
    /// The group goes first so a shell cannot outlive its own children, and it
    /// goes out as a `kill(-pgid, SIGKILL)` syscall rather than `/bin/kill`,
    /// which minimal images do not ship (there the sweep silently did nothing
    /// and every grandchild leaked).
    pub fn kill(&self) {
        let owned = self
            .pid_owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*owned {
            return;
        }
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            codypendent_sandbox::executor::kill_process_group(pid);
        }
        if let Ok(mut k) = self.killer.lock() {
            let _ = k.kill();
        }
    }

    /// Collect output until the deadline expires or the process exits and closes.
    pub async fn collect_output_until_deadline(
        &self,
        deadline: Instant,
        max_bytes: usize,
    ) -> HeadTailBuffer {
        let mut collected = HeadTailBuffer::new(max_bytes);

        loop {
            // Drain anything in buffer
            let drained = self.output.output_buffer.lock().await.drain();
            if !drained.is_empty() {
                collected.push_buffer(drained);
            }

            if self.output.exit_token.is_cancelled()
                && self.output.output_closed.load(Ordering::Acquire)
            {
                // Final drain
                let final_drained = self.output.output_buffer.lock().await.drain();
                if !final_drained.is_empty() {
                    collected.push_buffer(final_drained);
                }
                break;
            }

            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline.saturating_duration_since(now);

            tokio::select! {
                _ = self.output.output_notify.notified() => {}
                _ = self.output.exit_token.cancelled() => {
                    // Wait at most POST_EXIT_CLOSE_WAIT_CAP_MS for reader to close
                    let _ = tokio::time::timeout(
                        Duration::from_millis(POST_EXIT_CLOSE_WAIT_CAP_MS),
                        self.output.output_closed_notify.notified()
                    ).await;
                }
                _ = tokio::time::sleep(remaining) => {}
            }
        }

        collected
    }
}

impl Drop for UnifiedExecProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_protocol::{RunId, SessionId};
    use std::path::PathBuf;

    /// A large, multi-chunk PTY output must arrive in the buffer *completely* —
    /// no chunk silently dropped. The reader thread reads in 8 KiB slices, so
    /// 200 000 bytes forces ~25 reads; the old `try_lock` fallback dropped any
    /// slice it could not immediately lock, so this count came up short under
    /// contention. `blocking_lock` never drops one.
    #[tokio::test]
    async fn large_multi_chunk_output_is_never_dropped() {
        const EXPECTED: usize = 200_000;
        let spec = OpenProcessSpec {
            session_id: SessionId::new(),
            run_id: RunId::new(),
            program: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".to_string(),
                format!("yes A | head -n {EXPECTED} | tr -d '\\n'"),
            ],
            cwd: std::env::temp_dir(),
            // The unified-exec env is cleared, so hand the pipeline a PATH to
            // find `yes`/`head`/`tr`.
            environment: vec![("PATH".to_string(), "/usr/bin:/bin".to_string())],
        };

        let process = UnifiedExecProcess::spawn(&spec).expect("spawn PTY process");
        let deadline = Instant::now() + Duration::from_secs(30);
        let collected = process
            .collect_output_until_deadline(deadline, UNIFIED_EXEC_OUTPUT_MAX_BYTES)
            .await;

        // Output (200 KB) is well under the 1 MiB cap, so nothing is legitimately
        // omitted — every 'A' the child wrote must be present.
        assert_eq!(collected.omitted_bytes(), 0, "no bytes should be omitted");
        let a_count = collected.to_bytes().iter().filter(|&&b| b == b'A').count();
        assert_eq!(
            a_count, EXPECTED,
            "every emitted byte must reach the buffer"
        );
    }
}
