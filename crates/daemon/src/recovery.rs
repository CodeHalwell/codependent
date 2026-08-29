//! Startup recovery and the failure matrix (STEP 1.14).
//!
//! Before the socket opens, the daemon reconciles the durable state a previous
//! process may have left mid-flight. [`recover_on_startup`] runs, in order:
//!
//! 1. **Artifact tmp-sweep** — [`ArtifactStore::sweep_tmp`] deletes the `tmp/`
//!    garbage a crash leaves between a streamed write and its atomic rename
//!    (STEP 1.4 RULE 2).
//! 2. **Worktree reconciliation** — [`WorktreeManager::reconcile_on_startup`]
//!    marks leases whose directory has vanished `orphaned` and flags stray
//!    tracked worktrees; it never deletes (STEP 1.8).
//! 3. **Pending-effect reconciliation** — [`CommandProcessor::reconcile_pending_effects`]
//!    sweeps `pending_effects` still `intended`/`performed` from a crash mid-apply
//!    so a duplicate external effect can never be re-performed (STEP 1.3 RULE 4).
//! 4. **Run recovery** — every non-resumable run in a *live* state at boot
//!    ([`is_live`]) is ended cleanly. **`Paused` runs are preserved**: a pause
//!    only parks the loop at a step boundary (every completed step is already
//!    ledgered), and an explicit `ResumeRun` re-drives the loop from the
//!    reconstructed transcript — so failing one here would destroy deliberate
//!    user work on every restart. The workflow layer makes the same choice
//!    (`WorkflowConductorHost::recover` continues on `Paused`). Durable
//!    document-publish continuations are excluded here and re-armed by the
//!    assembly layer once its Git adapters are available. Other live runs have
//!    no mid-node checkpoint, so they transition through `Recovering` and
//!    finish as `Failed` with a chronicle artifact.
//! 5. **Orphaned-approval expiry** — [`ApprovalBroker::expire_orphaned`] resolves
//!    (as rejected) every `pending` approval whose run is now terminal; a
//!    decision for a dead run can never be consumed, so re-surfacing it on
//!    every boot would be noise forever. Preserved `Paused` runs are NOT
//!    terminal, so their pending approvals survive and are re-consumed when
//!    the run is resumed.
//!
//! Recovery is **idempotent**: a run already `Failed` is not a live run, so it is
//! never re-failed; a swept `tmp/` is already empty; reconciled effects are no
//! longer `intended`. Running it twice changes nothing the second time.

use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{
    Actor, ApprovalId, DataClassification, EventBody, QuestionId, RunDisposition, RunId, RunState,
    SessionEvent, SessionId,
};
use serde::Serialize;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::approvals::ApprovalBroker;
use crate::artifacts::{ArtifactStore, Provenance};
use crate::commands::CommandProcessor;
use crate::projections::{self, run_state_from_db};
use crate::questions::QuestionBroker;
use crate::subscriptions::SubscriptionHub;
use crate::worktrees::WorktreeManager;

/// The `RunDisposition::Failed` reason recorded on a run failed by restart
/// recovery.
const RESTART_REASON: &str = "daemon restart";

/// What [`recover_on_startup`] did, for the boot log and for tests. Every field
/// is empty/zero on a clean boot and on the idempotent second pass.
#[derive(Debug, Clone, Default)]
pub struct RecoveryReport {
    /// Number of stray files/dirs removed from the artifact store's `tmp/`.
    pub swept_tmp: usize,
    /// Lease ids whose worktree directory was missing; marked `orphaned`.
    pub orphaned_leases: Vec<Uuid>,
    /// Number of `pending_effects` reconciled (marked `reconciled`/`abandoned`).
    pub reconciled_effects: usize,
    /// Runs that were live at boot and were cleanly failed with a chronicle.
    pub failed_runs: Vec<RunId>,
    /// Runs that were `Paused` at boot and were deliberately preserved (an
    /// explicit `ResumeRun` re-drives them; see `RuntimeExecutor::resume_run`).
    pub preserved_paused: Vec<RunId>,
    /// Pending approvals expired because their run is terminal (they could
    /// never be consumed; recovery resolves them as rejected).
    pub expired_approvals: Vec<ApprovalId>,
    /// Pending questions expired because their run is terminal.
    pub expired_questions: Vec<QuestionId>,
}

