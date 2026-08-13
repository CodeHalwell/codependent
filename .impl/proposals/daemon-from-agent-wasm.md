# Proposal: daemon-side adapters for the sandbox capability seam

From **agent-wasm** (outcomes 12 + 13). Target: `crates/daemon` (new file
`crates/daemon/src/policy_gate.rs`, plus one `pub mod` line in
`crates/daemon/src/lib.rs`).

## Why this cannot live in `crates/sandbox`

`crates/daemon/Cargo.toml` already depends on `codypendent-sandbox`, so the
sandbox crate cannot depend on the daemon — a cycle. The WASM host and the hook
engine therefore define *seams* and the daemon implements them. This is the
whole architectural decision; the reasoning is in
`.impl/threat-models/12-executable-skills.md` §0.

`crates/daemon` is not in my ownership column, so this ships as a proposal.

## 1. `RunPolicyGate` — the skill/WASM capability adapter

`codypendent_sandbox::gate::RunPolicyGate` is the seam. Every privileged act a
WASM guest attempts is a `HostRequest`; the implementation lowers it into a
`ProposedAction`, evaluates it with the run's `PolicyEngine`, and only mints a
`GateGrant` from the `GateSeal` it was handed. **A `GateGrant` cannot be
constructed any other way** — `GateSeal`'s field is private to the sandbox
crate.

```rust
// crates/daemon/src/policy_gate.rs
use codypendent_protocol::ProposedAction;
use codypendent_sandbox::gate::{GateDenied, GateGrant, GateSeal, HostRequest, RunPolicyGate};

use crate::policy::{Decision, EvalContext, MergedPolicy, PolicyEngine};

/// Puts a sandboxed guest under the SAME deny-first policy every
/// model-proposed side effect passes. The package's own manifest is applied as
/// a ceiling by `CapabilityBroker` before this is called; this implementation
/// must NOT consult the manifest, or the second policy path comes back.
pub struct RunPolicyAdapter {
    engine: PolicyEngine,
    ctx: EvalContext,
    /// Approvals already resolved for this run, keyed by action digest. An
    /// approval is spent on exactly the action a human saw.
    approvals: std::collections::BTreeSet<String>,
}

impl RunPolicyGate for RunPolicyAdapter {
    fn authorize(&self, request: &HostRequest, seal: &GateSeal) -> Result<GateGrant, GateDenied> {
        let action = match request {
            HostRequest::ReadFile { path } => ProposedAction::ReadFiles { paths: vec![path.clone()] },
            HostRequest::WriteFile { .. } => {
                // No WASM host function performs writes today; refuse rather
                // than invent a WritePatch with no changeset behind it.
                return Err(GateDenied::new("policy.unsupported-action", "sandboxed writes are not supported"));
            }
            HostRequest::RunCommand { program, args } => ProposedAction::ExecuteCommand {
                program: program.clone(),
                args: args.clone(),
                // `cwd` etc. per the existing ExecuteCommand shape.
                ..Default::default()
            },
            HostRequest::Connect { host, port } => {
                ProposedAction::NetworkRequest { destination: format!("{host}:{port}") }
            }
            HostRequest::ReadSecret { .. } => {
                return Err(GateDenied::new("policy.no-secret-broker", "brokered secrets are not implemented"));
            }
        };
        let decision = self.engine.evaluate(&action, &self.ctx);
        match decision.decision {
            Decision::Allow => Ok(GateGrant::issue(seal, format!("run-policy@{}", decision.policy_version))),
            Decision::Deny => Err(GateDenied::new(
                decision.reasons.first().map(|r| r.code.clone()).unwrap_or_else(|| "policy.denied".into()),
                decision.reasons.first().map(|r| r.message.clone()).unwrap_or_default(),
            )),
            // A guest cannot block on an interactive prompt, so an approval that
            // was not already granted for THIS action is a refusal, not a wait.
            Decision::RequireApproval => Err(GateDenied::new(
                "policy.approval-required",
                "this action needs a human approval that has not been granted",
            )),
        }
    }
}
```

**Two rules the implementation must keep**, both testable:

1. Never inspect the package manifest here. The ceiling is `CapabilityBroker`'s
   job and applying it twice hides which gate refused.
2. Never return `Ok` for `RequireApproval` without an approval already bound to
   this exact action's digest. A guest running unattended must not be able to
   turn an approval prompt into an allow by retrying.

Wire it at the same place the run's `PolicyEngine`/`EvalContext` are built
(`crates/codypendentd/src/executor.rs` constructs both today), then pass
`Arc::new(adapter)` to `codypendent_knowledge::SkillRunner::enforcing(gate)`.

## 2. `PolicyReentry` — the hook rewrite adapter

`codypendent_sandbox::hook::PolicyReentry` is what a rewritten tool call is
re-evaluated through. `Unapproved<ToolCall>` has no accessor, so this is the
only way to get an executable call out of a hook rewrite.

```rust
impl codypendent_sandbox::hook::PolicyReentry for RunPolicyAdapter {
    fn evaluate(&self, call: &codypendent_sandbox::hook::ToolCall) -> Result<bool, String> {
        let action = crate::tools::proposed_action_for(&call.name, &call.arguments_json)
            .ok_or_else(|| "policy.unknown-tool".to_string())?;
        let decision = self.engine.evaluate(&action, &self.ctx);
        match decision.decision {
            Decision::Allow => Ok(true),
            // `false` = "allowed, but a human must approve it". The sandbox
            // crate then requires the approval digest to match THIS call.
            Decision::RequireApproval => Ok(false),
            Decision::Deny => Err(decision.reasons.first().map(|r| r.code.clone())
                .unwrap_or_else(|| "policy.denied".into())),
        }
    }
}
```

The `ReentryContext` handed alongside it must carry **only**
`approved_digest: Option<String>` — the digest of the action a human actually
saw this turn. Do not add the original call's decision, grant, or approval id:
the absence of those fields is what stops a rewrite inheriting authority.
