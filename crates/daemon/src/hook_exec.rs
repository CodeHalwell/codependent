//! Hook subprocess execution and verdict mapping (adoption 08).
//!
//! Lowers an approved [`HookSpec`] to a [`SandboxProfile`] + [`SandboxCommand`],
//! delivers the JSON [`HookPayload`] over stdin, parses the last `HOOK_CONTROL`
//! line on stdout, and maps the outcome onto [`HookVerdict`] per the verdict lattice.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use codypendent_sandbox::executor::{
    SandboxCommand, SandboxError, SandboxExecutor, SandboxOutcome,
};
use codypendent_sandbox::hook::{
    FailurePolicy, HookKind, HookRuntime, HookSpec, HookVerdict, ToolCall,
};
use codypendent_sandbox::profile::SandboxProfile;
use serde::{Deserialize, Serialize};

/// Maximum allowed arguments_json size in a rewrite (256 KiB).
pub const MAX_REWRITE_ARGUMENTS_BYTES: usize = 256 * 1024;

/// JSON payload delivered to the hook subprocess over stdin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookPayload<'a> {
    pub payload_version: u32,
    pub event: &'a str,
    pub hook_id: &'a str,
    pub session_id: String,
    pub run_id: String,
    pub repository: String,
    pub worktree: String,
    pub triggered_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<HookPayloadTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<HookPayloadOutcome>,
}

/// Tool invocation details in the payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookPayloadTool<'a> {
    pub name: &'a str,
    pub arguments_json: &'a str,
}

/// Tool outcome in a `tool.post` payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookPayloadOutcome {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub duration_ms: u64,
}

/// Decision field in `HOOK_CONTROL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookControlDecision {
    Allow,
    Deny,
    Rewrite,
}

/// Replacement tool call in a `mutate` rewrite control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookControlRewrite {
    pub name: String,
    pub arguments_json: String,
}

/// Control line emitted by the hook process on stdout.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookControl {
    pub decision: HookControlDecision,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub rewrite: Option<HookControlRewrite>,
}

/// Context paths for running a hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRunContextPaths {
    pub repository: PathBuf,
    pub worktree: PathBuf,
    pub hook_dir: PathBuf,
}

/// Parse the hook control line from subprocess stdout.
/// Follows cline's last-`HOOK_CONTROL\t`-line convention, with whole-stdout JSON fallback.
pub fn parse_control(stdout: &str) -> Result<Option<HookControl>, String> {
    let mut last_control = None;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("HOOK_CONTROL\t") {
            last_control = Some(rest);
        }
    }

    if let Some(control_json) = last_control {
        return serde_json::from_str::<HookControl>(control_json.trim())
            .map(Some)
            .map_err(|err| format!("malformed HOOK_CONTROL: {err}"));
    }

    let trimmed = stdout.trim();
    if !trimmed.is_empty() && trimmed.starts_with('{') && trimmed.ends_with('}') {
        if let Ok(control) = serde_json::from_str::<HookControl>(trimmed) {
            return Ok(Some(control));
        }
    }

    Ok(None)
}

/// Substitute placeholders in working directory paths (`$REPOSITORY`, `$WORKTREE`, `$HOME`).
pub fn substitute_hook_placeholders(
    value: &str,
    ctx: &HookRunContextPaths,
) -> Result<PathBuf, String> {
    let mut resolved = String::new();
    let mut rest = value;
    while let Some(at) = rest.find('$') {
        resolved.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        let end = tail
            .find(|c: char| !(c.is_ascii_uppercase() || c == '_'))
            .unwrap_or(tail.len());
        let name = &tail[..end];
        match name {
            "REPOSITORY" => resolved.push_str(&ctx.repository.to_string_lossy()),
            "WORKTREE" => resolved.push_str(&ctx.worktree.to_string_lossy()),
            "HOME" => {
                let home = std::env::var("HOME")
                    .map_err(|_| "HOME environment variable not set".to_string())?;
                resolved.push_str(&home);
            }
            other => return Err(format!("unknown placeholder ${other}")),
        }
        rest = &tail[end..];
    }
    resolved.push_str(rest);
    Ok(PathBuf::from(resolved))
}

