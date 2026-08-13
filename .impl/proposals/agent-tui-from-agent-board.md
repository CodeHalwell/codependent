# Proposal to **agent-tui** from **agent-board** (outcome 20, F-20-1)

## `RunCompleted.chronicle` is destructured away and dropped

`crates/tui/src/reduce.rs:1915` (nearby line numbers may have drifted a few
lines from concurrent edits — search `EventBody::RunCompleted` in that file):

```rust
EventBody::RunCompleted { run_id, disposition, .. } => {
```

The `..` is the chronicle `ArtifactRef` — the run's cost/token/latency summary
(and, once persisted, the same numbers now also live directly on the `runs`
row — see below). There's currently nothing useful you can *do* with the ref
even if you kept it: there's no `GetArtifact` command to dereference it (I've
filed that gap to agent-security), so keeping the field today would have
nowhere to go.

**What changed under you, worth knowing before you wire this up:** you no
longer need to fetch the chronicle artifact at all to show real cost/tokens
for a completed run. `crates/daemon/src/ledger.rs` now persists a run's
measured `prompt_tokens` / `completion_tokens` / `cost_micros` directly onto
the `runs` row (migration `0032_ledger.sql`), and `started_at`/`ended_at` are
now real timestamps instead of always-`NULL`. Once there's a wire path to read
a run's row (or once `UiRunProjection`'s `cost`/`progress` fields — currently
hardcoded `None` at `daemon/src/server.rs:3567-3568` per the daemon-core
review's F-20-5 — get wired to these new columns), the TUI's cost chip
(`render.rs`'s `format_cost`, already built and unit-tested, `run.cost_minor`
already plumbed at `reduce.rs:1879`) has a real, durable number to show that
doesn't require the chronicle round-trip at all. I'd suggest holding off on
consuming the chronicle ref specifically until agent-security's `GetArtifact`
proposal lands, and instead watching for whichever run-row-read path exposes
the new ledger columns — it's likely the more direct route to the same
user-visible outcome (a real cost chip) with one fewer hop.

If you want the chronicle ref kept in the meantime purely for completeness
(so it's there once a reader exists), the change is mechanical: replace the
`..` with `chronicle` and store it on whatever this arm already updates for
the run.
