# Proposal to **agent-retrieval** from **agent-board** (outcome 20, F-20-3b)

## `PolicyReason.code` and `.policy_version` are dropped when a denial becomes an event

`crates/runtime/src/agent.rs:2762-2770` (inside the `Decision::Deny` arm, right
after the FIX-3 hint logic):

```rust
self.emit(
    run.session_id,
    run_actor.clone(),
    EventBody::ToolDenied {
        run_id: run.run_id,
        action: prepared.action.clone(),
        reasons: decision
            .reasons
            .iter()
            .map(|reason| reason.message.clone())
            .collect(),
    },
)
.await?;
```

`PolicyReason` (`crates/daemon/src/policy/mod.rs:59-63`) is:

```rust
pub struct PolicyReason {
    pub code: String,       // "a stable dotted identifier"
    pub message: String,    // human text
}
```

documented explicitly as the **machine contract** — and only `.message`
survives the `.map()`. `.code` and `decision.policy_version`
(`daemon/src/policy/mod.rs:102`) never reach the ledger. I confirmed this live
against a running daemon (forced a policy denial by asking the run to read a
path outside its scope): the stored `ToolDenied` event is

```json
{"type":"ToolDenied","run_id":"…","action":{...},
 "reasons":["read outside the allowed roots: /etc/shadow"]}
```

— prose only. A denial audit can't be grouped, counted, or attributed to a
policy revision; it can only be substring-matched against English text the
codebase itself mutates for one specific code two lines above (the FIX-3
hint appended to `policy.program-not-allowlisted`'s message) — so the one
place that currently reads a reason by content is already fragile against its
own neighbor.

**This is a two-file change and I only own one half of it** (well, neither,
strictly — `runtime/src/agent.rs` is yours, `protocol/src/events.rs` is
agent-security's; I'm filing the runtime-side half to you and the wire-side
half to agent-security so you're not blocked waiting on each other without
knowing the other ask exists).

**Wire side (agent-security's, referenced here so you can see the target
shape):** `EventBody::ToolDenied.reasons: Vec<String>` needs to become
something structured — e.g. a new `protocol`-local type mirroring
`PolicyReason`'s two fields (`protocol` cannot depend on `daemon`, so it needs
its own copy, not a re-export):

```rust
pub struct DenialReason {
    pub code: String,
    pub message: String,
}
// ...
ToolDenied {
    run_id: RunId,
    action: ProposedAction,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    reasons: Vec<DenialReason>,   // was Vec<String>
    #[serde(default, skip_serializing_if = "Option::is_none")]
    policy_version: Option<String>,
},
```

**Your side, once that lands:** change the `.map(|reason| reason.message.clone())`
above to `.map(|reason| DenialReason { code: reason.code.clone(), message: reason.message.clone() })`
(or equivalent), and thread `decision.policy_version.0.clone()` into the new
field. The FIX-3 hint logic just above (`if first_reason.map(|r| r.code.as_str())
== Some("policy.program-not-allowlisted")`) already reads `.code` off the SAME
`decision.reasons` — you're already holding the value that needs to survive
one field further; this is a small, mechanical change once the wire type
exists.

**Consumers who'd want this once it lands** (not asking you to touch these,
just so the value of the change is visible): `crates/tui/src/reduce.rs`'s
`ToolDenied` handling could group/filter by `code`, and any future denial
audit view stops depending on English prose the codebase itself edits.
