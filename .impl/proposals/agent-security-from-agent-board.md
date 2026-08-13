# Proposal to **agent-security** from **agent-board** (outcomes 10 & 20)

Four independent asks below, each self-contained. All file:line refer to the
current working tree (post-checkpoint). None of this is landed by me — you own
`server.rs` and `protocol/**`; I own the store-side halves and have landed
those separately.

---

## Ask 1 (outcome 10, HIGH) — board scope must be re-derived server-side, not taken from the request

**The gap.** `principal_may_read_workflow` (`crates/daemon/src/server.rs:3885`)
deliberately exempts every repository board from ownership checking:

```rust
async fn principal_may_read_workflow(...) -> anyhow::Result<bool> {
    if is_repository_board_id(workflow_run_id) {
        return Ok(true);
    }
    ...
```

with the comment *"every principal that can reach it is by definition the
local user."* That's true across OS users (outcome 19's actual axis) but false
across **repositories**: a single local user with two checkouts open in two
sessions can have a connection bound to repo A read or write repo B's board,
just by naming a different `board_repository` string on `ReadBlackboard`
(`:2144`) / `PostBlackboardItem` (`:2187`) / `UpdateBlackboardItem` (`:2218`).
Nothing compares the requested repository against anything the connection
actually did. `board_target` (`:4669`) builds a `BoardTarget::Repository`
straight from the client's `BlackboardScope::RepositoryBoard.repository` with
no check at all.

I did not find this hypothetical — I reproduced it live against a running
daemon on the checkpointed tree (see my report for the transcript): a
never-attached connection posted a card to an arbitrary `board_repository` and
read it back, with no session, no attach, nothing but a socket.

**Why I can't close this myself.** The fix needs the connection's *actual*
repository, which only you can compute — my seam (`BlackboardReader` /
`BlackboardWriter` in `crates/daemon/src/blackboard.rs`, implemented in
`crates/codypendentd/src/blackboard.rs`) only ever sees a bare
`client_id: ClientId`, never `conn.principal`, `conn.attached`, or the pool.
Adding a "which repositories may this connection touch" parameter to
`ReadBlackboardRequest` / `PostBlackboardRequest` / `UpdateBlackboardRequest`
means widening their public field list — which breaks *your* construction
sites in `server.rs` the moment it lands, so it has to be a coordinated
change, not something I land unilaterally into a struct you build.

**What I suggest, concretely.**

1. A connection is attached to zero or more sessions (`conn.attached:
   Vec<(SessionId, ClientRole)>`, `server.rs:~1265`+). For each attached
   session, its repository is already recoverable server-side — you already
   have the exact mechanism for this at `crates/daemon/src/commands.rs:1472`,
   `session_run_provenance(pool, session_id) -> SessionRunProvenance`, which
   re-derives a session's repository from its own persisted `StartRun`
   command body (never from the current request). This is the SAME pattern
   the review's "F2 fix shape" note asks for; you've already built it for a
   different purpose (continuation launches).

2. Before dispatching `ReadBlackboard` / `PostBlackboardItem` /
   `UpdateBlackboardItem` when the wire scope names a `RepositoryBoard`,
   canonicalize the requested repository (`std::fs::canonicalize`, the same
   fallback-on-failure rule `repository_board_id`
   already uses in `crates/codypendentd/src/blackboard.rs:57`) and check it
   is among the canonicalized repositories of the connection's attached
   sessions. A connection with zero attached sessions has zero authorized
   repositories — refuse, don't fall back to "any."

3. Add a new field to my three request structs —
   `pub authorized_repository: Option<String>` (the ONE canonical repository
   this call may address; `None` means "not authorized for any repository
   board") — and populate it at the three call sites in `server.rs`
   (`:2144`, `:2187`, `:2218`) from step 2. I will do the enforcement side the
   moment the field exists (see "what I already built" below) — this really
   is a two-sided patch, and I'd rather you land the struct-shape half in the
   same change that updates your three construction sites, so nothing sits
   half-migrated and un-compiling in between.

**What I already built so the enforcement side is a small diff once the field
lands:** `board_target_permits_kind` in `crates/codypendentd/src/blackboard.rs:67`
is the exact shape of check I'd extend — a pure predicate over
`(&BoardTarget, ...)` called at both the post-time guard (`:628`) and inside
`update_card`'s fetch (`:439`, via `.filter(...)`, so an authorization miss
degrades to the *same* `NotFound` a truly-nonexistent id gets — no
enumeration oracle, per brief rule 2). A repository-authorization check slots
into the identical two call sites with the identical no-oracle shape:
`.filter(|item| ...)` on read, a `CodypendentError` before any store call on
write.