/// Whether a run state is *live* — in flight when the daemon stopped, and so a
/// candidate for recovery. The Phase 1 live set (STEP 1.14): a run that had begun
/// but not reached a terminal state. `Queued` is excluded (never started; simply
/// picked up), as are the terminal states.
pub fn is_live(state: RunState) -> bool {
    matches!(
        state,
        RunState::Running
            | RunState::Preparing
            | RunState::WaitingForApproval
            | RunState::WaitingForUserInput
            | RunState::Paused
            | RunState::Recovering
    )
}

/// Reconcile durable state a previous daemon left mid-flight, then return a
/// summary. Runs **before** the socket opens (wired into `main.rs` after
/// `record_boot`), so no client can observe a half-recovered run.
pub async fn recover_on_startup(
    pool: &SqlitePool,
    paths: &RuntimePaths,
) -> anyhow::Result<RecoveryReport> {
    // 1. Sweep artifact-store tmp garbage. Count the pre-existing entries first so
    //    the report reflects crash garbage only (the chronicle writes in step 4
    //    rename out of tmp cleanly).
    let artifacts_root = paths.data_dir.join("artifacts");
    let artifacts = ArtifactStore::new(artifacts_root.clone());
    let swept_tmp = count_tmp_entries(&artifacts_root).await;
    artifacts.sweep_tmp().await?;

    // 2. Reconcile worktree leases against Git (never deletes on startup).
    let reconcile = WorktreeManager::new().reconcile_on_startup(pool).await?;
    let orphaned_leases = reconcile.orphaned_leases;

    // 3. Reconcile in-flight pending effects (a fresh, throwaway processor: the
    //    real one is built in `server::run`; recovery only needs the sweep).
    let processor = CommandProcessor::new(
        SubscriptionHub::new(),
        ApprovalBroker::new(),
        QuestionBroker::new(),
    );
    let reconciled_effects = processor.reconcile_pending_effects(pool).await?;

    // 4. Cleanly fail every non-resumable live run (no mid-node checkpoint
    //    exists in Phase 1). `Paused` runs are preserved for an explicit
    //    `ResumeRun` (see `recover_live_runs`).
    let (failed_runs, preserved_paused) = recover_live_runs(pool, &artifacts).await?;

    // 5. Expire pending approvals whose run is now terminal — after step 4 that
    //    is every pending approval EXCEPT those of preserved `Paused` runs
    //    (whose run is not terminal and whose loop can be re-driven, so the
    //    decision is still consumable). The decision for a dead run can never
    //    be consumed, so leaving those rows `pending` would re-surface dead
    //    requests on every boot forever (and the real broker, built later in
    //    the executor, would reload them). Any other survivor is what a
    //    re-attaching client re-surfaces.
    let expired_approvals = ApprovalBroker::new()
        .expire_orphaned(pool, Utc::now())
        .await?;

    // 6. Expire pending questions whose run is now terminal.
    let expired_questions = QuestionBroker::new()
        .expire_orphaned(pool, Utc::now())
        .await?;

    Ok(RecoveryReport {
        swept_tmp,
        orphaned_leases,
        reconciled_effects,
        failed_runs,
        preserved_paused,
        expired_approvals,
        expired_questions,
    })
}