/// Lower a [`HookSpec`] into a [`SandboxProfile`].
#[must_use]
pub fn profile_for_hook(spec: &HookSpec, ctx: &HookRunContextPaths) -> SandboxProfile {
    let HookRuntime::Command {
        timeout_seconds, ..
    } = spec.runtime;
    let spec_timeout = timeout_seconds;
    SandboxProfile {
        plugin: format!("hook:{}", spec.id),
        env_allowlist: Vec::new(),
        read_paths: vec![
            ctx.worktree.to_string_lossy().to_string(),
            ctx.hook_dir.to_string_lossy().to_string(),
        ],
        // Write access to worktree is deliberate (e.g. `cargo test` builds target/).
        write_paths: vec![ctx.worktree.to_string_lossy().to_string()],
        network_allowlist: Vec::new(),
        brokered_secrets: Vec::new(),
        allow_subprocess: true,
        memory_mb: 512,
        cpu_seconds: spec_timeout,
        wall_seconds: spec_timeout,
        maximum_output_mb: 1,
    }
}

/// Audit record for one hook invocation (`hook_dispatches` table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchAudit {
    pub hook_row_id: String,
    pub run_id: Option<String>,
    pub event: String,
    pub subject_digest: String,
    pub verdict: String,
    pub applied: String,
    pub rewrote_action: Option<String>,
    pub exit_status: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: i64,
    pub output_bytes: i64,
    pub error: Option<String>,
}

/// Runs hook subprocesses in the enforcing sandbox and maps outcomes to verdicts.
pub struct HookRunner {
    executor: Arc<dyn SandboxExecutor>,
}

impl HookRunner {
    /// Create a new runner with the given sandbox executor.
    #[must_use]
    pub fn new(executor: Arc<dyn SandboxExecutor>) -> Self {
        Self { executor }
    }

    /// Run one hook against the payload.
    pub fn run_hook(
        &self,
        hook_row_id: &str,
        spec: &HookSpec,
        payload: &HookPayload<'_>,
        ctx: &HookRunContextPaths,
        subject_digest: &str,
    ) -> (HookVerdict, DispatchAudit) {
        let started = Instant::now();
        let HookRuntime::Command {
            program,
            args,
            working_directory,
            ..
        } = &spec.runtime;

        let cwd = match working_directory {
            Some(dir) => match substitute_hook_placeholders(dir, ctx) {
                Ok(path) => path,
                Err(err) => {
                    let duration_ms = started.elapsed().as_millis() as i64;
                    let (verdict, applied) = match spec.policy.failure {
                        FailurePolicy::Block => (
                            HookVerdict::Deny {
                                reason: format!("working directory substitution failed: {err}"),
                            },
                            "denied".to_string(),
                        ),
                        FailurePolicy::Warn => (HookVerdict::Observed, "allowed".to_string()),
                    };
                    return (
                        verdict,
                        DispatchAudit {
                            hook_row_id: hook_row_id.to_string(),
                            run_id: Some(payload.run_id.clone()),
                            event: payload.event.to_string(),
                            subject_digest: subject_digest.to_string(),
                            verdict: "deny".to_string(),
                            applied,
                            rewrote_action: None,
                            exit_status: None,
                            timed_out: false,
                            duration_ms,
                            output_bytes: 0,
                            error: Some(err),
                        },
                    );
                }
            },
            None => ctx.worktree.clone(),
        };

        let payload_bytes = match serde_json::to_vec(payload) {
            Ok(bytes) => bytes,
            Err(err) => {
                let duration_ms = started.elapsed().as_millis() as i64;
                return (
                    HookVerdict::Deny {
                        reason: format!("failed to serialize hook payload: {err}"),
                    },
                    DispatchAudit {
                        hook_row_id: hook_row_id.to_string(),
                        run_id: Some(payload.run_id.clone()),
                        event: payload.event.to_string(),
                        subject_digest: subject_digest.to_string(),
                        verdict: "deny".to_string(),
                        applied: "denied".to_string(),
                        rewrote_action: None,
                        exit_status: None,
                        timed_out: false,
                        duration_ms,
                        output_bytes: 0,
                        error: Some(err.to_string()),
                    },
                );
            }
        };

        let profile = profile_for_hook(spec, ctx);
        let command = SandboxCommand::new(
            PathBuf::from(program),
            args.clone(),
            cwd,
            format!("hook:{}", spec.id),
        )
        .with_stdin(payload_bytes);

        let outcome = self.executor.run(&profile, &command);
        let duration_ms = started.elapsed().as_millis() as i64;

        self.map_outcome(
            hook_row_id,
            spec,
            payload,
            subject_digest,
            outcome,
            duration_ms,
        )
    }

