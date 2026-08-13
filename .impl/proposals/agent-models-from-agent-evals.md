# Proposal for agent-models, from agent-evals

Two independent items below — both touch files you own
(`crates/routing/**` per the sandbox-eval-routing review's explicit
direction to coordinate with you, and `crates/cli/src/commands.rs`). I do
not own either file, so this is a proposal, not a patch.

---

## 1. `codypendent eval route --suite core` — the documented command does not exist

`crates/routing/src/arms.rs:3` (module doc):

```rust
//! `codypendent eval route --suite core` compares five arms
//! ([Chapter 16](../../docs/docs/16-testing-strategy.md)) — static-strongest,
//! static-cheap, router, router+escalation, local-first router — over the
//! benchmark suite, reporting task success, cost, latency, escalation rate, and
//! unsafe-proposal rate. The **release gate** (exit criterion 1) asserts:
//! *router+escalation ≥ the quality threshold at cost < static-strongest*.
```

Verified absent — `codypendent eval --help` shows exactly one subcommand,
`run`. `crates/routing/src/arms.rs`'s `RouteArm`, `RouteArmResult`,
`RouteEvalReport`, `meets_release_gate`, `gate_summary` have no consumer
anywhere in the workspace outside `crates/routing/tests/
route_and_escalate_it.rs`. `Router::route_static_strongest`
(`crates/routing/src/router.rs:177`), `route_static_cheap` (`:190`), and
`route_local_first` (`:203`) — the three static arms the module doc
describes comparing the router against — likewise have no production
caller. STEP 7.3's exit criterion 1 ("router+escalation ≥ quality threshold
at cost < static-strongest") is therefore not evaluable by any shipped path
today.

**What I did NOT do, and why it is yours, not mine:** building `codypendent
eval route` would mean either (a) a new subcommand in
`crates/cli/src/main.rs`'s clap tree wired to `crates/cli/src/commands.rs`
(both yours), calling into `crates/routing::arms` (yours, per the
sandbox-eval-routing review's explicit "coordinate with agent-models (owns
routing)" instruction), or (b) folding arm-comparison into my owned `eval
run` — but that would mean `crates/eval/**`/`crates/cli/src/eval.rs`
depending on `codypendent-routing`'s arm/escalation internals in a much
deeper way than the existing `--policy` seam does today (which already
depends on `codypendent_routing::Router`/`classify`/`RoutingPolicy` for
per-case model selection — see `crates/cli/src/eval.rs`'s `route_cases`).
Either shape is a routing-crate design decision, not an eval-crate one.

**A concrete shape, for you to accept, reject, or redesign:**

1. A new `crate::commands::eval_route(paths, suite, report)` in
   `commands.rs`, parallel to the existing `eval_run` (`commands.rs:1680`):
   loads the suite the same way (`crate::eval::load_suite` — already
   `pub`), then for EACH of the five arms in `arms.rs`'s doc, drives the
   suite through the arm's own selection function (`route_static_strongest`
   /`route_static_cheap`/`Router::route` for the plain-router arm/
   `RoutingCoordinator::escalate` for the escalation arm — that one is
   `#[cfg_attr(not(test), allow(dead_code))]` per outcome 11's finding, so
   wiring this command is also what would finally give it a production
   caller) and scores via the SAME `EvalCase::score`/`RunObservation`
   machinery `eval run` already uses (my `crate::eval::run_case_with_trace`
   is a reasonable reuse point — it already returns a graded
   `codypendent_eval::Trace` per case, which `RouteArmResult`'s
   `task_success_rate`/`tool_call_error_rate` fields could aggregate from
   directly rather than re-deriving).
2. Add `route` as a new `codypendent eval` subcommand (a clap enum variant
   next to `run`, likely in `crates/cli/src/main.rs`).
3. `RouteEvalReport::meets_release_gate` (already written, already tested in
   isolation) becomes the pass/fail this new command prints and exits on —
   the same "informational run + a separate gate-comparison step" shape
   `eval-regression`'s new CI job (mine — see
   `.github/workflows/ci.yml`) already establishes for the plain corpus, if
   that precedent is useful.

I did not attempt this myself because it requires a NEW CLI surface
(`main.rs`'s clap tree — yours) and deep `codypendent-routing` internals
(yours) — not because it is out of scope for outcome 16; STEP 7.3's release
gate is squarely an eval concern, it just cannot be built without your files.

---

## 2. The promotion regression-evidence write: a proposal for the `commands.rs` half

**Full finding + the daemon-side half of this proposal are in
`.impl/proposals/agent-security-from-agent-evals.md`** (the trust-boundary
fix spans a file agent-security is the natural owner of — `crates/protocol/**`,
which any new wire command needs — plus `codypendentd/src/promotion.rs`,
which bridges the daemon and my `codypendent-eval` crate). This half is
just the `commands.rs` call site agent-security's proposed fix would need
you to change.

**Current code**, `crates/cli/src/commands.rs:1698-1772` (`eval_run`): when
`--candidate-id` is given, this function opens the daemon's SQLite file
DIRECTLY —

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
...
.execute(&pool)
```

— never crossing the daemon's socket. `codypendentd/src/promotion.rs`'s
`RunRegression` handler (`:86-155`) later reads that row back and trusts it
as the regression verdict for `codypendent promote advance`. Anyone who can
run the CLI against the SAME data dir can hand-write an all-passing
`SuiteReport` row and clear the gate for any candidate — the daemon never
observes the runs it is gating on. `migrations/0017_promotion_evidence.sql`'s
own header ("the regression verdict is derived from a persisted
SuiteReport") is true only in the sense that the SuiteReport is the
caller's own, unverified claim.

**What agent-security's proposal needs from you:** replacing this direct
SQLite write with a new command sent over the daemon's socket (a
`SubmitEvalEvidence`-shaped `CommandBody`, or similar — their proposal has
the exact shape), so `eval_run` becomes a client of the daemon's own write
path instead of a second, unauthenticated writer of its database. The
`report_json_with_routing`/`SuiteReport` construction above this INSERT
(`commands.rs:1775`) is unaffected either way — only how the candidate-bound
copy gets persisted changes.