/// The minimal chronicle stored for a run ended by restart recovery. A full
/// Chronicle v0 (findings, actions, verification, costs) is folded from a run's
/// own events by the agent loop (STEP 1.10); a run killed mid-flight has no such
/// terminal fold, so recovery records this abbreviated form — enough to attribute
/// the failure and point at the run's last durable sequence.
#[derive(Debug, Serialize)]
struct RecoveryChronicle {
    run_id: RunId,
    objective: String,
    /// The terminal kind, always `"Failed"` here.
    disposition: String,
    /// Human-readable cause.
    summary: String,
    /// The run's last durable event sequence before recovery touched the ledger.
    last_sequence: u64,
    recovered_at: DateTime<Utc>,
}

/// Fail every live run in the `runs` table, returning the ids failed. Runs are
/// filtered in Rust via [`is_live`] (a single source of truth, forward-compatible
/// with new states) rather than an SQL `IN` list.
async fn recover_live_runs(
    pool: &SqlitePool,
    artifacts: &ArtifactStore,
) -> anyhow::Result<(Vec<RunId>, Vec<RunId>)> {
    let rows: Vec<(String, String, String, String)> =
        sqlx::query_as("SELECT id, session_id, objective, state FROM runs")
            .fetch_all(pool)
            .await?;

    let mut failed = Vec::new();
    let mut preserved_paused = Vec::new();
    for (id, session, objective, state) in rows {
        let run_state = run_state_from_db(&state);
        if !is_live(run_state) {
            continue;
        }
        // A `Paused` run parked at a step boundary by design: every completed
        // step is already in the ledger, and an explicit `ResumeRun` re-drives
        // the loop from the reconstructed transcript (see
        // `RuntimeExecutor::resume_run`). Failing it would destroy deliberate
        // user work on every restart — and its pending approvals, whose run is
        // not terminal, would survive step 5 anyway, pointing at a dead run.
        // The workflow layer makes the same choice (`recover` continues on
        // `Paused`).
        if run_state == RunState::Paused {
            preserved_paused.push(RunId::from_str(&id)?);
            continue;
        }
        let run_id = RunId::from_str(&id)?;
        let resumable_publish: (i64,) = sqlx::query_as(
            "SELECT EXISTS(SELECT 1 FROM document_publish_jobs \
             WHERE run_id = ? AND state IN ('pending', 'executing'))",
        )
        .bind(&id)
        .fetch_one(pool)
        .await?;
        if resumable_publish.0 != 0 {
            continue;
        }
        let session_id = SessionId::from_str(&session)?;
        if fail_live_run(pool, artifacts, run_id, session_id, &objective).await? {
            failed.push(run_id);
        }
    }
    Ok((failed, preserved_paused))
}

