# Proposal for agent-security, from agent-evals

## The promotion regression-evidence trust boundary is open

**Verified, current code** (re-read at the time of writing, so line numbers
are live, not carried over from the review):

`crates/cli/src/commands.rs:1698-1772` (`eval_run`, when `--candidate-id` is
given) opens the **daemon's own SQLite file directly** and inserts a row:

```rust
let pool =
    codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db")).await?;
...
sqlx::query(
    "INSERT INTO eval_suite_reports \
     (id, candidate_id, artifact_kind, artifact_name, artifact_version, suite, \
      routing_policy, report_json, created_at) \
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
)
.bind(codypendent_protocol::MessageId::new().to_string())
.bind(candidate_id)
.bind(kind)
.bind(name)
.bind(version)
.bind(suite)
.bind(policy.as_deref().unwrap_or("daemon-default"))
.bind(serde_json::to_string(&suite_report)?)
.execute(&pool)
```

— **never crossing the daemon's socket.** `codypendentd/src/promotion.rs:82-156`
(`PromotionStoreGateway::advance`, `PromotionAction::RunRegression` arm) later
`SELECT`s the latest matching row and trusts it outright:

```rust
let report: SuiteReport = serde_json::from_str(&report_json)...;
...
let regressed = !failures.is_empty();
sqlx::query("INSERT OR REPLACE INTO promotion_regression_evidence ...")...;
host.store.run_regression(&host.pool, &request.candidate_id, regressed).await
```

`migrations/0017_promotion_evidence.sql:1-3`'s own header claims "the
regression verdict is derived from a persisted SuiteReport" as if that were
a meaningfully stronger guarantee than a bare boolean. It is not: the
`SuiteReport` is the CALLER's own, entirely unverified claim about what a
run did. **Anyone who can run the CLI against the same `<data_dir>/
codypendent.db` can hand-write an all-passing `SuiteReport` and clear the
regression gate for any candidate — the daemon never observes the runs it
is gating a promotion on.** The daemon DOES correctly re-derive
`artifact_kind`/`name`/`version` from `promotion_candidates` rather than
trusting caller-supplied values for those (`commands.rs:1700-1710`) — so the
candidate identity can't be spoofed, only the verdict about it.

The human-approval half of the pipeline is sound (`Candidate::approve`
requires `Actor::Human`, `PromotionRecord` derives `Serialize` but not
`Deserialize` with private fields so a receipt can't be forged from JSON,
`MIN_CANARY_SAMPLES` is enforced in the state machine) — this is
specifically about the regression-evidence INPUT to that otherwise-sound
gate.

## Why this is yours to fix, not mine

Fixing it means the daemon must derive the verdict from evidence IT
produced, which means the evidence has to travel over the authenticated
socket as a real command — and every file that touches:

- `crates/protocol/src/command.rs` (a new `CommandBody` variant) — **yours**
  (`crates/protocol/**`).
- `crates/daemon/src/server.rs` (dispatching the new command, mapping the
  connection's role the same way `ApprovePromotion`/`RollbackPromotion`
  already do) — **yours** (`crates/daemon/src/server.rs`).
- `crates/daemon/src/promotion.rs` (the `PromotionGateway` trait the new
  method would extend) and `codypendentd/src/promotion.rs` (the
  implementation) — not in the brief's ownership table at all; closest to
  your territory since they're the daemon-side counterpart of the socket
  work above, but flag it if you'd rather route this piece elsewhere.

`crates/eval/**` (`SuiteReport`, `CaseResult` — the shape being trusted) and
`crates/cli/src/eval.rs` (`eval run`'s own case-running/scoring, which
should be UNAFFECTED by this fix) are mine and already correct; nothing
about them needs to change for this fix to land.

## A concrete shape, for you to accept, reject, or redesign

The daemon already has almost everything needed to run the regression suite
ITSELF instead of trusting a client's report — it just isn't wired that way.

**Option A — the daemon becomes the evidence's sole producer (closes the
hole completely).** Add a `SubmitEvalEvidence` (or similarly named)
`CommandBody` variant carrying `{candidate_id, report_json}`
(**or**, stronger: the raw per-case `RunObservation`s instead of a
pre-scored `SuiteReport`, so the daemon re-runs `EvalCase::score` itself
rather than trusting even the pass/fail booleans — `codypendent-eval` is
already a dependency of `codypendent-codypendentd`, so this is a
straightforward call, not a new dependency). The daemon:

1. Authenticates the connection the same way `ApprovePromotion` does
   (`Controller` role — this doesn't need `Actor::Human` specifically, since
   an automated CI-triggered `eval run --candidate-id` is a legitimate
   caller of this, unlike `ApprovePromotion`).
2. Re-derives `artifact_kind`/`name`/`version` from `promotion_candidates`
   exactly as `commands.rs:1700-1710` already does today (that half is
   already correct and should move, not change).
3. Writes `eval_suite_reports` itself, over the SAME pool, from data it
   received over the authenticated socket rather than from a second,
   unauthenticated writer of its own database file.
4. `crate::commands::eval_run` (`commands.rs:1755-1773`) changes from
   "open the daemon's SQLite file and INSERT" to "send this new command
   over the connection it already has open" — a strictly SMALLER diff than
   the current code, since the row-construction logic mostly just moves.

**Option B — cheaper, partial mitigation (does not close the hole, only
narrows it).** Leave the direct-SQLite write, but have `RunRegression`
(`codypendentd/src/promotion.rs:86`) additionally verify something about
the report's PROVENANCE that a hand-written row cannot forge — e.g., a
daemon-issued nonce/session token bound to a live `eval run --candidate-id`
invocation, checked at `RunRegression` time. This is a real improvement
(raises the bar from "hand-edit a SQL row" to "forge a live session token")
but does not remove the fundamental issue: the daemon still never OBSERVES
the runs.

I'd build Option A if it were mine — it is the only one of the two that
actually makes `migrations/0017_promotion_evidence.sql`'s "derived from a
persisted SuiteReport" claim true rather than aspirational. Option B is
listed because it's a real, much smaller diff if the socket-command work
doesn't fit this wave.

## What I did NOT do

I did not touch `codypendentd/src/promotion.rs`, `crates/daemon/src/
promotion.rs`, `crates/daemon/src/server.rs`, or `crates/protocol/**` —
all outside `crates/eval/**` / `crates/cli/src/eval.rs`, which are the only
files this task assigned me write access to. I also did not weaken or
work around the trust boundary from my side (`eval_run`'s report
construction is unchanged) — the fix belongs entirely on the daemon side,
per the rule that a check gating a resource must be enforced where the
resource is written, not only where a well-behaved caller happens to write
it from.