    fn map_outcome(
        &self,
        hook_row_id: &str,
        spec: &HookSpec,
        payload: &HookPayload<'_>,
        subject_digest: &str,
        outcome_result: Result<SandboxOutcome, SandboxError>,
        fallback_duration_ms: i64,
    ) -> (HookVerdict, DispatchAudit) {
        match outcome_result {
            Ok(outcome) => {
                let duration_ms = outcome.duration.as_millis() as i64;
                let output_bytes = (outcome.stdout.text.len() + outcome.stderr.text.len()) as i64;
                let timed_out = outcome.timed_out;
                let exit_status = outcome.exit_code;

                if timed_out || exit_status != Some(0) {
                    let err_msg = if timed_out {
                        format!("hook timed out after {}s", hook_spec_timeout(spec))
                    } else {
                        let stderr_head = outcome
                            .stderr
                            .text
                            .lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(200)
                            .collect::<String>();
                        format!(
                            "hook failed: exit {} — {}",
                            exit_status.unwrap_or(-1),
                            stderr_head
                        )
                    };

                    let (verdict, applied_status) = match spec.policy.failure {
                        FailurePolicy::Block => {
                            (HookVerdict::Deny { reason: err_msg }, "denied".to_string())
                        }
                        FailurePolicy::Warn => (HookVerdict::Observed, "allowed".to_string()),
                    };

                    return (
                        verdict,
                        DispatchAudit {
                            hook_row_id: hook_row_id.to_string(),
                            run_id: Some(payload.run_id.clone()),
                            event: payload.event.to_string(),
                            subject_digest: subject_digest.to_string(),
                            verdict: "deny".to_string(),
                            applied: applied_status,
                            rewrote_action: None,
                            exit_status,
                            timed_out,
                            duration_ms,
                            output_bytes,
                            error: None,
                        },
                    );
                }

                // Exit 0 path — parse control line from stdout.
                match parse_control(&outcome.stdout.text) {
                    Ok(Some(control)) => match (spec.kind, control.decision) {
                        (HookKind::Observe, _) => (
                            HookVerdict::Observed,
                            DispatchAudit {
                                hook_row_id: hook_row_id.to_string(),
                                run_id: Some(payload.run_id.clone()),
                                event: payload.event.to_string(),
                                subject_digest: subject_digest.to_string(),
                                verdict: "observe".to_string(),
                                applied: "allowed".to_string(),
                                rewrote_action: None,
                                exit_status,
                                timed_out: false,
                                duration_ms,
                                output_bytes,
                                error: None,
                            },
                        ),
                        (HookKind::Validate | HookKind::Mutate, HookControlDecision::Allow) => (
                            HookVerdict::Allow,
                            DispatchAudit {
                                hook_row_id: hook_row_id.to_string(),
                                run_id: Some(payload.run_id.clone()),
                                event: payload.event.to_string(),
                                subject_digest: subject_digest.to_string(),
                                verdict: "allow".to_string(),
                                applied: "allowed".to_string(),
                                rewrote_action: None,
                                exit_status,
                                timed_out: false,
                                duration_ms,
                                output_bytes,
                                error: None,
                            },
                        ),
                        (HookKind::Validate | HookKind::Mutate, HookControlDecision::Deny) => {
                            let reason = control
                                .reason
                                .unwrap_or_else(|| "denied by hook".to_string());
                            (
                                HookVerdict::Deny { reason },
                                DispatchAudit {
                                    hook_row_id: hook_row_id.to_string(),
                                    run_id: Some(payload.run_id.clone()),
                                    event: payload.event.to_string(),
                                    subject_digest: subject_digest.to_string(),
                                    verdict: "deny".to_string(),
                                    applied: "denied".to_string(),
                                    rewrote_action: None,
                                    exit_status,
                                    timed_out: false,
                                    duration_ms,
                                    output_bytes,
                                    error: None,
                                },
                            )
                        }
                        (HookKind::Validate, HookControlDecision::Rewrite) => (
                            HookVerdict::Deny {
                                reason: "validate hook attempted a rewrite".to_string(),
                            },
                            DispatchAudit {
                                hook_row_id: hook_row_id.to_string(),
                                run_id: Some(payload.run_id.clone()),
                                event: payload.event.to_string(),
                                subject_digest: subject_digest.to_string(),
                                verdict: "deny".to_string(),
                                applied: "denied".to_string(),
                                rewrote_action: None,
                                exit_status,
                                timed_out: false,
                                duration_ms,
                                output_bytes,
                                error: Some("validate hook attempted a rewrite".to_string()),
                            },
                        ),
                        (HookKind::Mutate, HookControlDecision::Rewrite) => {
                            if let Some(rewrite) = control.rewrite {
                                if rewrite.arguments_json.len() > MAX_REWRITE_ARGUMENTS_BYTES
                                    || serde_json::from_str::<serde_json::Value>(
                                        &rewrite.arguments_json,
                                    )
                                    .is_err()
                                {
                                    (
                                        HookVerdict::Deny {
                                            reason: "malformed rewrite".to_string(),
                                        },
                                        DispatchAudit {
                                            hook_row_id: hook_row_id.to_string(),
                                            run_id: Some(payload.run_id.clone()),
                                            event: payload.event.to_string(),
                                            subject_digest: subject_digest.to_string(),
                                            verdict: "deny".to_string(),
                                            applied: "denied".to_string(),
                                            rewrote_action: None,
                                            exit_status,
                                            timed_out: false,
                                            duration_ms,
                                            output_bytes,
                                            error: Some("malformed rewrite".to_string()),
                                        },
                                    )
                                } else {
                                    let call = ToolCall {
                                        name: rewrite.name,
                                        arguments_json: rewrite.arguments_json,
                                    };
                                    let rewrote_digest = call.digest();
                                    (
                                        HookVerdict::Rewrite { call },
                                        DispatchAudit {
                                            hook_row_id: hook_row_id.to_string(),
                                            run_id: Some(payload.run_id.clone()),
                                            event: payload.event.to_string(),
                                            subject_digest: subject_digest.to_string(),
                                            verdict: "rewrite".to_string(),
                                            applied: "allowed".to_string(), // will be updated when rewrite executes/refuses
                                            rewrote_action: Some(rewrote_digest),
                                            exit_status,
                                            timed_out: false,
                                            duration_ms,
                                            output_bytes,
                                            error: None,
                                        },
                                    )
                                }
                            } else {
                                (
                                    HookVerdict::Deny {
                                        reason: "malformed rewrite: missing rewrite field"
                                            .to_string(),
                                    },
                                    DispatchAudit {
                                        hook_row_id: hook_row_id.to_string(),
                                        run_id: Some(payload.run_id.clone()),
                                        event: payload.event.to_string(),
                                        subject_digest: subject_digest.to_string(),
                                        verdict: "deny".to_string(),
                                        applied: "denied".to_string(),
                                        rewrote_action: None,
                                        exit_status,
                                        timed_out: false,
                                        duration_ms,
                                        output_bytes,
                                        error: Some("missing rewrite payload".to_string()),
                                    },
                                )
                            }
                        }
                    },
                    Ok(None) => match spec.kind {
                        HookKind::Observe => (
                            HookVerdict::Observed,
                            DispatchAudit {
                                hook_row_id: hook_row_id.to_string(),
                                run_id: Some(payload.run_id.clone()),
                                event: payload.event.to_string(),
                                subject_digest: subject_digest.to_string(),
                                verdict: "observe".to_string(),
                                applied: "allowed".to_string(),
                                rewrote_action: None,
                                exit_status,
                                timed_out: false,
                                duration_ms,
                                output_bytes,
                                error: None,
                            },
                        ),
                        HookKind::Validate | HookKind::Mutate => (
                            HookVerdict::Allow,
                            DispatchAudit {
                                hook_row_id: hook_row_id.to_string(),
                                run_id: Some(payload.run_id.clone()),
                                event: payload.event.to_string(),
                                subject_digest: subject_digest.to_string(),
                                verdict: "allow".to_string(),
                                applied: "allowed".to_string(),
                                rewrote_action: None,
                                exit_status,
                                timed_out: false,
                                duration_ms,
                                output_bytes,
                                error: None,
                            },
                        ),
                    },
                    Err(err) => match spec.kind {
                        HookKind::Observe => (
                            HookVerdict::Observed,
                            DispatchAudit {
                                hook_row_id: hook_row_id.to_string(),
                                run_id: Some(payload.run_id.clone()),
                                event: payload.event.to_string(),
                                subject_digest: subject_digest.to_string(),
                                verdict: "observe".to_string(),
                                applied: "allowed".to_string(),
                                rewrote_action: None,
                                exit_status,
                                timed_out: false,
                                duration_ms,
                                output_bytes,
                                error: Some(err),
                            },
                        ),
                        HookKind::Validate | HookKind::Mutate => (
                            HookVerdict::Deny {
                                reason: err.clone(),
                            },
                            DispatchAudit {
                                hook_row_id: hook_row_id.to_string(),
                                run_id: Some(payload.run_id.clone()),
                                event: payload.event.to_string(),
                                subject_digest: subject_digest.to_string(),
                                verdict: "deny".to_string(),
                                applied: "denied".to_string(),
                                rewrote_action: None,
                                exit_status,
                                timed_out: false,
                                duration_ms,
                                output_bytes,
                                error: Some(err),
                            },
                        ),
                    },
                }
            }
            Err(err) => {
                let err_str = err.to_string();
                let (verdict, applied_status) = match spec.policy.failure {
                    FailurePolicy::Block => (
                        HookVerdict::Deny {
                            reason: format!("sandbox refused hook execution: {err_str}"),
                        },
                        "denied".to_string(),
                    ),
                    FailurePolicy::Warn => (HookVerdict::Observed, "allowed".to_string()),
                };

                (
                    verdict,
                    DispatchAudit {
                        hook_row_id: hook_row_id.to_string(),
                        run_id: Some(payload.run_id.clone()),
                        event: payload.event.to_string(),
                        subject_digest: subject_digest.to_string(),
                        verdict: "deny".to_string(),
                        applied: applied_status,
                        rewrote_action: None,
                        exit_status: None,
                        timed_out: false,
                        duration_ms: fallback_duration_ms,
                        output_bytes: 0,
                        error: Some(err_str),
                    },
                )
            }
        }
    }
}

