//! Hook dispatch engine (adoption 08).
//!
//! Loads approved hooks for an event in total dispatch order `(priority, id)`,
//! executes them sequentially through [`HookRunner`], combines their verdicts
//! under the lattice, audits each invocation in `hook_dispatches`, and reports
//! rewrite outcomes.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use codypendent_protocol::{RunId, SessionId};
use codypendent_sandbox::executor::SandboxExecutor;
use codypendent_sandbox::hook::{combine, HookEvent, HookOutcome, HookSpec, ToolCall};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::hook_exec::{
    DispatchAudit, HookPayload, HookPayloadOutcome, HookPayloadTool, HookRunContextPaths,
    HookRunner,
};
use crate::hooks::HookRecord;

/// Everything the engine needs to dispatch one event for one run.
#[derive(Debug, Clone)]
pub struct HookRunMeta {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub repository: PathBuf,
    pub worktree: PathBuf,
}

/// An APPROVED hook row whose stored `spec_json` could not be parsed.
///
/// Neither of the two obvious answers is acceptable. Silently skipping it is
/// fail-open: an approved deny hook vanishes and the call it was written to
/// refuse goes through unremarked. Propagating an error is fail-shut past the
/// point of usefulness: `agent.rs` awaits `tool_pre(..)?`, so one corrupt row —
/// or a spec written by a newer build and read back after a downgrade — makes
/// EVERY tool call for that event, in every run in that scope, fail
/// permanently, with no in-product recovery (the row is only fixable by editing
/// the database).
///
/// The row is **quarantined** instead: excluded from dispatch so it contributes
/// no verdict, recorded in `hook_dispatches` as `verdict = 'error'` /
/// `applied = 'quarantined'`, and logged at ERROR naming the hook and the
/// remedy. Other hooks' verdicts and the tool call itself proceed, and the
/// operator has a durable, legible trail pointing at the one hook to re-approve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinedHook {
    /// The `hooks.id` row key (the audit row's foreign key).
    pub row_id: String,
    /// The hook's stable `hook_id`, as the operator names it.
    pub hook_id: String,
    /// The hook's human name.
    pub name: String,
    /// Why the spec could not be parsed.
    pub reason: String,
}

impl QuarantinedHook {
    /// The operator-facing diagnostic: which hook, why, and how to fix it.
    #[must_use]
    pub fn notice(&self) -> String {
        format!(
            "hook '{}' ({}) is approved but its stored spec cannot be parsed: {}. \
             It was quarantined for this dispatch — it did not run and contributed no \
             verdict, while every other hook and the tool call itself proceeded. \
             Re-scan the hook source and re-approve it (`codypendent hook approve {}`) \
             to put it back in service.",
            self.hook_id, self.row_id, self.reason, self.hook_id
        )
    }
}

/// What one `approved_hooks` load yielded: the hooks that will actually
/// dispatch, and the approved-but-unparseable rows that were quarantined.
struct LoadedHooks {
    dispatchable: Vec<(HookRecord, HookSpec)>,
    quarantined: Vec<QuarantinedHook>,
}

/// The hook dispatch engine.
pub struct HookEngine {
    pool: SqlitePool,
    runner: HookRunner,
}

impl HookEngine {
    /// Create a new hook engine with database pool and sandbox executor.
    #[must_use]
    pub fn new(pool: SqlitePool, executor: Arc<dyn SandboxExecutor>) -> Self {
        Self {
            pool,
            runner: HookRunner::new(executor),
        }
    }