/// End one live run cleanly: record it moving through `Recovering`, store a
/// chronicle, and append the terminal `RunCompleted { Failed }` that references
/// it — leaving the projection row `Failed`.
///
/// The chronicle is written to the artifact store *before* the failing
/// transaction (its `put` runs its own commit and cannot join our tx). This is
/// crash-safe: a crash after the `put` but before the tx commit leaves the run
/// still live, so the next recovery re-fails it and writes a fresh chronicle —
/// the CAS store dedups identical blobs, and an unreferenced artifact row is
/// harmless. What matters is atomicity of the *failing itself*: the `Recovering`
/// marker, the projection flip to `Failed`, and the `RunCompleted` event all
/// commit together, so a live run never lands half-failed.
async fn fail_live_run(
    pool: &SqlitePool,
    artifacts: &ArtifactStore,
    run_id: RunId,
    session_id: SessionId,
    objective: &str,
) -> anyhow::Result<bool> {
    // The run's last durable sequence, before recovery appends anything.
    let last_sequence = crate::ledger::next_sequence(pool, session_id)
        .await?
        .saturating_sub(1);

    let chronicle = RecoveryChronicle {
        run_id,
        objective: objective.to_string(),
        disposition: "Failed".to_string(),
        summary: "ended by daemon restart recovery".to_string(),
        last_sequence,
        recovered_at: Utc::now(),
    };
    let chronicle_ref = artifacts
        .put(
            pool,
            "application/json",
            DataClassification::Internal,
            Provenance::system(format!("recovery-chronicle:{run_id}")),
            &serde_json::to_vec(&chronicle)?,
        )
        .await?;

    // One transaction ends the run. Sequences are allocated inside it, the
    // approvals/commands atomic-append pattern.
    let now = Utc::now().to_rfc3339();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;

    // Another terminal path may have won between the startup scan and this
    // transaction. Never append a second contradictory terminal outcome.
    let current: Option<(String,)> = sqlx::query_as("SELECT state FROM runs WHERE id = ?")
        .bind(run_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
    if !current
        .as_ref()
        .is_some_and(|(state,)| is_live(run_state_from_db(state)))
    {
        tx.rollback().await?;
        return Ok(false);
    }

    let seq = next_sequence(&mut *tx, session_id).await?;
    append_event(
        &mut *tx,
        session_id,
        seq,
        &Actor::System,
        &EventBody::RunStateChanged {
            run_id,
            state: RunState::Recovering,
        },
        &now,
    )
    .await?;

    projections::set_run_state(&mut *tx, run_id, RunState::Failed).await?;

    // The terminal `RunStateChanged { Failed }` must be in the ledger, not only
    // the projection: clients fold run liveness from `RunStateChanged` events,
    // so without it a folded catch-up shows the run stuck in `Recovering`
    // forever while the projection says `Failed` — breaking
    // `projection = fold(events)` for every recovered run.
    let seq = next_sequence(&mut *tx, session_id).await?;
    append_event(
        &mut *tx,
        session_id,
        seq,
        &Actor::System,
        &EventBody::RunStateChanged {
            run_id,
            state: RunState::Failed,
        },
        &now,
    )
    .await?;

    let seq = next_sequence(&mut *tx, session_id).await?;
    append_event(
        &mut *tx,
        session_id,
        seq,
        &Actor::System,
        &EventBody::RunCompleted {
            run_id,
            disposition: RunDisposition::Failed {
                reason: RESTART_REASON.to_string(),
            },
            chronicle: chronicle_ref,
        },
        &now,
    )
    .await?;

    crate::control_plane_sync::outbox::enqueue_run_snapshot(&mut tx, &run_id.to_string()).await?;
    tx.commit().await?;
    Ok(true)
}

/// Fail a run cleanly to a terminal `Failed` state — persisting a chronicle and
/// both the `RunStateChanged { Failed }` and `RunCompleted { Failed }` events in
/// one transaction — then **publish** those events to `subscriptions` so an
/// attached client observes the terminal transition live.
///
/// Used by the assembly binary's run executor when a run cannot be executed
/// (most commonly: no model is configured or reachable). The point is that the
/// run reaches a TERMINAL state — never left `Queued`/`Running` — so a headless
/// `codypendent run --jsonl` stops waiting instead of hanging.
///
/// Unlike [`fail_live_run`] (startup recovery, which runs *before* the socket
/// opens and so has no subscribers, and routes a mid-flight run through
/// `Recovering`), this is a *live* failure: it transitions straight to `Failed`
/// and publishes, mirroring the agent loop's own terminal path
/// (persist-before-publish). The transition is also a transactional compare-and-
/// set: a cancellation, pause, or terminal outcome that committed first wins,
/// and this function becomes an idempotent no-op rather than overwriting it with
/// a contradictory failure.
pub async fn fail_run(
    pool: &SqlitePool,
    artifacts: &ArtifactStore,
    subscriptions: &SubscriptionHub,
    run_id: RunId,
    session_id: SessionId,
    objective: &str,
    reason: &str,
) -> anyhow::Result<()> {
    // The run's last durable sequence, before this failure appends anything.
    let last_sequence = crate::ledger::next_sequence(pool, session_id)
        .await?
        .saturating_sub(1);

    let chronicle = RecoveryChronicle {
        run_id,
        objective: objective.to_string(),
        disposition: "Failed".to_string(),
        summary: reason.to_string(),
        last_sequence,
        recovered_at: Utc::now(),
    };
    // The chronicle blob is written before the failing transaction (its `put`
    // runs its own commit); an unreferenced blob after a crash is harmless.
    let chronicle_ref = artifacts
        .put(
            pool,
            "application/json",
            DataClassification::Internal,
            Provenance::system(format!("run-failed:{run_id}")),
            &serde_json::to_vec(&chronicle)?,
        )
        .await?;

    // One transaction: the projection flip to `Failed` and both terminal events
    // commit together, so a run never lands half-failed.
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;

    // A lifecycle command or another terminal path may have won after the
    // caller observed its infrastructure failure. Re-read under the same write
    // transaction that appends the terminal events, and fail only states still
    // actively executable. In particular, never turn Cancelled/Completed/Failed
    // into Failed, and never defeat a PauseRun that committed first.
    let current: Option<(String, String)> =
        sqlx::query_as("SELECT state, session_id FROM runs WHERE id = ?")
            .bind(run_id.to_string())
            .fetch_optional(&mut *tx)
            .await?;
    let may_fail = current.as_ref().is_some_and(|(state, owner_session)| {
        owner_session == &session_id.to_string()
            && matches!(
                crate::projections::run_state_from_db(state),
                RunState::Queued
                    | RunState::Preparing
                    | RunState::Running
                    | RunState::WaitingForApproval
                    | RunState::WaitingForUserInput
                    | RunState::Recovering
            )
    });
    if !may_fail {
        tx.rollback().await?;
        return Ok(());
    }

    let failed_state = EventBody::RunStateChanged {
        run_id,
        state: RunState::Failed,
    };
    let seq1 = next_sequence(&mut *tx, session_id).await?;
    append_event(
        &mut *tx,
        session_id,
        seq1,
        &Actor::System,
        &failed_state,
        &now_str,
    )
    .await?;

    projections::set_run_state(&mut *tx, run_id, RunState::Failed).await?;

    let completed = EventBody::RunCompleted {
        run_id,
        disposition: RunDisposition::Failed {
            reason: reason.to_string(),
        },
        chronicle: chronicle_ref,
    };
    let seq2 = next_sequence(&mut *tx, session_id).await?;
    append_event(
        &mut *tx,
        session_id,
        seq2,
        &Actor::System,
        &completed,
        &now_str,
    )
    .await?;

    crate::control_plane_sync::outbox::enqueue_run_snapshot(&mut tx, &run_id.to_string()).await?;
    tx.commit().await?;

    // Persist-before-publish: only after the commit do the terminal events fan
    // out to any attached client. Publishing to zero subscribers is normal.
    subscriptions.publish(
        session_id,
        SessionEvent {
            sequence: u64::try_from(seq1)?,
            occurred_at: now,
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body: failed_state,
        },
    );
    subscriptions.publish(
        session_id,
        SessionEvent {
            sequence: u64::try_from(seq2)?,
            occurred_at: now,
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body: completed,
        },
    );
    Ok(())
}

/// Keep a durable live run's terminalization obligation active until its
/// failure outcome is committed (or a lifecycle/terminal CAS winner makes the
/// operation an idempotent success).
///
/// The still-live `runs` row is the crash-durable obligation: startup recovery
/// will pick it up if this process exits. While the process remains alive, this
/// loop must not abandon that obligation after an arbitrary retry count and
/// leave clients waiting forever. The delay is bounded so a persistent storage
/// outage neither hot-loops nor grows an unbounded timer.
pub async fn fail_run_until_settled(
    pool: &SqlitePool,
    artifacts: &ArtifactStore,
    subscriptions: &SubscriptionHub,
    run_id: RunId,
    session_id: SessionId,
    objective: &str,
    reason: &str,
) {
    retry_terminalization(run_id, || {
        fail_run(
            pool,
            artifacts,
            subscriptions,
            run_id,
            session_id,
            objective,
            reason,
        )
    })
    .await;
}

/// Supply the authoritative completion barrier for a cancellation whose live
/// worker did not reach its normal present phase promptly.
///
/// `CancelRun` commits the `Cancelled` projection before the assembly fires the
/// in-memory token. Usually the runtime observes that token and atomically
/// appends its own richer chronicle plus `RunCompleted`. Cancellation can,
/// however, arrive while the worker is in assembly-owned setup (context
/// hydration, model readiness, or worktree binding), where there is no runtime
/// future to select against yet. A session owner must not be left unable to
/// close forever just because that setup is slow or wedged.
///
/// This fallback is deliberately state- and event-guarded. It does nothing
/// unless the durable projection is already `Cancelled`, and
/// [`crate::ledger::append_run_terminal`] makes the final append idempotent with
/// a runtime completion racing it. It never turns a live, failed, or successful
/// run into a cancellation.
pub async fn complete_cancelled_run(
    pool: &SqlitePool,
    artifacts: &ArtifactStore,
    subscriptions: &SubscriptionHub,
    run_id: RunId,
) -> anyhow::Result<()> {
    let Some((session_id, objective)) = cancelled_run_missing_completion(pool, run_id).await?
    else {
        return Ok(());
    };
    let last_sequence = crate::ledger::next_sequence(pool, session_id)
        .await?
        .saturating_sub(1);
    let chronicle = RecoveryChronicle {
        run_id,
        objective,
        disposition: "Cancelled".to_string(),
        summary: "run cancelled before its worker emitted terminal evidence".to_string(),
        last_sequence,
        recovered_at: Utc::now(),
    };
    let chronicle_ref = artifacts
        .put(
            pool,
            "application/json",
            DataClassification::Internal,
            Provenance::system(format!("run-cancelled:{run_id}")),
            &serde_json::to_vec(&chronicle)?,
        )
        .await?;
    let completion = EventBody::RunCompleted {
        run_id,
        disposition: RunDisposition::Cancelled {
            reason: Some("run cancelled".to_string()),
        },
        chronicle: chronicle_ref,
    };
    let events = match crate::ledger::append_run_terminal(
        pool,
        session_id,
        &Actor::System,
        RunState::Cancelled,
        &completion,
        Utc::now(),
    )
    .await
    {
        Ok(events) => events,
        Err(error) => {
            // The runtime may have won the idempotency race and the owner may
            // have closed the session before this fallback acquired its write
            // transaction. That is success, not a storage outage to retry
            // forever. Re-read after the failed append; only preserve the error
            // while the cancelled run still lacks its barrier in an open
            // session.
            if cancelled_run_missing_completion(pool, run_id)
                .await?
                .is_none()
            {
                return Ok(());
            }
            return Err(error);
        }
    };
    for event in events {
        subscriptions.publish(session_id, event);
    }
    Ok(())
}

/// Return the identity needed by the cancellation fallback only while there is
/// still a real terminal-evidence obligation. A completed or closed session is
/// already settled, as is any projection other than `Cancelled`.
async fn cancelled_run_missing_completion(
    pool: &SqlitePool,
    run_id: RunId,
) -> anyhow::Result<Option<(SessionId, String)>> {
    let row: Option<(String, String, String, String, i64)> = sqlx::query_as(
        "SELECT r.session_id, r.objective, r.state, s.state, \
         EXISTS(SELECT 1 FROM events e WHERE e.session_id = r.session_id \
           AND json_valid(e.body) AND json_extract(e.body, '$.type') = 'RunCompleted' \
           AND json_extract(e.body, '$.run_id') = r.id) \
         FROM runs r JOIN sessions s ON s.id = r.session_id WHERE r.id = ?",
    )
    .bind(run_id.to_string())
    .fetch_optional(pool)
    .await?;
    let Some((session_id, objective, run_state, session_state, completed)) = row else {
        return Ok(None);
    };
    if run_state_from_db(&run_state) != RunState::Cancelled
        || session_state == "closed"
        || completed != 0
    {
        return Ok(None);
    }
    Ok(Some((SessionId::from_str(&session_id)?, objective)))
}

/// Keep the cancellation-completion obligation active across transient storage
/// failures. The cancelled projection is durable, so giving up after an
/// arbitrary retry count would leave an uncloseable session until restart.
pub async fn complete_cancelled_run_until_settled(
    pool: &SqlitePool,
    artifacts: &ArtifactStore,
    subscriptions: &SubscriptionHub,
    run_id: RunId,
) {
    retry_terminalization(run_id, || {
        complete_cancelled_run(pool, artifacts, subscriptions, run_id)
    })
    .await;
}

async fn retry_terminalization<F, Fut>(run_id: RunId, mut persist: F) -> u32
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let mut attempt = 0u32;
    let mut delay = std::time::Duration::from_millis(100);
    loop {
        attempt = attempt.saturating_add(1);
        match persist().await {
            Ok(()) => return attempt,
            Err(error) => {
                tracing::warn!(
                    %run_id,
                    %error,
                    attempt,
                    delay_ms = delay.as_millis(),
                    "run terminalization is still pending; retrying"
                );
                tokio::time::sleep(delay).await;
                delay = delay
                    .saturating_mul(2)
                    .min(std::time::Duration::from_secs(30));
            }
        }
    }
}

