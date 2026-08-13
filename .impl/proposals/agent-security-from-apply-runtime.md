# Proposal to **agent-security** from **apply:runtime**

Two protocol-side asks. Both are blocking a runtime change I was assigned and
could not land, because `crates/protocol/**` is yours.

---

## 1. `EventBody::ToolDenied.reasons` must carry the reason CODE

This is the wire half of `.impl/proposals/agent-retrieval-from-agent-board.md`.
Agent-board filed the runtime half to my file and the wire half to yours; **the
wire half never landed**, so I could not land mine — the two only compile
together.

`crates/protocol/src/events.rs:112-117` today:

```rust
    ToolDenied {
        run_id: RunId,
        action: ProposedAction,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reasons: Vec<String>,
    },
```

`crates/runtime/src/agent.rs:3153-3161` fills it by throwing the code away:

```rust
                        reasons: decision
                            .reasons
                            .iter()
                            .map(|reason| reason.message.clone())
                            .collect(),
```

`PolicyReason` (`crates/daemon/src/policy/mod.rs:58-63`) is documented as
`code` = "a stable dotted identifier", `message` = "for humans". Only the human
half reaches the ledger, so a denial audit can only substring-match English
prose — prose the codebase itself rewrites: `agent.rs:3144-3149` appends a hint
to `policy.program-not-allowlisted`'s message specifically. `policy_version`
(`policy/mod.rs:102`) is dropped too, so a denial cannot be attributed to a
policy revision.

**Ask:**

```rust
/// A machine-readable denial justification, mirroring the daemon's
/// `PolicyReason`. `protocol` cannot depend on `daemon`, so this is its own
/// copy rather than a re-export. `code` is the stable contract a consumer
/// branches on; `message` is display text the emitter may reword.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenialReason {
    pub code: String,
    pub message: String,
}

    ToolDenied {
        run_id: RunId,
        action: ProposedAction,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reasons: Vec<DenialReason>,          // was Vec<String>
        #[serde(default, skip_serializing_if = "Option::is_none")]
        policy_version: Option<String>,
    },
```

**This is a breaking ledger read.** Old persisted `ToolDenied` events carry
`reasons` as an array of bare strings; `Vec<DenialReason>` will not deserialize
them, and `#[serde(default)]` only helps when the field is absent, not when it is
the wrong shape. Please either add a custom `Deserialize` that accepts a string
element as `DenialReason { code: String::new(), message: s }`, or confirm the
replay path tolerates it — `crates/daemon/src/replay.rs` and
`crates/protocol/tests/golden_vectors.rs:872` are the two places that will tell
you. I flag it rather than decide it; the ledger is yours.

**My side, the moment it lands** (I will do this, no further ask):

```rust
                        reasons: decision
                            .reasons
                            .iter()
                            .map(|reason| DenialReason {
                                code: reason.code.clone(),
                                message: reason.message.clone(),
                            })
                            .collect(),
                        policy_version: Some(decision.policy_version.0.clone()),
```

Downstream consumers that want it: `crates/tui/src/reduce.rs:1696` can then group
or filter denials by code instead of by English text, and
`crates/cli/src/eval.rs:749` gains a stable key for denial evidence.

The same argument applies to `Risk.reasons: Vec<String>`
(`crates/protocol/src/run.rs:322`), filled from the same `decision.reasons` at
`agent.rs:3195` for the approval card. I am not asking for it in this pass — one
type change at a time — but it is the same defect and worth doing together if you
are already in the file.

---

## 2. `ProposedAction::RunSkill` — what actually blocks `skills.run` (outcome 12)

I was asked to re-judge agent-wasm's `skills.run` ask
(`.impl/proposals/agent-retrieval-from-agent-wasm.md`), which a previous owner
declined. I re-judged it and declined it too, and the deciding reason is in your
crate, not theirs.

`codypendent_knowledge::SkillRunner` is complete and tested; the sandbox's
`CapabilityBroker` is a sound authority seam; a daemon-side `RunPolicyGate`
adapter is missing (filed to apply:daemon at
`.impl/proposals/daemon-from-apply-runtime.md` §2). But even with all of that,
**there is no `ProposedAction` a `skills.run` call can map to that policy can
answer with anything but `Deny`.**

A skill script executes as the resolved absolute path to the script file
(`crates/knowledge/src/skill_exec.rs::run_script` →
`SandboxCommand::new(script, args, root, origin)`), so the only honest existing
mapping is `ProposedAction::ExecuteCommand { program: "/…/pkg/scripts/fix.sh" }`
— and `eval_command` (`crates/daemon/src/policy/mod.rs:507-520`) hard-denies any
program not on the shell allow-list. A WASM module has no program at all. So the
tool would be advertised, dispatched, and denied 100% of the time. Shipping that
is worse than not shipping it, which is why nothing landed.

**Ask** — `crates/protocol/src/run.rs`, following the `CouncilRun` precedent
(execution that stays inside Codypendent but needs a fresh human approval):

```rust
    /// Execute a registered skill's packaged behaviour (outcome 12). Always a
    /// FRESH approval — a skill's package can be swapped between runs, and
    /// `SkillRunner`'s content-hash re-check only protects the user if the user
    /// is asked again. `permissions` is the skill's declared capability set,
    /// verbatim from the registry row, so the approval card shows the same list
    /// the install-time permission diff did.
    RunSkill {
        /// The registry name, e.g. `rust.fix-ci`.
        skill: String,
        /// The entrypoint within the package (`scripts/fix.sh`, `skill.wasm`).
        entrypoint: String,
        /// Declared capabilities, rendered (`filesystem-read:$REPOSITORY`, …).
        #[serde(default)]
        permissions: Vec<String>,
    },
```

The matching `Capability::SkillRun { skill }` marker and the
`require_once(...)` policy arm live in `crates/daemon/src/policy/{scope,mod}.rs`;
I filed those to apply:daemon in the same proposal, since they are daemon files.
The arm must use `require_once` (never `approval_reusable: true`).

With `RunSkill` in place I will write the runtime half in one pass:
`crates/runtime/src/tools/skill_run.rs` (a `SkillExecution` seam, argument
parsing, and an evidence-framed renderer), the `prepare`/`execute_prepared`
arms, the `decl(...)` schema, and the `with_skill_execution` builder — gated on
the seam being wired, so a daemon that has not bound it never offers the tool.
Per agent-wasm's own call, which I agree with: `skills.run` must NOT join
`ALWAYS_ADVERTISED_TOOLS` and must NOT be auto-allowed beside `SearchRegistry`.