    /// Approved hooks for `event`, in total dispatch order.
    /// Filters repository hooks to `scope_key = meta.repository` so checkouts stay isolated.
    ///
    /// A row whose `spec_json` cannot be parsed is not silently dropped and does
    /// not fail the load: it is returned separately as a [`QuarantinedHook`] —
    /// see that type for why neither extreme is acceptable.
    async fn approved_hooks(
        &self,
        event: HookEvent,
        meta: &HookRunMeta,
    ) -> anyhow::Result<LoadedHooks> {
        let repo_key = meta.repository.to_string_lossy().to_string();
        let rows = sqlx::query_as::<_, HookRecord>(
            "SELECT id, registry_item_id, hook_id, name, scope_kind, scope_key, event, kind, priority, \
             source_path, content_hash, spec_json, approval_state, approved_content_hash, approved_by, \
             approved_at, created_at, updated_at FROM hooks \
             WHERE approval_state = 'approved' AND approved_content_hash = content_hash \
               AND event = ? AND scope_kind IN ('user', 'repository') \
               AND (scope_kind = 'user' OR scope_key = ?) \
             ORDER BY priority, id",
        )
        .bind(event.as_str())
        .bind(&repo_key)
        .fetch_all(&self.pool)
        .await?;

        let mut dispatchable = Vec::new();
        let mut quarantined = Vec::new();
        for row in rows {
            match serde_json::from_str::<HookSpec>(&row.spec_json) {
                Ok(spec) => dispatchable.push((row, spec)),
                Err(err) => quarantined.push(QuarantinedHook {
                    row_id: row.id,
                    hook_id: row.hook_id,
                    name: row.name,
                    reason: err.to_string(),
                }),
            }
        }
        dispatchable.sort_by(|a, b| a.1.dispatch_key().cmp(&b.1.dispatch_key()));
        quarantined.sort_by(|a, b| a.row_id.cmp(&b.row_id));
        Ok(LoadedHooks {
            dispatchable,
            quarantined,
        })
    }

    /// Surface every quarantined hook: one ERROR log naming the hook and the
    /// remedy, and one `hook_dispatches` row so the failure is durable rather
    /// than living only in a log the operator may never read. Recording is
    /// best-effort — an audit write failure must not turn a survivable corrupt
    /// row back into a dispatch failure.
    async fn record_quarantined(
        &self,
        quarantined: &[QuarantinedHook],
        event: HookEvent,
        subject_digest: &str,
        meta: &HookRunMeta,
    ) {
        for hook in quarantined {
            tracing::error!(
                hook_id = %hook.hook_id,
                hook_row_id = %hook.row_id,
                hook_name = %hook.name,
                event = event.as_str(),
                run_id = %meta.run_id,
                "{}",
                hook.notice()
            );
            let audit = DispatchAudit {
                hook_row_id: hook.row_id.clone(),
                run_id: Some(meta.run_id.to_string()),
                event: event.as_str().to_string(),
                subject_digest: subject_digest.to_string(),
                verdict: "error".to_string(),
                applied: "quarantined".to_string(),
                rewrote_action: None,
                exit_status: None,
                timed_out: false,
                duration_ms: 0,
                output_bytes: 0,
                error: Some(hook.notice()),
            };
            if let Err(err) = self.record_audit(&audit).await {
                tracing::warn!("failed to record quarantined hook audit: {err}");
            }
        }
    }