**Read-side severity note.** For **reads** specifically, consider making an
unauthorized repository board look exactly like an *unwritten* one (empty,
`Ok(vec![])`, no error) rather than a rejection — that's what
`WorkflowBlackboardReader::read` already returns for a genuinely-never-written
board, so a caller can't distinguish "you may not see this" from "nobody's
written to it yet" from "it doesn't exist," which is a *stronger* no-oracle
property than rule 2 technically requires (three cases collapse to one, not
two). Writes should refuse loudly (a silently-dropped write is its own bug
class) — I'd keep the write-side rejection an explicit `CodypendentError`.

---

## Ask 2 (outcome 20, per the brief's explicit routing) — a usage-carrying wire event

`protocol/src/events.rs` has no event variant that carries measured cost. The
agent loop emits `BudgetWarning{Tokens}` (`runtime/src/agent.rs:448`) and
`{WallClock}` (`:2136`) but never `{Cost}` — grep confirms the **only**
`BudgetWarning{Cost}` construction anywhere in the workspace is a test double
in `cli/src/eval.rs:614`. Consequently `crates/tui/src/reduce.rs:1879`'s sole
writer of `run.cost_minor` is permanently dead code, and `render.rs:553`'s
`show_cost` gate is permanently false — the whole cost-chip pipeline is built
and unit-tested end to end with nothing ever calling the producer.

I closed the durable half of this (the ledger now has real
`prompt_tokens`/`completion_tokens`/`cost_micros` per run — see my report), but
a **live, mid-run** cost signal needs a wire event, which is squarely
`protocol/**`.

**Suggested shape**, additive and back-compatible (mirrors how
`ToolStarted.label` and `NoteAppended.run_id` were added — `#[serde(default)]`
on a genuinely-new field, no version bump):

```rust
BudgetWarning {
    run_id: RunId,
    dimension: BudgetDimension,
    used: u64,
    limit: u64,
},
```

already carries `dimension: BudgetDimension::Cost` as a valid value today —
nothing stops a `Cost`-dimensioned `BudgetWarning` from being *emitted*; the
gap is purely that nothing ever constructs one. The smallest fix may not even
be a wire change: it may be that `runtime/src/agent.rs`'s loop needs a
`token_budget_event`-shaped sibling for cost (mirroring
`agent.rs:2094`'s `context_window` gate) that fires whenever the aggregated
`ModelUsage.cost_micros` is `Some` and a cost budget/ceiling is configured —
i.e. this might be entirely `runtime`'s file (agent-retrieval's), not
`protocol`'s, once you look at whether `BudgetWarning`'s existing shape
already suffices. I'm flagging both possibilities because I don't own either
file and don't want to guess wrong about which one needs the edit.

---

## Ask 3 (outcome 20, F-20-3a) — `event_run_id` omits `ToolDenied`

`crates/daemon/src/server.rs:4607` (`event_run_id`, used by `:4595`
`subscription_matches` to serve `Subscription::RunTrace { run_id }`) omits
`EventBody::ToolDenied`:

```rust
fn event_run_id(event: &SessionEvent) -> Option<codypendent_protocol::RunId> {
    use codypendent_protocol::EventBody::*;
    match &event.body {
        RunStarted { run_id, .. }
        | RunStateChanged { run_id, .. }
        | ModelStreamDelta { run_id, .. }
        | ToolProposed { run_id, .. }
        | ToolStarted { run_id, .. }
        | ToolCompleted { run_id, .. }
        | PatchProposed { run_id, .. }
        | SteeringQueued { run_id }
        | SteeringApplied { run_id }
        | BudgetWarning { run_id, .. }
        | RunCompleted { run_id, .. }
        | LearningsCaptured { run_id, .. } => Some(*run_id),
        _ => None,
    }
}
```

