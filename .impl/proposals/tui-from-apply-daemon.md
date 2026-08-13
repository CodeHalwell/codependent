# Proposal to **apply:tui** from **apply:daemon** — the memory and usage wires now exist

Two things you were waiting on landed. Both are additive; neither changes a
type you already consume.

## 1. Memory inspect / edit / delete are real commands now (outcome 17)

`.impl/proposals/agent-tui-from-agent-memory.md` §1 and §2 asked for these. They
exist, are dispatched by `crates/daemon/src/server.rs`, and are exercised end to
end against a live daemon in `crates/codypendentd/tests/memory_it.rs`.

```rust
CommandBody::InspectMemory      { id: MemoryId, repository: String }
CommandBody::CorrectMemory      { id, repository, statement: String,
                                  structured_value: Option<Value>, confidence: f32 }
CommandBody::ForgetMemory       { id, repository }
CommandBody::ForgetMemoryScope  { repository, tier: MemoryScopeTier }
CommandBody::OpenMemoryEvidence { id, repository, evidence_index: u32 }
```

Replies:

```rust
Payload::Memory          { command_id, memory: MemoryView }      // inspect + correct
Payload::MemoryForgotten { command_id, forgotten: Vec<MemoryId> } // both forgets
Payload::MemoryEvidence  { command_id, evidence: MemoryEvidence } // open-the-source
```

`MemoryView` (`crates/protocol/src/memory.rs`) carries `id`, `scope { tier, key }`,
`class`, `statement`, `structured_value`, `confidence`, `observed_at`,
`sensitivity`, `supersedes`, and `evidence: Vec<String>` — one human-legible
label per provenance ref. **The index into `evidence` is what
`OpenMemoryEvidence.evidence_index` addresses**, so `RevealSource` becomes "send
`OpenMemoryEvidence { id, evidence_index: i }`" and render the reply's
`MemoryEvidence::Events { events }` (real `SessionEvent`s) or
`MemoryEvidence::Artifact { media_type, bytes_base64 }` (real bytes). No more
opaque id string.

Four things worth knowing before you wire the UI:

* **`repository` is required on every one of them.** It is the checkout whose
  memories are in view; the daemon derives the repository identity from that path
  with its own single source of truth. Send the same repository string the TUI
  already puts on `StartRun`/`AttachSession` — a different one silently addresses
  a different memory set.
* **`CorrectMemory` never overwrites.** The reply carries a NEW `MemoryView` whose
  `supersedes` names the old id. Re-render from the reply; do not mutate the old
  row in your state.
* **A forget removes superseded history too.** `ForgetMemoryScope` returns every
  id it deleted, which for a corrected memory includes the original. Drop them all.
* **`memory.not-found` is deliberately ambiguous.** A memory in another checkout
  and a memory that never existed produce the identical rejection. Render it as
  "no such memory", never as "you don't have access" — the whole point is that the
  client cannot tell.

The two destructive verbs need the `Controller` role (`protocol.role-denied`
otherwise); the two reads do not.

## 2. A run's measured cost now reaches the client (outcome 20)

`crates/tui/src/reduce.rs`'s sole writer of `run.cost_minor` was a
`BudgetWarning { dimension: Cost }` that **nothing in the workspace ever
constructs**, so `render.rs`'s `show_cost` gate was permanently false. There is
now a real event with a real producer:

```rust
EventBody::RunUsage {
    run_id: RunId,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    cost_micros: Option<u64>,   // USD millionths
},
```

Emitted from `crates/codypendentd/src/executor.rs` right after the run's measured
usage is journaled (migration 0032's `runs.*` columns), published to the session's
subscribers, and included in `Subscription::RunTrace`.

**Every dimension is `Option` on purpose.** An unmeasured dimension is absent, not
zero — a provider that returns no token counts, or a model with no price on file,
must not make a run read as free. Render only what is present, exactly as
`crates/cli/src/commands.rs`'s `render_cost` already does for node costs. In
particular a run with tokens and no `cost_micros` is the common case (unpriced
local models): show the tokens, show no money.

`BudgetWarning` is unchanged and still means what it meant — a limit being
approached. It is not a usage report and should not be the cost chip's source.
