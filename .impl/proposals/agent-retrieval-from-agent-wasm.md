# Proposal: the two call sites that make outcomes 12 and 13 reachable

From **agent-wasm**. Target: `crates/runtime/src/agent.rs` and
`crates/runtime/src/tools/**` (agent-retrieval's files).

Everything below this line is built and tested in `crates/sandbox` and
`crates/knowledge`; what is missing is the caller.

## 1. `skills.run` — the tool that makes an executable skill executable

The 2026-08-13 review's highest-leverage finding (12.3) was that nothing in the
product calls the skill executor. `codypendent_knowledge::SkillRunner` is now
that entry point and enforces every precondition itself (Active status,
`executable` flag, package content-hash re-verification, entrypoint containment,
manifest `[limits]`, `$REPOSITORY`/`$WORKTREE` substitution). A tool only has to
resolve the registry item and call it.

```rust
// crates/runtime/src/tools/skill_run.rs
use codypendent_knowledge::{PlaceholderContext, SkillInvocation, SkillRunner};

// {"skill": "rust.fix-ci", "script": "scripts/fix.sh", "args": ["--check"]}
// or {"skill": "rust.fix-ci", "module": "skill.wasm", "input": "…"}
let item = registry.by_identity(&pool, RegistryItemKind::Skill, &req.skill, &scope).await?
    .ok_or_else(|| tool_error("no such skill"))?;
let ctx = PlaceholderContext::new(repository_root, worktree_root)?;
let outcome = runner.run(&item, &invocation, &ctx)?;
// `outcome.audit_summary()` is safe to log; the captured output is already
// sanitized and origin-labeled, so it enters context as evidence, not text.
```

**Policy**: `skills.run` must NOT be auto-allowed. A skill's script inherits its
`commands` grant, so the natural `ProposedAction` for it is the
`ExecuteCommand` the script's own program implies — i.e. it goes through the
existing approval gate rather than being added to the always-allow list beside
`SearchRegistry`.

**Degraded backends**: render `runner.capability_diagnostic()` when a run is
refused, and at `skill add` time. This is the `CapabilityReport` the review
found built, tested, documented, and never rendered (12.6) — on a host without
`bwrap`, it turns a mystery `ToolUnavailable` into a legible install-time
message.

## 2. The hook dispatch seam

`crates/sandbox/src/hook.rs` has the parser, the verdict lattice, and the
`Unapproved`/`Authorized` type wall. It has no dispatch site, so **no hook can
fire today** — the correct fail-closed state. Wiring it up needs an event
emission point in the agent loop's existing policy→approval→execute path
(`crates/runtime/src/agent.rs:2694` area).

```rust
// Immediately BEFORE the existing policy evaluation for a tool call.
let subject = ToolCall { name: tool_name.clone(), arguments_json: args_json.clone() };
match hooks.dispatch(HookEvent::ToolPre, &subject, depth)? {
    HookOutcome::Proceed => { /* fall through to the existing path, unchanged */ }
    HookOutcome::Denied { reasons } => return Err(blocked_by_hook(reasons)),
    HookOutcome::Rewritten(unapproved) => {
        // The rewritten call re-enters policy from scratch. There is no API that
        // lets it inherit the original's decision or approval — `Unapproved` has
        // no accessor, and `ReentryContext` has no field to inherit from.
        let authorized = unapproved.reenter(&policy_adapter, &ReentryContext {
            approved_digest: current_turn_approved_digest.clone(),
        })?;
        let call = authorized.into_inner();
        // Then run the ORDINARY path on `call` — policy, approval, execute.
        // Do not skip any of it because a hook "already approved" it.
    }
}
```

Three invariants to preserve when wiring, each already unit-tested on the
sandbox side:

1. `depth > 0` disables dispatch entirely. A hook must not be able to cause the
   event it fires on to be re-dispatched.
2. Hooks are ordered by `HookSpec::dispatch_key()` — `(priority, id)` — never by
   directory enumeration order.
3. A repository-scoped hook is inert until a human approves it, and the approval
   binds to the hook's content hash (migration `0027_hooks.sql`). Discovery is
   not activation.

The full reasoning, including why a rewrite cannot become a privilege
escalation, is in `.impl/threat-models/13-hooks.md`.
