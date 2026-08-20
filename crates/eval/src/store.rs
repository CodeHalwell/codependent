//! Durable promotion-candidate storage (STEP 7.5 daemon wiring): a
//! [`PromotionStore`] over a SQLite pool, mirroring `codypendent-workflow`'s
//! `store` module in shape and in its no-back-door discipline.
//!
//! **No SQL path reaches `stage = 'promoted'` except through
//! [`Candidate::approve`].** Every mutating method here follows the same
//! sequence: load the persisted [`Candidate`] (deserializing its private
//! fields via `serde` — "a deliberate, daemon-owned persistence path", per
//! [`crate::promote`]'s trust-boundary note), call the **real** state-machine
//! method on it, and persist exactly the value that method produced. There is
//! no setter that writes a stage directly; the `stage` TEXT column is always a
//! read-only, denormalized copy of whatever `candidate_json` says, derived at
//! the moment of the write, never independently. Activating a version
//! ([`PromotionStore::approve`]) additionally requires the
//! [`PromotionRecord`] `Candidate::approve` returns — the same unforgeable
//! receipt the in-memory `ActiveVersions` type requires.

use chrono::Utc;
use codypendent_protocol::events::Actor;
use serde::{Deserialize, Serialize};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::promote::{
    ArtifactKind, ArtifactVersion, CanaryOutcome, Candidate, PromotionError, PromotionRecord,
    PromotionStage,
};

/// An error from the promotion store.
#[derive(Debug, thiserror::Error)]
pub enum PromotionStoreError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
    /// No candidate with the given id.
    #[error("no such promotion candidate: {0}")]
    NotFound(String),
    /// A stored row could not be decoded (should never happen; the store wrote it).
    #[error("corrupt promotion-candidate row: {0}")]
    Corrupt(String),
    /// The underlying state-machine transition was illegal (stage guard,
    /// non-human approver, unobserved canary, …) — the store never overrides
    /// this; it only ever surfaces it.
    #[error(transparent)]
    Promotion(#[from] PromotionError),
    /// Another observation advanced the candidate's window between the caller
    /// measuring it and this write. The samples just measured overlap the ones
    /// already counted, so they are refused rather than added: retry, and the
    /// next measurement starts from the window this one established.
    #[error("promotion candidate {0} was observed concurrently; re-measure and retry")]
    WindowMoved(String),
}

/// A persisted candidate: its durable id plus the live `Candidate` value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateSnapshot {
    pub id: String,
    pub candidate: Candidate,
}

/// The promotion store. Stateless; the pool is passed to each method (mirrors
/// `codypendent_workflow::WorkflowStore`).
#[derive(Debug, Clone, Copy, Default)]
pub struct PromotionStore;

impl PromotionStore {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Draft a fresh candidate and persist it. Returns the new candidate id.
    /// `author` need not be human — a grader/agent may draft; see
    /// [`Candidate::draft`].
    pub async fn propose(
        &self,
        pool: &SqlitePool,
        artifact: ArtifactVersion,
        author: &Actor,
        requires_permission_review: bool,
    ) -> Result<String, PromotionStoreError> {
        let id = Uuid::now_v7().to_string();
        let candidate = draft_candidate(artifact, author, requires_permission_review);
        insert_candidate(pool, &id, &candidate).await?;
        Ok(id)
    }

