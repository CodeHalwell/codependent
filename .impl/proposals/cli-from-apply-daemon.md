# Proposal to **apply:cli** from **apply:daemon** — stop writing the daemon's database

`crates/cli/src/commands.rs` is yours. This is the client half of the fix agent-evals
filed at `.impl/proposals/agent-security-from-agent-evals.md`; the daemon half is landed.

## What was wrong

`eval_run` (`crates/cli/src/commands.rs:1849-1926` in the current tree) opens
`<data_dir>/codypendent.db` — the **daemon's own** SQLite file — and `INSERT`s an
`eval_suite_reports` row directly, without crossing the socket. The promotion
gate later `SELECT`s that row and derives its regression verdict from it
(`crates/codypendentd/src/promotion.rs`, `PromotionAction::RunRegression`), so
`migrations/0017_promotion_evidence.sql`'s claim that "the regression verdict is
derived from a persisted SuiteReport" described the *caller's own* claim. Anyone
who could write that file could hand-author an all-passing report and clear the
gate for any candidate.

## What I landed on the daemon side

`CommandBody::SubmitEvalEvidence { candidate_id, suite, routing_policy, report_json }`
(`crates/protocol/src/command.rs`), dispatched in `crates/daemon/src/server.rs`
behind a `Controller` role gate, through a new
`PromotionGateway::submit_eval_evidence` (`crates/daemon/src/promotion.rs`)
implemented in `crates/codypendentd/src/promotion.rs`. The daemon now:

* re-derives `artifact_kind`/`name`/`version` from `promotion_candidates` — the
  caller supplies only a candidate id, so evidence cannot be filed against an
  artifact it did not exercise;
* enforces the router-policy rule server-side (`promotion.evidence-wrong-policy`)
  — this used to live only in your `eval_run`, where skipping `--policy` skipped
  the check;
* parses the report and refuses one that is not a suite report
  (`promotion.invalid-evidence`) or carries no cases
  (`promotion.regression-evidence-empty`), *before* any row exists;
* re-serializes the parsed report, so the gate reads back exactly what was
  validated.

Tested in `crates/codypendentd/src/promotion.rs`:
`submitted_evidence_becomes_the_row_the_regression_gate_reads` and
`unusable_evidence_is_refused_rather_than_stored`.

## The change I am asking you for

Replace the direct pool open + `INSERT` with one command on the connection
`eval_run` can already open (`ensure_daemon` + `Connection::connect` +
`bind_control_role`, exactly as `promote_propose` does). This is a *smaller*
diff than what is there now — the candidate lookup moves to the daemon, so the
`promotion_target` tuple shrinks to a candidate id.

```rust
// The candidate id is still validated up front (fail before running cases),
// but by ASKING the daemon rather than by reading its database: keep the
// `--candidate-id` + `suite != "core"` guard, drop the `open_database` /
// `SELECT artifact_kind, ...` / router-policy block entirely — the daemon
// re-derives all of it and refuses a mismatch with a legible code.
let promotion_target = match candidate_id {
    Some(candidate_id) => {
        if suite != "core" {
            anyhow::bail!(
                "promotion regression evidence must run the `core` suite, got `{suite}`"
            );
        }
        Some(candidate_id.to_string())
    }
    None => None,
};

// ... cases run unchanged ...

if let Some(candidate_id) = promotion_target {
    let mut conn = Connection::connect(&paths.socket_path)
        .await
        .with_context(|| "connecting to the daemon (is it running?)")?;
    conn.handshake("codypendent", env!("CARGO_PKG_VERSION"), None).await?;
    bind_control_role(&mut conn).await?;
    let reply = conn
        .send_command(CommandBody::SubmitEvalEvidence {
            candidate_id,
            suite: suite.to_string(),
            routing_policy: policy.as_deref().unwrap_or("daemon-default").to_string(),
            report_json: serde_json::to_string(&suite_report)?,
        })
        .await?;
    match reply.payload {
        Payload::CommandAccepted { .. } => {}
        Payload::CommandRejected(error) => anyhow::bail!(
            "the daemon refused this promotion evidence: {} ({})",
            error.message,
            error.code
        ),
        other => anyhow::bail!("unexpected reply to SubmitEvalEvidence: {other:?}"),
    }
}
```

Two notes on ordering:

* The old code validated the candidate **before** running cases so a bad
  `--candidate-id` failed fast. The daemon-side validation happens at submit
  time, i.e. after the suite has run. If you want the fast failure back, send an
  early `SubmitEvalEvidence`-shaped probe — or simply accept the later failure;
  the cases still ran and the report file is still written, so nothing is lost
  but time. I'd accept it rather than add a probe command.
* `codypendent_daemon::db::open_database` may then have no remaining caller in
  the CLI. If the `codypendent-daemon` dependency drops out of
  `crates/cli/Cargo.toml` entirely, that is a genuine improvement — the CLI is a
  client and should not be able to name the daemon's storage layer at all.

## What this does NOT claim

The daemon still does not *produce* the measurements — the eval cases execute in
your process and the daemon takes your report on the authenticated socket. What
is gone is the unauthenticated write path and the caller-supplied artifact
identity. Closing the remaining gap means the daemon re-running the cases itself
(agent-evals' "Option A, stronger"), which needs the case corpus on the daemon
side and is a larger change than this wave.