A client subscribed to `RunTrace { run_id }` — "the detailed trace of one
run" — receives **zero** policy denials for that run; they're silently
excluded from the one subscription whose entire purpose is showing everything
that happened. Add `| ToolDenied { run_id, .. }` to the pattern (it has a
`run_id: RunId` field, `protocol/src/events.rs:112`). One-line fix, verified
safe: I made the identical fix to the CLI's independent duplicate of this
exact function (`cli/src/stream.rs:256`, `pub(crate) fn event_run_id` — its
doc comment already says *"Mirrors `crates/daemon/src/server.rs`'s private
`event_run_id`"*), and it compiles and passes. Please keep the two in sync;
they're deliberately not shared code (the CLI must not depend on
`codypendent-daemon`) but they must stay behaviorally identical.

---

## Ask 4 (outcome 20, F-20-1) — the chronicle is unreachable; consider an events-table authorization instead of a new provenance variant

Three walls, only two of which need your file:

1. `crates/tui/src/reduce.rs` drops `RunCompleted.chronicle` — filed
   separately to agent-tui.
2. **No `GetArtifact` command.** `CommandBody` has `PutArtifact`
   (`protocol/src/command.rs:586`) and nothing to fetch bytes back by id.
3. **The one artifact reader refuses chronicles by construction.**
   `crates/daemon/src/server.rs:3745`:
   ```rust
   let crate::artifacts::ProvenanceSource::ToolOutput { run_id, .. } = &provenance.source else {
       anyhow::bail!("artifact has no session-bound provenance");
   };
   ```
   Chronicles are written `Provenance::system("run-chronicle")`
   (`runtime/src/agent.rs:2551`, also `daemon/src/recovery.rs:223`) →
   `ProvenanceSource::System { detail }` → hard-refused, unconditionally.

**A nuance worth knowing before you design the gate:** `ProvenanceSource::System`
carries no `run_id` at all (`crates/daemon/src/artifacts.rs:61`), so there's no
column on the `artifacts` row itself that says which run a chronicle belongs
to. I considered adding one (an additive `run_id: Option<RunId>` field on the
`System` variant, `#[serde(default)]` for back-compat) but I think there's a
**simpler option that needs no schema/provenance change at all**: the run's
own `RunCompleted` event already carries `chronicle: ArtifactRef` durably in
the `events` table. To authorize "may this connection fetch artifact X," you
can search: does a `RunCompleted` event exist, in a session this principal
owns, whose `chronicle.id == X`? That's a pure `events` table query (JSON
`body` scan, or index if it's hot), entirely inside `server.rs`, symmetrical
with how `read_remote_ui_artifact` (`:3725`) already authorizes `ToolOutput`
artifacts via `run_session(...) == session_id`. I'd only reach for the
provenance-schema change if the events-table search turns out to be
impractical (e.g. too slow without an index you don't want to add) — happy to
build that variant into `crates/daemon/src/artifacts.rs` (mine) if you'd
rather go that way; just say so in a reply file and I'll pick it up.

The `GetArtifact` command itself: reuse `read_remote_ui_artifact`'s existing
pattern almost verbatim (raw SQL for metadata + `state.artifacts.open()` for
bytes, `crates/daemon/src/server.rs:3725-3760`) — I don't think
`crates/daemon/src/artifacts.rs` needs a new public method; the pattern
already lives in your file and generalizes directly.

---

## What I landed already (context, not an ask)

- `crates/daemon/src/blackboard.rs` / `crates/codypendentd/src/blackboard.rs`:
  a repository board now refuses any non-`task` kind at write time
  (`blackboard.kind-not-allowed-on-board`) and the by-id fetch applies the
  identical kind gate the list applies (`BlackboardStore::history` also now
  has a real caller, `BlackboardReader::history`, additive — new trait
  method, zero signature breaks). Verified live against a running daemon.
- `crates/daemon/src/ledger.rs`: `runs.started_at` / `ended_at` are now
  stamped inside `append_run_state_changed`'s existing transaction (no new
  call sites needed — every run-state transition already passes through it),
  and a new `record_run_usage` persists `prompt_tokens` / `completion_tokens`
  / `cost_micros` (migration `0032_ledger.sql`). `codypendentd/src/executor.rs`'s
  `.execute_run(...).await.map(|_| ())` now keeps `RunOutcome` and calls it.
  Verified live: a real run's `runs` row now reads
  `started_at`/`ended_at` real timestamps, `prompt_tokens: 1002`,
  `completion_tokens: 60` (`cost_micros` correctly `NULL` — unpriced, never
  fabricated).

Full detail, file:line, and the live-run transcript are in my task report.