    /// Draft a fresh candidate **idempotently**, keyed by a client
    /// `idempotency_key` (mirrors
    /// `WorkflowStore::create_run_idempotent`): a duplicate `ProposePromotion`
    /// delivery (a client retrying after a lost acknowledgement) resolves to
    /// the *same* candidate instead of drafting a second one. The id is
    /// derived deterministically from the key; `INSERT OR IGNORE` makes a
    /// concurrent duplicate delivery resolve to one row (SQLite serializes
    /// writes).
    pub async fn propose_idempotent(
        &self,
        pool: &SqlitePool,
        idempotency_key: &str,
        artifact: ArtifactVersion,
        author: &Actor,
        requires_permission_review: bool,
    ) -> Result<String, PromotionStoreError> {
        let id = deterministic_candidate_id(idempotency_key);
        let candidate = draft_candidate(artifact, author, requires_permission_review);
        let json = serde_json::to_string(&candidate)?;
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT OR IGNORE INTO promotion_candidates \
             (id, artifact_kind, artifact_name, artifact_version, stage, candidate_json, \
              created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(candidate.artifact().kind.as_str())
        .bind(&candidate.artifact().name)
        .bind(i64::from(candidate.artifact().version))
        .bind(stage_str(candidate.stage()))
        .bind(&json)
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await?;
        Ok(id)
    }

    /// Record that a synthesized candidate's permission review passed (a
    /// prerequisite for [`Self::run_regression`] to accept it — see
    /// [`Candidate::mark_permission_reviewed`]).
    pub async fn mark_permission_reviewed(
        &self,
        pool: &SqlitePool,
        id: &str,
    ) -> Result<(), PromotionStoreError> {
        let mut tx = pool.begin().await?;
        let mut candidate = load_for_update(&mut tx, id).await?;
        candidate.mark_permission_reviewed();
        save_candidate(&mut tx, id, &candidate).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Persist a server-computed offline regression verdict. The assembly
    /// layer owns evidence collection; this store owns the state transition.
    pub async fn run_regression(
        &self,
        pool: &SqlitePool,
        id: &str,
        regressed: bool,
    ) -> Result<(), PromotionStoreError> {
        let mut tx = pool.begin().await?;
        let mut candidate = load_for_update(&mut tx, id).await?;
        let result = candidate.run_regression(regressed);
        save_candidate(&mut tx, id, &candidate).await?;
        tx.commit().await?;
        result.map_err(Into::into)
    }

    /// Begin the shadow run. See [`Candidate::start_shadow`].
    pub async fn start_shadow(
        &self,
        pool: &SqlitePool,
        id: &str,
    ) -> Result<(), PromotionStoreError> {
        let mut tx = pool.begin().await?;
        let mut candidate = load_for_update(&mut tx, id).await?;
        candidate.start_shadow()?;
        save_candidate(&mut tx, id, &candidate).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Begin the limited canary. See [`Candidate::start_canary`].
    pub async fn start_canary(
        &self,
        pool: &SqlitePool,
        id: &str,
    ) -> Result<(), PromotionStoreError> {
        let mut tx = pool.begin().await?;
        let mut candidate = load_for_update(&mut tx, id).await?;
        candidate.start_canary()?;
        save_candidate(&mut tx, id, &candidate).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Record aggregate canary evidence derived by the assembly layer from
    /// measured metrics. The sample population is persisted in the candidate
    /// and enforced by [`Candidate::finish_canary`].
    /// A regression auto-rolls-back and this method persists the resulting
    /// system-attributed [`PromotionRecord`] to the audit trail (P7-5) —
    /// [`Self::finish_canary`] can never be reached by fabricating a pass:
    /// [`Candidate::finish_canary`] requires at least one such call (P7-2).
    pub async fn observe_canary(
        &self,
        pool: &SqlitePool,
        id: &str,
        regressed: bool,
    ) -> Result<CanaryOutcome, PromotionStoreError> {
        self.observe_canary_samples(pool, id, regressed, 1, None)
            .await
    }

    /// Record a measured canary population and its server-derived verdict.
    /// Fold one measured canary slice into `id`.
    ///
    /// `measured_from` is the `updated_at` the caller read when it chose the
    /// observation window, and this is a compare-and-swap on it. Two things
    /// made that necessary. The caller reads the window bound OUTSIDE any
    /// transaction, and this transaction was `BEGIN` — deferred, so its read
    /// took no write lock. Two concurrent or retried observations therefore
    /// measured the identical window and each added its samples, so fifty real
    /// executions counted twice satisfied `MIN_CANARY_SAMPLES = 100`: the
    /// safety gate defeated by precisely the error class it exists to catch.
    ///
    /// `BEGIN IMMEDIATE` serializes the writers; the CAS makes the second one
    /// refuse rather than double-count. `save_candidate` advances `updated_at`,
    /// so a caller that retries measures a window starting where this one
    /// ended.
    pub async fn observe_canary_samples(
        &self,
        pool: &SqlitePool,
        id: &str,
        regressed: bool,
        sample_count: u64,
        measured_from: Option<&str>,
    ) -> Result<CanaryOutcome, PromotionStoreError> {
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        // `None` is the single-sample path, which derives no window and so has
        // nothing to compare; `BEGIN IMMEDIATE` above still serializes it.
        if let Some(expected) = measured_from {
            let current: String =
                sqlx::query_scalar("SELECT updated_at FROM promotion_candidates WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or_else(|| PromotionStoreError::NotFound(id.to_string()))?;
            if current != expected {
                return Err(PromotionStoreError::WindowMoved(id.to_string()));
            }
        }
        let mut candidate = load_for_update(&mut tx, id).await?;
        let outcome = candidate.observe_canary_samples(regressed, sample_count)?;
        save_candidate(&mut tx, id, &candidate).await?;
        if let CanaryOutcome::AutoRolledBack(record) = &outcome {
            append_event(&mut tx, id, record).await?;
        }
        tx.commit().await?;
        Ok(outcome)
    }

    /// Finish the canary and assemble the comparison. Fails with
    /// [`PromotionError::CanaryInsufficientEvidence`] (surfaced as
    /// [`PromotionStoreError::Promotion`]) if [`Self::observe_canary`] was
    /// never called — the structural guard against a canary "passing"
    /// unobserved (P7-2). See [`Candidate::finish_canary`].
    pub async fn finish_canary(
        &self,
        pool: &SqlitePool,
        id: &str,
    ) -> Result<(), PromotionStoreError> {
        let mut tx = pool.begin().await?;
        let mut candidate = load_for_update(&mut tx, id).await?;
        candidate.finish_canary()?;
        save_candidate(&mut tx, id, &candidate).await?;
        tx.commit().await?;
        Ok(())
    }

    /// **Approve and promote, then activate the version.** The only path in
    /// this store that can reach `stage = 'promoted'` — because it is the
    /// only path that calls [`Candidate::approve`], which itself refuses any
    /// non-[`Actor::Human`] approver (ADR-010, exit criterion 2; ported
    /// unchanged from the in-memory type). On success, the resulting
    /// [`PromotionRecord`] is both appended to the audit trail and used to
    /// activate the artifact's version (mirrors `ActiveVersions::activate` —
    /// idempotent on a repeat activation of the version already on top).
    pub async fn approve(
        &self,
        pool: &SqlitePool,
        id: &str,
        approver: &Actor,
    ) -> Result<PromotionRecord, PromotionStoreError> {
        let mut tx = pool.begin().await?;
        let mut candidate = load_for_update(&mut tx, id).await?;
        let record = candidate.approve(approver)?;
        save_candidate(&mut tx, id, &candidate).await?;
        append_event(&mut tx, id, &record).await?;
        activate(
            &mut tx,
            &record.artifact().stem(),
            record.artifact().version,
        )
        .await?;
        tx.commit().await?;
        Ok(record)
    }

    /// Manually roll back a promoted candidate, attributing `actor` (P7-5) and
    /// popping the artifact's active-version stack back to its predecessor
    /// (mirrors `ActiveVersions::rollback`). See [`Candidate::rollback`] for
    /// why this is deliberately not restricted to [`Actor::Human`].
    pub async fn rollback(
        &self,
        pool: &SqlitePool,
        id: &str,
        actor: &Actor,
    ) -> Result<PromotionRecord, PromotionStoreError> {
        let mut tx = pool.begin().await?;
        let mut candidate = load_for_update(&mut tx, id).await?;
        let record = candidate.rollback(actor)?;
        save_candidate(&mut tx, id, &candidate).await?;
        append_event(&mut tx, id, &record).await?;
        deactivate(&mut tx, &record.artifact().stem()).await?;
        tx.commit().await?;
        Ok(record)
    }

    /// The current snapshot of a candidate, or `None` if it does not exist.
    pub async fn get(
        &self,
        pool: &SqlitePool,
        id: &str,
    ) -> Result<Option<CandidateSnapshot>, PromotionStoreError> {
        let row = sqlx::query("SELECT candidate_json FROM promotion_candidates WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let candidate = decode_candidate(&row)?;
        Ok(Some(CandidateSnapshot {
            id: id.to_string(),
            candidate,
        }))
    }

    /// Every candidate currently at `stage`, oldest first.
    pub async fn list_by_stage(
        &self,
        pool: &SqlitePool,
        stage: PromotionStage,
    ) -> Result<Vec<CandidateSnapshot>, PromotionStoreError> {
        let rows = sqlx::query(
            "SELECT id, candidate_json FROM promotion_candidates \
             WHERE stage = ? ORDER BY created_at ASC, id ASC",
        )
        .bind(stage_str(stage))
        .fetch_all(pool)
        .await?;
        decode_snapshots(rows)
    }

    /// Every candidate (any stage, any version) for one named artifact, oldest
    /// first — the promotion history of `kind/name`.
    pub async fn list_by_artifact(
        &self,
        pool: &SqlitePool,
        kind: ArtifactKind,
        name: &str,
    ) -> Result<Vec<CandidateSnapshot>, PromotionStoreError> {
        let rows = sqlx::query(
            "SELECT id, candidate_json FROM promotion_candidates \
             WHERE artifact_kind = ? AND artifact_name = ? ORDER BY created_at ASC, id ASC",
        )
        .bind(kind.as_str())
        .bind(name)
        .fetch_all(pool)
        .await?;
        decode_snapshots(rows)
    }

    /// The currently active version of an artifact stem (`router/tool-selection`),
    /// if any has ever been activated.
    pub async fn active_version(
        &self,
        pool: &SqlitePool,
        stem: &str,
    ) -> Result<Option<u32>, PromotionStoreError> {
        let row = sqlx::query(
            "SELECT version FROM promotion_active_versions \
             WHERE stem = ? ORDER BY position DESC LIMIT 1",
        )
        .bind(stem)
        .fetch_optional(pool)
        .await?;
        row.map(|row| {
            u32::try_from(row.get::<i64, _>("version")).map_err(|_| {
                PromotionStoreError::Corrupt(format!("negative active version for {stem}"))
            })
        })
        .transpose()
    }
}

// --- internal helpers --------------------------------------------------------

fn draft_candidate(
    artifact: ArtifactVersion,
    author: &Actor,
    requires_permission_review: bool,
) -> Candidate {
    let candidate = Candidate::draft(artifact, author);
    if requires_permission_review {
        candidate.needs_permission_review()
    } else {
        candidate
    }
}

/// The wire string for a stage — matches the `#[serde(rename_all =
/// "kebab-case")]` already declared on [`PromotionStage`], kept as a small
/// local match (rather than round-tripping through `serde_json`) because the
/// store never needs to parse it back: the authoritative stage always comes
/// from deserializing `candidate_json` (see the module doc's no-back-door
/// note), so this TEXT column is write-only from the store's perspective,
/// purely for `WHERE stage = ?` queries.
fn stage_str(stage: PromotionStage) -> &'static str {
    match stage {
        PromotionStage::Draft => "draft",
        PromotionStage::RegressionPassed => "regression-passed",
        PromotionStage::Shadow => "shadow",
        PromotionStage::Canary => "canary",
        PromotionStage::ComparisonReady => "comparison-ready",
        PromotionStage::Promoted => "promoted",
        PromotionStage::RolledBack => "rolled-back",
        PromotionStage::Rejected => "rejected",
    }
}

async fn insert_candidate(
    pool: &SqlitePool,
    id: &str,
    candidate: &Candidate,
) -> Result<(), PromotionStoreError> {
    let now = Utc::now().to_rfc3339();
    let json = serde_json::to_string(candidate)?;
    sqlx::query(
        "INSERT INTO promotion_candidates \
         (id, artifact_kind, artifact_name, artifact_version, stage, candidate_json, \
          created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(candidate.artifact().kind.as_str())
    .bind(&candidate.artifact().name)
    .bind(i64::from(candidate.artifact().version))
    .bind(stage_str(candidate.stage()))
    .bind(&json)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

fn decode_candidate(row: &sqlx::sqlite::SqliteRow) -> Result<Candidate, PromotionStoreError> {
    let json: String = row.get("candidate_json");
    Ok(serde_json::from_str(&json)?)
}

fn decode_snapshots(
    rows: Vec<sqlx::sqlite::SqliteRow>,
) -> Result<Vec<CandidateSnapshot>, PromotionStoreError> {
    rows.into_iter()
        .map(|row| {
            let id: String = row.get("id");
            let candidate = decode_candidate(&row)?;
            Ok(CandidateSnapshot { id, candidate })
        })
        .collect()
}

/// Load a candidate INSIDE a transaction, for a load-mutate-persist sequence.
/// Not exposed outside the module: every caller must follow it with
/// [`save_candidate`] in the same transaction (the no-back-door discipline —
/// see the module doc).
async fn load_for_update(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Candidate, PromotionStoreError> {
    let row = sqlx::query("SELECT candidate_json FROM promotion_candidates WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| PromotionStoreError::NotFound(id.to_string()))?;
    decode_candidate(&row)
}

/// Persist `candidate`'s CURRENT (post-mutation) state — the only write path
/// for the `stage` column, always derived from the value just returned by a
/// real `Candidate` state-machine method.
async fn save_candidate(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    candidate: &Candidate,
) -> Result<(), PromotionStoreError> {
    let json = serde_json::to_string(candidate)?;
    let now = Utc::now().to_rfc3339();
    let affected = sqlx::query(
        "UPDATE promotion_candidates SET stage = ?, candidate_json = ?, updated_at = ? WHERE id = ?",
    )
    .bind(stage_str(candidate.stage()))
    .bind(&json)
    .bind(&now)
    .bind(id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(PromotionStoreError::NotFound(id.to_string()));
    }
    Ok(())
}

/// Append one audit-trail row for a minted [`PromotionRecord`] (a promotion or
/// a rollback, manual or system-auto).
async fn append_event(
    tx: &mut Transaction<'_, Sqlite>,
    candidate_id: &str,
    record: &PromotionRecord,
) -> Result<(), PromotionStoreError> {
    let id = Uuid::now_v7().to_string();
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO promotion_events \
         (id, candidate_id, artifact_kind, artifact_name, artifact_version, actor_kind, stage, \
          reason, occurred_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(candidate_id)
    .bind(record.artifact().kind.as_str())
    .bind(&record.artifact().name)
    .bind(i64::from(record.artifact().version))
    .bind(record.actor_kind())
    .bind(stage_str(record.stage()))
    .bind(record.reason())
    .bind(&now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Activate a version for `stem`, mirroring `ActiveVersions::activate`:
/// idempotent when the version is already on top (never pushes a duplicate —
/// the `[11, 12, 12]` corruption a prior defect batch fixed in the in-memory
/// type), otherwise appends it at the next position.
async fn activate(
    tx: &mut Transaction<'_, Sqlite>,
    stem: &str,
    version: u32,
) -> Result<(), PromotionStoreError> {
    let top = sqlx::query(
        "SELECT position, version FROM promotion_active_versions \
         WHERE stem = ? ORDER BY position DESC LIMIT 1",
    )
    .bind(stem)
    .fetch_optional(&mut **tx)
    .await?;
    let next_position = match top {
        Some(row) => {
            let existing_version: i64 = row.get("version");
            if existing_version == i64::from(version) {
                return Ok(()); // already active — idempotent, no duplicate row.
            }
            row.get::<i64, _>("position") + 1
        }
        None => 0,
    };
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO promotion_active_versions (stem, position, version, activated_at) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(stem)
    .bind(next_position)
    .bind(i64::from(version))
    .bind(&now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Roll back `stem` to its predecessor, mirroring `ActiveVersions::rollback`:
/// pop the top position; a stem with fewer than two positions has no
/// predecessor to restore, so this is a no-op (matches the in-memory type).
async fn deactivate(
    tx: &mut Transaction<'_, Sqlite>,
    stem: &str,
) -> Result<(), PromotionStoreError> {
    let rows = sqlx::query(
        "SELECT position FROM promotion_active_versions WHERE stem = ? ORDER BY position DESC LIMIT 2",
    )
    .bind(stem)
    .fetch_all(&mut **tx)
    .await?;
    if rows.len() < 2 {
        return Ok(()); // nothing to restore to.
    }
    let top_position: i64 = rows[0].get("position");
    sqlx::query("DELETE FROM promotion_active_versions WHERE stem = ? AND position = ?")
        .bind(stem)
        .bind(top_position)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// A deterministic candidate id derived from a command's idempotency key
/// (mirrors `codypendent_workflow::store`'s `deterministic_run_id`), so a
/// duplicate `ProposePromotion` delivery resolves to the same candidate.
fn deterministic_candidate_id(idempotency_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"promotion-candidate\x00");
    hasher.update(idempotency_key.as_bytes());
    format!("cand-{}", hex::encode(&hasher.finalize()[..16]))
}
