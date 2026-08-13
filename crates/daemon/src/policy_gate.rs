//! The daemon half of the sandbox capability seam (outcomes 12 + 13).
//!
//! `crates/daemon` already depends on `codypendent-sandbox`, so the sandbox
//! crate cannot depend back on the daemon to call [`PolicyEngine::evaluate`] —
//! that would be a cycle, and mirroring the run capabilities into a third enum
//! inside the sandbox crate would reproduce the very defect the seam exists to
//! remove. So the dependency is inverted: `codypendent_sandbox::gate` defines
//! the *vocabulary* ([`HostRequest`]) and the *seam* ([`RunPolicyGate`]), and
//! this module implements it — the daemon is the one layer where both the run
//! capability model and the plugin capability model are legitimately visible.
//!
//! # What this module must never do
//!
//! **Consult the package's manifest.** `CapabilityBroker` applies the manifest
//! as a declaration *ceiling* before this gate is ever called; applying it twice
//! would hide which gate refused and re-create the second policy path.
//!
//! **Turn an approval prompt into an allow.** A sandboxed guest cannot block on
//! an interactive prompt, so a `RequireApproval` decision is a refusal here, not
//! a wait — see [`RunPolicyAdapter::authorize`].
//!
//! # Wiring status
//!
//! This is the adapter, not its installation. Nothing in the workspace
//! constructs a `SkillRunner::enforcing(...)` yet, so no guest currently runs
//! under it; the assembly wires it at the point where a run's [`PolicyEngine`]
//! and [`EvalContext`] are built. Until then the shipped behaviour is
//! `DenyAllGate` — a guest can compute and nothing else — which is the correct
//! default for an unwired capability path.

use std::sync::Arc;

use codypendent_protocol::ProposedAction;
use codypendent_sandbox::gate::{GateDenied, GateGrant, GateSeal, HostRequest, RunPolicyGate};
use codypendent_sandbox::hook::{PolicyReentry, ToolCall};

use crate::policy::{Decision, EvalContext, PolicyDecision, PolicyEngine};

/// Lowers a tool call onto the [`ProposedAction`] the run policy evaluates.
///
/// The daemon has no tool registry — tool names and argument schemas live in
/// `codypendent-runtime`, which sits *above* this crate — so the assembly
/// supplies this. An adapter built without one refuses every hook rewrite
/// (`policy.unknown-tool`) rather than guessing at what a tool does.
pub type ToolCallLowering = Arc<dyn Fn(&str, &str) -> Option<ProposedAction> + Send + Sync>;

/// Puts a sandboxed guest — and a hook's rewritten tool call — under the SAME
/// deny-first policy every model-proposed side effect passes.
#[derive(Clone)]
pub struct RunPolicyAdapter {
    engine: PolicyEngine,
    ctx: EvalContext,
    lower_tool: Option<ToolCallLowering>,
}

impl std::fmt::Debug for RunPolicyAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunPolicyAdapter")
            .field("ctx", &self.ctx)
            .field("lowers_tool_calls", &self.lower_tool.is_some())
            .finish_non_exhaustive()
    }
}

impl RunPolicyAdapter {
    /// An adapter over the run's engine and evaluation context. Hook re-entry is
    /// refused until [`with_tool_lowering`](Self::with_tool_lowering) supplies a
    /// lowering — fail-closed, so a caller that forgets it loses hook rewrites
    /// rather than gaining unevaluated ones.
    #[must_use]
    pub fn new(engine: PolicyEngine, ctx: EvalContext) -> Self {
        Self {
            engine,
            ctx,
            lower_tool: None,
        }
    }

    /// Teach the adapter to lower tool calls, enabling [`PolicyReentry`].
    #[must_use]
    pub fn with_tool_lowering(mut self, lowering: ToolCallLowering) -> Self {
        self.lower_tool = Some(lowering);
        self
    }

    /// Lower a guest request onto the action the run policy understands.
    ///
    /// `HostRequest` is deliberately isomorphic to the *privileged subset* of
    /// `ProposedAction`, so this is a total mapping for everything the host can
    /// actually perform. The two variants with no honest lowering are refused
    /// here rather than approximated: inventing a `WritePatch` with no change
    /// set behind it, or a secret read with no broker to serve it, would put an
    /// action in the audit ledger that never happened.
    fn lower(&self, request: &HostRequest) -> Result<ProposedAction, GateDenied> {
        match request {
            HostRequest::ReadFile { path } => Ok(ProposedAction::ReadFiles {
                paths: vec![path.clone()],
            }),
            HostRequest::WriteFile { .. } => Err(GateDenied::new(
                "policy.unsupported-action",
                "sandboxed writes are not supported",
            )),
            HostRequest::RunCommand { program, args } => Ok(ProposedAction::ExecuteCommand {
                program: program.clone(),
                args: args.clone(),
                // A guest inherits nothing: an unshown environment is exactly
                // the smuggling channel `ExecuteCommand.environment` exists to
                // close. And the run's worktree is the only directory it may run
                // in — the guest does not get to name one.
                environment: Vec::new(),
                cwd: Some(self.ctx.worktree.to_string_lossy().into_owned()),
            }),
            HostRequest::Connect { host, port } => Ok(ProposedAction::NetworkRequest {
                destination: format!("{host}:{port}"),
            }),
            HostRequest::ReadSecret { .. } => Err(GateDenied::new(
                "policy.no-secret-broker",
                "brokered secrets are not implemented",
            )),
        }
    }
}