/// Count the top-level entries under `<artifacts>/tmp` (missing dir ⇒ 0). Read
/// before the sweep so the report reflects only crash garbage.
async fn count_tmp_entries(artifacts_root: &Path) -> usize {
    let tmp_dir = artifacts_root.join("tmp");
    let mut entries = match tokio::fs::read_dir(&tmp_dir).await {
        Ok(entries) => entries,
        Err(_) => return 0,
    };
    let mut count = 0usize;
    while let Ok(Some(_entry)) = entries.next_entry().await {
        count += 1;
    }
    count
}

/// The next 1-based event sequence for a session, read inside the caller's
/// transaction so the append that claims it is atomic with the read (mirrors
/// [`crate::approvals`] / [`crate::commands`]).
async fn next_sequence(
    exec: impl sqlx::SqliteExecutor<'_>,
    session_id: SessionId,
) -> Result<i64, sqlx::Error> {
    let (max,): (i64,) =
        sqlx::query_as("SELECT COALESCE(MAX(sequence), 0) FROM events WHERE session_id = ?")
            .bind(session_id.to_string())
            .fetch_one(exec)
            .await?;
    Ok(max + 1)
}

/// Append one event within the caller's transaction (`System` actor, no
/// causation — recovery housekeeping, like the approval broker's own events).
async fn append_event(
    exec: impl sqlx::SqliteExecutor<'_>,
    session_id: SessionId,
    sequence: i64,
    actor: &Actor,
    body: &EventBody,
    occurred_at: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO events \
         (session_id, sequence, occurred_at, actor, body, causation_id, correlation_id, schema_version) \
         VALUES (?, ?, ?, ?, ?, NULL, NULL, 1)",
    )
    .bind(session_id.to_string())
    .bind(sequence)
    .bind(occurred_at)
    .bind(serde_json::to_string(actor)?)
    .bind(serde_json::to_string(body)?)
    .execute(exec)
    .await?;
    Ok(())
}

#[cfg(test)]
mod terminalization_retry_tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test(start_paused = true)]
    async fn terminalization_obligation_is_not_abandoned_after_four_failures() {
        let calls = AtomicU32::new(0);
        let attempts = retry_terminalization(RunId::new(), || {
            let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if call <= 5 {
                    anyhow::bail!("injected storage outage")
                }
                Ok(())
            }
        })
        .await;

        assert_eq!(attempts, 6);
        assert_eq!(calls.load(Ordering::SeqCst), 6);
    }
}