fn hook_spec_timeout(spec: &HookSpec) -> u64 {
    let HookRuntime::Command {
        timeout_seconds, ..
    } = spec.runtime;
    timeout_seconds
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_sandbox::executor::CapabilityReport;
    use codypendent_sandbox::hook::{HookEvent, HookPolicy, HookScope};

    struct MockSandbox {
        outcome: Result<SandboxOutcome, String>,
    }

    impl SandboxExecutor for MockSandbox {
        fn capability_report(&self) -> CapabilityReport {
            CapabilityReport {
                platform: "mock",
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
            match &self.outcome {
                Ok(out) => Ok(out.clone()),
                Err(err) => Err(SandboxError::InvalidCommand(err.clone())),
            }
        }

        fn prepare_interactive(
            &self,
            _profile: &SandboxProfile,
            _command: &SandboxCommand,
        ) -> Result<codypendent_sandbox::executor::SandboxProcessSpec, SandboxError> {
            Err(SandboxError::InvalidCommand("not supported".into()))
        }
    }

    fn sample_spec(kind: HookKind, failure: FailurePolicy) -> HookSpec {
        HookSpec {
            schema_version: 1,
            id: "test.hook".into(),
            name: "Test Hook".into(),
            scope: HookScope::User,
            event: HookEvent::ToolPre,
            kind,
            priority: 100,
            runtime: HookRuntime::Command {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "echo ok".into()],
                working_directory: None,
                timeout_seconds: 30,
            },
            policy: HookPolicy {
                failure,
                requires_approval: kind == HookKind::Mutate,
                network: codypendent_sandbox::hook::HookNetwork::Deny,
            },
            output: Default::default(),
        }
    }

    fn sample_paths() -> HookRunContextPaths {
        HookRunContextPaths {
            repository: PathBuf::from("/repo"),
            worktree: PathBuf::from("/worktree"),
            hook_dir: PathBuf::from("/hooks/test.hook"),
        }
    }

    #[test]
    fn payload_serializes_stable_wire_shape() {
        let payload = HookPayload {
            payload_version: 1,
            event: "tool.pre",
            hook_id: "rust.verify",
            session_id: "sess-123".into(),
            run_id: "run-456".into(),
            repository: "/path/to/repo".into(),
            worktree: "/path/to/worktree".into(),
            triggered_at: "2026-08-15T00:00:00Z".into(),
            tool: Some(HookPayloadTool {
                name: "shell.run",
                arguments_json: r#"{"command":"cargo test"}"#,
            }),
            outcome: None,
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains(r#""payload_version":1"#));
        assert!(json.contains(r#""event":"tool.pre""#));
        assert!(json.contains(r#""hook_id":"rust.verify""#));
        assert!(json.contains(r#""session_id":"sess-123""#));
        assert!(json.contains(r#""run_id":"run-456""#));
        assert!(json.contains(r#""repository":"/path/to/repo""#));
        assert!(json.contains(r#""worktree":"/path/to/worktree""#));
        assert!(json.contains(r#""triggered_at":"2026-08-15T00:00:00Z""#));
        assert!(json.contains(
            r#""tool":{"name":"shell.run","arguments_json":"{\"command\":\"cargo test\"}"}"#
        ));
        assert!(!json.contains(r#""outcome""#));
    }

    #[test]
    fn control_line_last_hook_control_wins() {
        let stdout = "some log line\nHOOK_CONTROL\t{\"decision\":\"allow\"}\nignored log\nHOOK_CONTROL\t{\"decision\":\"deny\",\"reason\":\"failed check\"}\ntrailing log\n";
        let parsed = parse_control(stdout).unwrap().unwrap();
        assert_eq!(parsed.decision, HookControlDecision::Deny);
        assert_eq!(parsed.reason, Some("failed check".into()));
    }

    #[test]
    fn whole_stdout_fallback_parses() {
        let stdout = r#"{"decision":"deny","reason":"blocked"}"#;
        let parsed = parse_control(stdout).unwrap().unwrap();
        assert_eq!(parsed.decision, HookControlDecision::Deny);
        assert_eq!(parsed.reason, Some("blocked".into()));
    }

    #[test]
    fn unknown_control_fields_are_a_protocol_error() {
        let stdout =
            "HOOK_CONTROL\t{\"decision\":\"allow\",\"systemPrompt\":\"injected prompt\"}\n";
        let err = parse_control(stdout).unwrap_err();
        assert!(err.contains("malformed HOOK_CONTROL"));
        assert!(err.contains("unknown field `systemPrompt`"));
    }

    #[test]
    fn verdict_table_tests() {
        let spec_validate = sample_spec(HookKind::Validate, FailurePolicy::Block);
        let spec_mutate = sample_spec(HookKind::Mutate, FailurePolicy::Block);
        let paths = sample_paths();

        // 1. Exit 0, allow control -> Allow
        let runner = HookRunner::new(Arc::new(MockSandbox {
            outcome: Ok(SandboxOutcome {
                backend: codypendent_sandbox::executor::SandboxBackend::None,
                exit_code: Some(0),
                timed_out: false,
                duration: std::time::Duration::from_millis(50),
                stdout: codypendent_sandbox::sanitize::sanitize_untrusted(
                    "test",
                    "HOOK_CONTROL\t{\"decision\":\"allow\"}",
                    1024,
                ),
                stderr: codypendent_sandbox::sanitize::sanitize_untrusted("test", "", 1024),
                output_truncated: false,
            }),
        }));
        let payload = HookPayload {
            payload_version: 1,
            event: "tool.pre",
            hook_id: "test.hook",
            session_id: "s1".into(),
            run_id: "r1".into(),
            repository: "/repo".into(),
            worktree: "/worktree".into(),
            triggered_at: "2026-08-15T00:00:00Z".into(),
            tool: None,
            outcome: None,
        };
        let (verdict, audit) = runner.run_hook("h1", &spec_validate, &payload, &paths, "digest1");
        assert_eq!(verdict, HookVerdict::Allow);
        assert_eq!(audit.applied, "allowed");

        // 2. Exit 0, deny control -> Deny
        let runner_deny = HookRunner::new(Arc::new(MockSandbox {
            outcome: Ok(SandboxOutcome {
                backend: codypendent_sandbox::executor::SandboxBackend::None,
                exit_code: Some(0),
                timed_out: false,
                duration: std::time::Duration::from_millis(50),
                stdout: codypendent_sandbox::sanitize::sanitize_untrusted(
                    "test",
                    "HOOK_CONTROL\t{\"decision\":\"deny\",\"reason\":\"tests must pass\"}",
                    1024,
                ),
                stderr: codypendent_sandbox::sanitize::sanitize_untrusted("test", "", 1024),
                output_truncated: false,
            }),
        }));
        let (verdict, audit) =
            runner_deny.run_hook("h1", &spec_validate, &payload, &paths, "digest1");
        assert_eq!(
            verdict,
            HookVerdict::Deny {
                reason: "tests must pass".into()
            }
        );
        assert_eq!(audit.applied, "denied");

        // 3. Exit 0, validate hook attempted rewrite -> Deny
        let runner_rewrite = HookRunner::new(Arc::new(MockSandbox {
            outcome: Ok(SandboxOutcome {
                backend: codypendent_sandbox::executor::SandboxBackend::None,
                exit_code: Some(0),
                timed_out: false,
                duration: std::time::Duration::from_millis(50),
                stdout: codypendent_sandbox::sanitize::sanitize_untrusted(
                    "test",
                    r#"HOOK_CONTROL	{"decision":"rewrite","rewrite":{"name":"shell.run","arguments_json":"{}"}}"#,
                    1024,
                ),
                stderr: codypendent_sandbox::sanitize::sanitize_untrusted("test", "", 1024),
                output_truncated: false,
            }),
        }));
        let (verdict, audit) =
            runner_rewrite.run_hook("h1", &spec_validate, &payload, &paths, "digest1");
        assert_eq!(
            verdict,
            HookVerdict::Deny {
                reason: "validate hook attempted a rewrite".into()
            }
        );
        assert_eq!(audit.applied, "denied");

        // 4. Exit 0, mutate hook rewrite -> Rewrite
        let (verdict, audit) =
            runner_rewrite.run_hook("h1", &spec_mutate, &payload, &paths, "digest1");
        assert_eq!(
            verdict,
            HookVerdict::Rewrite {
                call: ToolCall {
                    name: "shell.run".into(),
                    arguments_json: "{}".into()
                }
            }
        );
        assert_eq!(audit.verdict, "rewrite");

        // 5. Exit 1, failure = block -> Deny
        let runner_fail_block = HookRunner::new(Arc::new(MockSandbox {
            outcome: Ok(SandboxOutcome {
                backend: codypendent_sandbox::executor::SandboxBackend::None,
                exit_code: Some(1),
                timed_out: false,
                duration: std::time::Duration::from_millis(50),
                stdout: codypendent_sandbox::sanitize::sanitize_untrusted("test", "", 1024),
                stderr: codypendent_sandbox::sanitize::sanitize_untrusted(
                    "test",
                    "assertion failed\n",
                    1024,
                ),
                output_truncated: false,
            }),
        }));
        let (verdict, audit) =
            runner_fail_block.run_hook("h1", &spec_validate, &payload, &paths, "digest1");
        match verdict {
            HookVerdict::Deny { reason } => {
                assert!(reason.contains("exit 1"));
                assert!(reason.contains("assertion failed"));
            }
            other => panic!("expected Deny, got {other:?}"),
        }
        assert_eq!(audit.applied, "denied");

        // 6. Exit 1, failure = warn -> Observed
        let spec_warn = sample_spec(HookKind::Validate, FailurePolicy::Warn);
        let (verdict, audit) =
            runner_fail_block.run_hook("h1", &spec_warn, &payload, &paths, "digest1");
        assert_eq!(verdict, HookVerdict::Observed);
        assert_eq!(audit.applied, "allowed");
    }

    #[test]
    fn timeout_kills_and_maps_through_failure_policy() {
        let spec_block = sample_spec(HookKind::Validate, FailurePolicy::Block);
        let spec_warn = sample_spec(HookKind::Validate, FailurePolicy::Warn);
        let paths = sample_paths();
        let payload = HookPayload {
            payload_version: 1,
            event: "tool.pre",
            hook_id: "test.hook",
            session_id: "s1".into(),
            run_id: "r1".into(),
            repository: "/repo".into(),
            worktree: "/worktree".into(),
            triggered_at: "2026-08-15T00:00:00Z".into(),
            tool: None,
            outcome: None,
        };

        let runner_timeout = HookRunner::new(Arc::new(MockSandbox {
            outcome: Ok(SandboxOutcome {
                backend: codypendent_sandbox::executor::SandboxBackend::None,
                exit_code: None,
                timed_out: true,
                duration: std::time::Duration::from_secs(30),
                stdout: codypendent_sandbox::sanitize::sanitize_untrusted("test", "", 1024),
                stderr: codypendent_sandbox::sanitize::sanitize_untrusted("test", "", 1024),
                output_truncated: false,
            }),
        }));

        let (verdict_block, audit_block) =
            runner_timeout.run_hook("h1", &spec_block, &payload, &paths, "d1");
        assert!(matches!(verdict_block, HookVerdict::Deny { .. }));
        assert!(audit_block.timed_out);
        assert_eq!(audit_block.applied, "denied");

        let (verdict_warn, audit_warn) =
            runner_timeout.run_hook("h1", &spec_warn, &payload, &paths, "d1");
        assert_eq!(verdict_warn, HookVerdict::Observed);
        assert!(audit_warn.timed_out);
        assert_eq!(audit_warn.applied, "allowed");
    }
}