    /// Record a hook dispatch in the `hook_dispatches` audit table.
    async fn record_audit(&self, audit: &DispatchAudit) -> anyhow::Result<()> {
        let id = Uuid::now_v7().to_string();
        let now = Utc::now().to_rfc3339();
        let timed_out_val: i64 = if audit.timed_out { 1 } else { 0 };

        sqlx::query(
            r#"
INSERT INTO hook_dispatches (
    id, hook_row_id, run_id, event, subject_digest, verdict, applied,
    rewrote_action, exit_status, timed_out, duration_ms, output_bytes, error, created_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
"#,
        )
        .bind(&id)
        .bind(&audit.hook_row_id)
        .bind(&audit.run_id)
        .bind(&audit.event)
        .bind(&audit.subject_digest)
        .bind(&audit.verdict)
        .bind(&audit.applied)
        .bind(&audit.rewrote_action)
        .bind(audit.exit_status.map(i64::from))
        .bind(timed_out_val)
        .bind(audit.duration_ms)
        .bind(audit.output_bytes)
        .bind(&audit.error)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Sequential dispatch of approved hooks for `tool.pre`, combining verdicts.
    pub async fn dispatch_tool_pre(
        &self,
        meta: &HookRunMeta,
        call: &ToolCall,
    ) -> anyhow::Result<HookOutcome> {
        let loaded = self.approved_hooks(HookEvent::ToolPre, meta).await?;
        let subject_digest = call.digest();
        self.record_quarantined(
            &loaded.quarantined,
            HookEvent::ToolPre,
            &subject_digest,
            meta,
        )
        .await;
        if loaded.dispatchable.is_empty() {
            return Ok(HookOutcome::Proceed);
        }

        let triggered_at = Utc::now().to_rfc3339();
        let mut verdicts = Vec::new();

        for (record, spec) in loaded.dispatchable {
            let hook_dir = PathBuf::from(&record.source_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new(""))
                .to_path_buf();
            let ctx = HookRunContextPaths {
                repository: meta.repository.clone(),
                worktree: meta.worktree.clone(),
                hook_dir,
            };
            let payload = HookPayload {
                payload_version: 1,
                event: "tool.pre",
                hook_id: &spec.id,
                session_id: meta.session_id.to_string(),
                run_id: meta.run_id.to_string(),
                repository: meta.repository.to_string_lossy().to_string(),
                worktree: meta.worktree.to_string_lossy().to_string(),
                triggered_at: triggered_at.clone(),
                tool: Some(HookPayloadTool {
                    name: &call.name,
                    arguments_json: &call.arguments_json,
                }),
                outcome: None,
            };

            let (verdict, audit) =
                self.runner
                    .run_hook(&record.id, &spec, &payload, &ctx, &subject_digest);

            if let Err(err) = self.record_audit(&audit).await {
                tracing::warn!("failed to record hook dispatch audit: {err}");
            }

            verdicts.push((spec.id.clone(), verdict));
        }

        Ok(combine(verdicts))
    }

    /// Dispatch approved hooks for `tool.post` (observation only, errors swallowed).
    pub async fn dispatch_tool_post(
        &self,
        meta: &HookRunMeta,
        call: &ToolCall,
        outcome: &HookPayloadOutcome,
    ) -> anyhow::Result<()> {
        let loaded = self.approved_hooks(HookEvent::ToolPost, meta).await?;
        let subject_digest = call.digest();
        self.record_quarantined(
            &loaded.quarantined,
            HookEvent::ToolPost,
            &subject_digest,
            meta,
        )
        .await;
        if loaded.dispatchable.is_empty() {
            return Ok(());
        }

        let triggered_at = Utc::now().to_rfc3339();

        for (record, spec) in loaded.dispatchable {
            let hook_dir = PathBuf::from(&record.source_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new(""))
                .to_path_buf();
            let ctx = HookRunContextPaths {
                repository: meta.repository.clone(),
                worktree: meta.worktree.clone(),
                hook_dir,
            };
            let payload = HookPayload {
                payload_version: 1,
                event: "tool.post",
                hook_id: &spec.id,
                session_id: meta.session_id.to_string(),
                run_id: meta.run_id.to_string(),
                repository: meta.repository.to_string_lossy().to_string(),
                worktree: meta.worktree.to_string_lossy().to_string(),
                triggered_at: triggered_at.clone(),
                tool: Some(HookPayloadTool {
                    name: &call.name,
                    arguments_json: &call.arguments_json,
                }),
                outcome: Some(outcome.clone()),
            };

            let (_verdict, audit) =
                self.runner
                    .run_hook(&record.id, &spec, &payload, &ctx, &subject_digest);

            if let Err(err) = self.record_audit(&audit).await {
                tracing::warn!("failed to record hook dispatch audit: {err}");
            }
        }

        Ok(())
    }

    /// Dispatch approved hooks for `run.start` or `run.end` (observation only).
    pub async fn dispatch_run_event(
        &self,
        meta: &HookRunMeta,
        event: HookEvent,
    ) -> anyhow::Result<()> {
        let loaded = self.approved_hooks(event, meta).await?;
        let subject_digest = meta.run_id.to_string();
        self.record_quarantined(&loaded.quarantined, event, &subject_digest, meta)
            .await;
        if loaded.dispatchable.is_empty() {
            return Ok(());
        }

        let triggered_at = Utc::now().to_rfc3339();

        for (record, spec) in loaded.dispatchable {
            let hook_dir = PathBuf::from(&record.source_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new(""))
                .to_path_buf();
            let ctx = HookRunContextPaths {
                repository: meta.repository.clone(),
                worktree: meta.worktree.clone(),
                hook_dir,
            };
            let payload = HookPayload {
                payload_version: 1,
                event: event.as_str(),
                hook_id: &spec.id,
                session_id: meta.session_id.to_string(),
                run_id: meta.run_id.to_string(),
                repository: meta.repository.to_string_lossy().to_string(),
                worktree: meta.worktree.to_string_lossy().to_string(),
                triggered_at: triggered_at.clone(),
                tool: None,
                outcome: None,
            };

            let (_verdict, audit) =
                self.runner
                    .run_hook(&record.id, &spec, &payload, &ctx, &subject_digest);

            if let Err(err) = self.record_audit(&audit).await {
                tracing::warn!("failed to record hook dispatch audit: {err}");
            }
        }

        Ok(())
    }

    /// Stamp how a rewrite ended (e.g. `rewrite-reentered` or `rewrite-refused`) onto the dispatch row.
    pub async fn report_rewrite(
        &self,
        run_id: RunId,
        subject_digest: &str,
        applied: &str,
    ) -> anyhow::Result<()> {
        let run_id_str = run_id.to_string();
        sqlx::query(
            "UPDATE hook_dispatches SET applied = ? \
             WHERE run_id = ? AND subject_digest = ? AND verdict = 'rewrite'",
        )
        .bind(applied)
        .bind(&run_id_str)
        .bind(subject_digest)
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}

/// The hook dispatch trait consumed by the agent runtime.
#[async_trait::async_trait]
pub trait HookDispatch: Send + Sync {
    /// Dispatch tool.pre hooks for a model-proposed tool call.
    async fn tool_pre(&self, meta: &HookRunMeta, call: &ToolCall) -> anyhow::Result<HookOutcome>;
    /// Dispatch tool.post observation hooks after tool execution.
    async fn tool_post(
        &self,
        meta: &HookRunMeta,
        call: &ToolCall,
        success: bool,
        message: Option<&str>,
        duration_ms: u64,
    ) -> anyhow::Result<()>;
    /// Dispatch run.start (start=true) or run.end (start=false) lifecycle hooks.
    async fn run_event(&self, meta: &HookRunMeta, start: bool) -> anyhow::Result<()>;
    /// Record the final outcome of a rewrite (rewrite-reentered or rewrite-refused).
    async fn report_rewrite(
        &self,
        run_id: RunId,
        subject_digest: &str,
        applied: &str,
    ) -> anyhow::Result<()>;
}

#[async_trait::async_trait]
impl HookDispatch for HookEngine {
    async fn tool_pre(&self, meta: &HookRunMeta, call: &ToolCall) -> anyhow::Result<HookOutcome> {
        self.dispatch_tool_pre(meta, call).await
    }

    async fn tool_post(
        &self,
        meta: &HookRunMeta,
        call: &ToolCall,
        success: bool,
        message: Option<&str>,
        duration_ms: u64,
    ) -> anyhow::Result<()> {
        let outcome = HookPayloadOutcome {
            success,
            message: message.map(str::to_string),
            duration_ms,
        };
        self.dispatch_tool_post(meta, call, &outcome).await
    }

    async fn run_event(&self, meta: &HookRunMeta, start: bool) -> anyhow::Result<()> {
        let event = if start {
            HookEvent::RunStart
        } else {
            HookEvent::RunEnd
        };
        self.dispatch_run_event(meta, event).await
    }

    async fn report_rewrite(
        &self,
        run_id: RunId,
        subject_digest: &str,
        applied: &str,
    ) -> anyhow::Result<()> {
        self.report_rewrite(run_id, subject_digest, applied).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::{approve_hook, scan_hook_root};
    use codypendent_sandbox::executor::CapabilityReport;
    use codypendent_sandbox::executor::SandboxCommand;
    use codypendent_sandbox::executor::SandboxError;
    use codypendent_sandbox::executor::SandboxOutcome;
    use codypendent_sandbox::hook::HookScope;
    use codypendent_sandbox::profile::SandboxProfile;
    use tempfile::tempdir;

    struct StubSandbox;
    impl SandboxExecutor for StubSandbox {
        fn capability_report(&self) -> CapabilityReport {
            CapabilityReport {
                platform: "stub",
                backend: codypendent_sandbox::executor::SandboxBackend::None,
                available: true,
                enforces_filesystem: true,
                enforces_network: true,
                enforces_clean_env: true,
                enforces_wall_clock: true,
                enforces_output_cap: true,
                enforces_rlimits: true,
                degraded: Vec::new(),
            }
        }

        fn run(
            &self,
            _profile: &SandboxProfile,
            _command: &SandboxCommand,
        ) -> Result<SandboxOutcome, SandboxError> {
            Ok(SandboxOutcome {
                backend: codypendent_sandbox::executor::SandboxBackend::None,
                exit_code: Some(0),
                timed_out: false,
                duration: std::time::Duration::from_millis(10),
                // `sanitize_untrusted` takes (origin, raw): the control line is
                // the RAW stdout, not the origin label. Passing them the other
                // way round made this stub emit the literal text "test", so
                // every hook it ran silently produced no control line at all.
                stdout: codypendent_sandbox::sanitize::sanitize_untrusted(
                    "test",
                    "HOOK_CONTROL\t{\"decision\":\"allow\"}",
                    1024,
                ),
                stderr: codypendent_sandbox::sanitize::sanitize_untrusted("test", "", 1024),
                output_truncated: false,
            })
        }

        fn prepare_interactive(
            &self,
            _profile: &SandboxProfile,
            _command: &SandboxCommand,
        ) -> Result<codypendent_sandbox::executor::SandboxProcessSpec, SandboxError> {
            Err(SandboxError::InvalidCommand("not supported".into()))
        }
    }

    fn make_meta(repo: PathBuf, worktree: PathBuf) -> HookRunMeta {
        HookRunMeta {
            session_id: SessionId::new(),
            run_id: RunId::new(),
            repository: repo,
            worktree,
        }
    }

    #[tokio::test]
    async fn only_approved_hash_matched_hooks_dispatch() {
        let dir = tempdir().unwrap();
        let pool = crate::db::open_database(&dir.path().join("test.db"))
            .await
            .unwrap();
        let engine = HookEngine::new(pool.clone(), Arc::new(StubSandbox));

        let hook_file = dir.path().join("hook.toml");
        std::fs::write(
            &hook_file,
            r#"
schema_version = 1
id = "test.hook"
name = "Test Hook"
scope = "user"
event = "tool.pre"
kind = "validate"
priority = 100

[runtime]
type = "command"
program = "/bin/sh"
args = ["-c", "exit 0"]
timeout_seconds = 30

[policy]
failure = "block"
requires_approval = false
network = "deny"
"#,
        )
        .unwrap();

        scan_hook_root(&pool, dir.path(), HookScope::User, "").await;
        let meta = make_meta(dir.path().to_path_buf(), dir.path().to_path_buf());
        let call = ToolCall {
            name: "shell.run".into(),
            arguments_json: r#"{"command":"cargo test"}"#.into(),
        };

        // Pending hook -> does not dispatch, proceeds
        let outcome = engine.dispatch_tool_pre(&meta, &call).await.unwrap();
        assert_eq!(outcome, HookOutcome::Proceed);

        // Approve hook -> dispatches
        approve_hook(&pool, "test.hook", "operator").await.unwrap();
        let outcome2 = engine.dispatch_tool_pre(&meta, &call).await.unwrap();
        assert_eq!(outcome2, HookOutcome::Proceed); // allow verdict combines to Proceed
    }

    #[tokio::test]
    async fn repository_hooks_filter_on_scope_key() {
        let dir = tempdir().unwrap();
        let pool = crate::db::open_database(&dir.path().join("test.db"))
            .await
            .unwrap();
        let engine = HookEngine::new(pool.clone(), Arc::new(StubSandbox));

        let repo_a = tempdir().unwrap();
        let repo_b = tempdir().unwrap();

        let hook_file = repo_a.path().join("hook.toml");
        std::fs::write(
            &hook_file,
            r#"
schema_version = 1
id = "repo.hook"
name = "Repo Hook"
scope = "repository"
event = "tool.pre"
kind = "validate"
priority = 100

[runtime]
type = "command"
program = "/bin/sh"
args = ["-c", "exit 0"]
timeout_seconds = 30

[policy]
failure = "block"
requires_approval = false
network = "deny"
"#,
        )
        .unwrap();

        let repo_a_key = repo_a.path().to_string_lossy().to_string();
        scan_hook_root(&pool, repo_a.path(), HookScope::Repository, &repo_a_key).await;
        approve_hook(&pool, "repo.hook", "operator").await.unwrap();

        // Querying for repo_b does not dispatch repo_a's hook
        let meta_b = make_meta(repo_b.path().to_path_buf(), repo_b.path().to_path_buf());
        let approved_b = engine
            .approved_hooks(HookEvent::ToolPre, &meta_b)
            .await
            .unwrap();
        assert!(approved_b.dispatchable.is_empty());

        // Querying for repo_a dispatches repo_a's hook
        let meta_a = make_meta(repo_a.path().to_path_buf(), repo_a.path().to_path_buf());
        let approved_a = engine
            .approved_hooks(HookEvent::ToolPre, &meta_a)
            .await
            .unwrap();
        assert_eq!(approved_a.dispatchable.len(), 1);
    }

    #[tokio::test]
    async fn dispatch_rows_are_written_per_invocation_and_report_rewrite() {
        let dir = tempdir().unwrap();
        let pool = crate::db::open_database(&dir.path().join("test.db"))
            .await
            .unwrap();
        let engine = HookEngine::new(pool.clone(), Arc::new(StubSandbox));

        let hook_file = dir.path().join("hook.toml");
        std::fs::write(
            &hook_file,
            r#"
schema_version = 1
id = "test.hook"
name = "Test Hook"
scope = "user"
event = "tool.pre"
kind = "validate"
priority = 100

[runtime]
type = "command"
program = "/bin/sh"
args = ["-c", "exit 0"]
timeout_seconds = 30

[policy]
failure = "block"
requires_approval = false
network = "deny"
"#,
        )
        .unwrap();

        scan_hook_root(&pool, dir.path(), HookScope::User, "").await;
        approve_hook(&pool, "test.hook", "operator").await.unwrap();

        let meta = make_meta(dir.path().to_path_buf(), dir.path().to_path_buf());
        let call = ToolCall {
            name: "shell.run".into(),
            arguments_json: "{}".into(),
        };

        engine.dispatch_tool_pre(&meta, &call).await.unwrap();

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM hook_dispatches")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 1);

        // Report rewrite test
        engine
            .report_rewrite(meta.run_id, &call.digest(), "rewrite-reentered")
            .await
            .unwrap();
    }

    /// A sandbox whose hooks all vote deny — lets a test prove a surviving
    /// hook's verdict still reaches the caller.
    struct DenyingSandbox;
    impl SandboxExecutor for DenyingSandbox {
        fn capability_report(&self) -> CapabilityReport {
            StubSandbox.capability_report()
        }

        fn run(
            &self,
            _profile: &SandboxProfile,
            _command: &SandboxCommand,
        ) -> Result<SandboxOutcome, SandboxError> {
            Ok(SandboxOutcome {
                backend: codypendent_sandbox::executor::SandboxBackend::None,
                exit_code: Some(0),
                timed_out: false,
                duration: std::time::Duration::from_millis(10),
                stdout: codypendent_sandbox::sanitize::sanitize_untrusted(
                    "test",
                    "HOOK_CONTROL\t{\"decision\":\"deny\",\"reason\":\"the surviving hook ran\"}",
                    1024,
                ),
                stderr: codypendent_sandbox::sanitize::sanitize_untrusted("test", "", 1024),
                output_truncated: false,
            })
        }

        fn prepare_interactive(
            &self,
            _profile: &SandboxProfile,
            _command: &SandboxCommand,
        ) -> Result<codypendent_sandbox::executor::SandboxProcessSpec, SandboxError> {
            Err(SandboxError::InvalidCommand("not supported".into()))
        }
    }

    fn hook_toml(id: &str, name: &str, priority: i32) -> String {
        format!(
            r#"
schema_version = 1
id = "{id}"
name = "{name}"
scope = "user"
event = "tool.pre"
kind = "validate"
priority = {priority}

[runtime]
type = "command"
program = "/bin/sh"
args = ["-c", "exit 0"]
timeout_seconds = 30

[policy]
failure = "block"
requires_approval = false
network = "deny"
"#
        )
    }

    /// Two approved hooks, one of whose stored `spec_json` is corrupt (a row
    /// written by a newer build, read back after a downgrade). The corrupt row
    /// must not brick the dispatch: the surviving hook still runs and its deny
    /// verdict still reaches the caller, instead of the whole tool call failing
    /// permanently for everything in this scope.
    #[tokio::test]
    async fn a_corrupt_approved_hook_does_not_brick_unrelated_tool_calls() {
        let dir = tempdir().unwrap();
        let pool = crate::db::open_database(&dir.path().join("test.db"))
            .await
            .unwrap();
        let engine = HookEngine::new(pool.clone(), Arc::new(DenyingSandbox));

        std::fs::write(
            dir.path().join("corrupt.toml"),
            hook_toml("corrupt.hook", "Corrupt Hook", 10),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("healthy.toml"),
            hook_toml("healthy.hook", "Healthy Hook", 20),
        )
        .unwrap();
        scan_hook_root(&pool, dir.path(), HookScope::User, "").await;
        approve_hook(&pool, "corrupt.hook", "operator")
            .await
            .unwrap();
        approve_hook(&pool, "healthy.hook", "operator")
            .await
            .unwrap();

        // Corrupt the stored spec the way only a schema drift or a stray write
        // can: the row stays approved and hash-matched.
        sqlx::query("UPDATE hooks SET spec_json = ? WHERE hook_id = ?")
            .bind("{\"schema_version\":9999,\"not\":\"a spec\"}")
            .bind("corrupt.hook")
            .execute(&pool)
            .await
            .unwrap();

        let meta = make_meta(dir.path().to_path_buf(), dir.path().to_path_buf());
        let call = ToolCall {
            name: "shell.run".into(),
            arguments_json: r#"{"command":"cargo test"}"#.into(),
        };

        let outcome = engine
            .dispatch_tool_pre(&meta, &call)
            .await
            .expect("a corrupt row must not fail the dispatch");
        match outcome {
            HookOutcome::Denied { reasons } => assert!(
                reasons.iter().any(|r| r.contains("healthy.hook")),
                "the surviving hook's verdict still lands: {reasons:?}"
            ),
            other => panic!("expected the surviving hook's deny, got {other:?}"),
        }
    }

    /// …and it is not silently ignored either (the fail-open bug). The corrupt
    /// row leaves a durable `hook_dispatches` diagnostic naming the hook and
    /// how to restore it, and contributes no verdict of its own.
    #[tokio::test]
    async fn a_corrupt_approved_hook_is_quarantined_and_recorded_not_silently_skipped() {
        let dir = tempdir().unwrap();
        let pool = crate::db::open_database(&dir.path().join("test.db"))
            .await
            .unwrap();
        let engine = HookEngine::new(pool.clone(), Arc::new(StubSandbox));

        std::fs::write(
            dir.path().join("corrupt.toml"),
            hook_toml("corrupt.hook", "Corrupt Hook", 10),
        )
        .unwrap();
        scan_hook_root(&pool, dir.path(), HookScope::User, "").await;
        approve_hook(&pool, "corrupt.hook", "operator")
            .await
            .unwrap();
        sqlx::query("UPDATE hooks SET spec_json = ? WHERE hook_id = ?")
            .bind("not json at all")
            .bind("corrupt.hook")
            .execute(&pool)
            .await
            .unwrap();

        let meta = make_meta(dir.path().to_path_buf(), dir.path().to_path_buf());
        let call = ToolCall {
            name: "shell.run".into(),
            arguments_json: "{}".into(),
        };

        // The only hook in scope is corrupt: the call proceeds under the
        // remaining (empty) verdict set rather than failing.
        let outcome = engine.dispatch_tool_pre(&meta, &call).await.unwrap();
        assert_eq!(outcome, HookOutcome::Proceed);

        let (verdict, applied, error): (String, String, Option<String>) =
            sqlx::query_as("SELECT verdict, applied, error FROM hook_dispatches")
                .fetch_one(&pool)
                .await
                .expect("the quarantined hook leaves an audit row");
        assert_eq!(verdict, "error");
        assert_eq!(applied, "quarantined");
        let error = error.expect("the audit row carries the diagnostic");
        assert!(
            error.contains("corrupt.hook") && error.contains("codypendent hook approve"),
            "the diagnostic names the hook and the remedy: {error}"
        );
    }
}