/// The first reason on a decision, or a stable fallback pair. A denial must
/// always carry a code, even from an engine that returned none.
fn denial_of(decision: &PolicyDecision, fallback_code: &str, fallback_message: &str) -> GateDenied {
    match decision.reasons.first() {
        Some(reason) => GateDenied::new(reason.code.clone(), reason.message.clone()),
        None => GateDenied::new(fallback_code, fallback_message),
    }
}

impl RunPolicyGate for RunPolicyAdapter {
    fn authorize(&self, request: &HostRequest, seal: &GateSeal) -> Result<GateGrant, GateDenied> {
        let action = self.lower(request)?;
        let decision = self.engine.evaluate(&action, &self.ctx);
        match decision.decision {
            Decision::Allow => Ok(GateGrant::issue(
                seal,
                format!("run-policy@{}", decision.policy_version),
            )),
            Decision::Deny => Err(denial_of(
                &decision,
                "policy.denied",
                "the run policy refused this request",
            )),
            // A guest runs unattended: there is nobody to answer a prompt, and
            // no approval can be bound to a request whose digest this crate
            // cannot even compute (`HostRequest::digest` is private to the
            // sandbox crate, deliberately). Refusing is the only answer that
            // cannot be turned into an allow by retrying.
            Decision::RequireApproval => Err(GateDenied::new(
                "policy.approval-required",
                "this action needs a human approval that has not been granted",
            )),
        }
    }
}

impl PolicyReentry for RunPolicyAdapter {
    fn evaluate(&self, call: &ToolCall) -> Result<bool, String> {
        let lowering = self
            .lower_tool
            .as_ref()
            .ok_or_else(|| "policy.unknown-tool".to_string())?;
        let action = lowering(&call.name, &call.arguments_json)
            .ok_or_else(|| "policy.unknown-tool".to_string())?;
        let decision = self.engine.evaluate(&action, &self.ctx);
        match decision.decision {
            Decision::Allow => Ok(true),
            // `false` = "allowed, but a human must approve it". The sandbox
            // crate then requires the approval digest to match THIS call, which
            // is what stops a rewrite inheriting the original's approval.
            Decision::RequireApproval => Ok(false),
            Decision::Deny => Err(decision
                .reasons
                .first()
                .map(|reason| reason.code.clone())
                .unwrap_or_else(|| "policy.denied".to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_sandbox::gate::CapabilityBroker;
    use codypendent_sandbox::profile::SandboxProfile;

    fn adapter(root: &std::path::Path) -> RunPolicyAdapter {
        RunPolicyAdapter::new(PolicyEngine::with_defaults(), EvalContext::new(root, root))
    }

    /// A profile that declares everything, so the ceiling never decides a test
    /// for us: what these tests exercise is the RUN POLICY half of the broker.
    fn permissive_profile(root: &std::path::Path) -> SandboxProfile {
        SandboxProfile {
            plugin: "test-plugin@1".to_string(),
            env_allowlist: Vec::new(),
            read_paths: vec![root.to_string_lossy().into_owned()],
            write_paths: vec![root.to_string_lossy().into_owned()],
            network_allowlist: vec!["example.com:443".to_string()],
            brokered_secrets: vec!["token".to_string()],
            allow_subprocess: true,
            memory_mb: 64,
            cpu_seconds: 5,
            wall_seconds: 5,
            maximum_output_mb: 1,
        }
    }

    #[test]
    fn a_read_inside_the_worktree_is_granted_by_the_run_policy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize");
        let file = root.join("src.rs");
        std::fs::write(&file, b"fn main() {}").expect("write");
        let profile = permissive_profile(&root);
        let gate = adapter(&root);
        let grant = CapabilityBroker::new(&profile, &gate)
            .request(&HostRequest::ReadFile {
                path: file.to_string_lossy().into_owned(),
            })
            .expect("the run policy allows a read inside the worktree");
        assert!(grant.authority().starts_with("run-policy@"));
    }

    /// A read OUTSIDE the run's roots is refused by the run policy even though
    /// the manifest ceiling declared file reads — the whole point of the seam
    /// being two gates and not one.
    #[test]
    fn a_read_the_manifest_declared_is_still_refused_outside_the_run_roots() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().canonicalize().expect("canonicalize");
        let root = base.join("run");
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir_all(&root).expect("run root");
        std::fs::create_dir_all(&elsewhere).expect("other dir");
        let secret = elsewhere.join("secret.txt");
        std::fs::write(&secret, b"not yours").expect("write");

        // The ceiling is deliberately WIDER than the run: the manifest declares
        // the whole base directory, so whatever refuses below is the run policy.
        let mut profile = permissive_profile(&base);
        profile.read_paths = vec![base.to_string_lossy().into_owned()];
        let gate = adapter(&root);
        let denied = CapabilityBroker::new(&profile, &gate)
            .request(&HostRequest::ReadFile {
                path: secret.to_string_lossy().into_owned(),
            })
            .expect_err("a read outside the run roots must not be granted");
        assert!(
            denied.code.starts_with("policy."),
            "the RUN policy must be the refuser, not the ceiling: {denied:?}"
        );
    }

    /// Rule 2 of the seam: an approval-demanding action must NEVER come back as
    /// a grant. A guest cannot answer a prompt, so treating "needs approval" as
    /// "allow" would be a silent escalation available on every retry.
    #[test]
    fn an_approval_demanding_action_is_refused_not_deferred() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize");
        let gate = adapter(&root);
        // Prove the fixture is not vacuous: this destination really is
        // approval-gated by the default policy, not denied and not allowed.
        let action = ProposedAction::NetworkRequest {
            destination: "example.com:443".to_string(),
        };
        let decision = PolicyEngine::with_defaults()
            .evaluate(&action, &EvalContext::new(&root, &root))
            .decision;
        if decision == Decision::RequireApproval {
            let profile = permissive_profile(&root);
            let denied = CapabilityBroker::new(&profile, &gate)
                .request(&HostRequest::Connect {
                    host: "example.com".to_string(),
                    port: 443,
                })
                .expect_err("an approval-gated request must be refused, never granted");
            assert_eq!(denied.code, "policy.approval-required");
        } else {
            // The defaults deny outright rather than gate on approval. Assert
            // the refusal anyway — a grant would be wrong under either reading.
            let profile = permissive_profile(&root);
            let denied = CapabilityBroker::new(&profile, &gate)
                .request(&HostRequest::Connect {
                    host: "example.com".to_string(),
                    port: 443,
                })
                .expect_err("an un-allowed network request must be refused");
            assert!(denied.code.starts_with("policy."), "{denied:?}");
        }
    }

    /// The two requests with no honest lowering must refuse rather than
    /// approximate — an invented action in the audit ledger is worse than a
    /// refusal, and both refuse with their OWN code so an operator can tell
    /// which gate spoke.
    #[test]
    fn unsupported_requests_refuse_with_their_own_codes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize");
        let profile = permissive_profile(&root);
        let gate = adapter(&root);
        let broker = CapabilityBroker::new(&profile, &gate);
        let write = broker
            .request(&HostRequest::WriteFile {
                path: root.join("x").to_string_lossy().into_owned(),
            })
            .expect_err("sandboxed writes have no lowering");
        assert_eq!(write.code, "policy.unsupported-action");
        let secret = broker
            .request(&HostRequest::ReadSecret {
                name: "token".to_string(),
            })
            .expect_err("there is no secret broker");
        assert_eq!(secret.code, "policy.no-secret-broker");
    }

    /// An adapter with no lowering must refuse every hook rewrite. Fail-closed:
    /// a caller that forgets to install one loses rewrites rather than gaining
    /// unevaluated ones.
    #[test]
    fn hook_reentry_without_a_lowering_refuses() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let refused = PolicyReentry::evaluate(
            &adapter(tmp.path()),
            &ToolCall {
                name: "shell.run".to_string(),
                arguments_json: "{}".to_string(),
            },
        )
        .expect_err("no lowering means no evaluation");
        assert_eq!(refused, "policy.unknown-tool");
    }

    /// With a lowering installed, a rewritten call is evaluated afresh — and an
    /// approval-gated one comes back as `Ok(false)` ("allowed, but a human must
    /// approve"), which is what makes the sandbox crate demand a digest match
    /// instead of letting the rewrite inherit the original's approval.
    #[test]
    fn hook_reentry_reports_approval_separately_from_allow() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize");
        let repository = root.to_string_lossy().into_owned();
        let commit = ProposedAction::GitCommit {
            repository: repository.clone(),
        };
        assert_eq!(
            PolicyEngine::with_defaults()
                .evaluate(&commit, &EvalContext::new(&root, &root))
                .decision,
            Decision::RequireApproval,
            "the fixture action must actually demand approval, or this proves nothing"
        );
        let lowering: ToolCallLowering = Arc::new(move |name: &str, _args: &str| match name {
            "git.commit" => Some(ProposedAction::GitCommit {
                repository: repository.clone(),
            }),
            _ => None,
        });
        let adapter = adapter(&root).with_tool_lowering(lowering);

        assert_eq!(
            PolicyReentry::evaluate(
                &adapter,
                &ToolCall {
                    name: "git.commit".to_string(),
                    arguments_json: "{}".to_string(),
                }
            ),
            Ok(false),
            "an approval-gated rewrite must not report itself as allowed outright"
        );
        assert_eq!(
            PolicyReentry::evaluate(
                &adapter,
                &ToolCall {
                    name: "nonesuch.tool".to_string(),
                    arguments_json: "{}".to_string(),
                }
            ),
            Err("policy.unknown-tool".to_string()),
            "a tool the lowering does not know is refused, never guessed at"
        );
    }
}
