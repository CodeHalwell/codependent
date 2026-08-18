//! The memory fabric (Chapter 06, STEP 2.4).
//!
//! [`MemoryStore`] is the governed ledger over `memories`. Like [`Registry`], it
//! is stateless — every method takes the [`SqlitePool`] per call — and every
//! authoritative write appends a [`MemoryChanged`](KnowledgeIndexEvent::MemoryChanged)
//! [`outbox`] row **in the same transaction** so an indexer crash can never
//! corrupt the authoritative rows.
//!
//! ## Invariants
//!
//! - **Cross-repository isolation is a SQL filter, never a heuristic.**
//!   [`query`](MemoryStore::query) matches `scope_tier`/`scope_key` in the WHERE
//!   clause; a `Repository(A)` memory can never surface for `Repository(B)`, even
//!   with an identical statement and class.
//! - **A newer fact supersedes, it never deletes.**
//!   [`supersede`](MemoryStore::supersede) stamps the old record's `valid_until`
//!   and inserts the new one with `supersedes = [old]`; a query at the old
//!   revision still returns the old fact.
//! - **Every durable memory carries evidence.** The [`curate`](MemoryStore::curate)
//!   pipeline rejects evidence-free candidates.
//! - **Forgetting leaves no trace of content.** [`forget`](MemoryStore::forget)
//!   deletes the row, writes a tombstone outbox event, and returns a
//!   [`ForgetAudit`] that never contains the deleted statement text. The same
//!   audit is also durably logged to `memory_forget_audits` (never the
//!   statement) so "was X ever forgotten" survives past the call that did it —
//!   see [`forget_audit_log`](MemoryStore::forget_audit_log).
//! - **A correction is a newer fact, not a silent overwrite.**
//!   [`correct`](MemoryStore::correct) — the "edit" half of the Chapter 06
//!   inspect/edit/delete triad (2026-08-13 review F3) — is implemented as an
//!   attributed [`supersede`](MemoryStore::supersede) of the live record: the
//!   corrected statement becomes a new record whose `supersedes` names the one
//!   it replaces, so an edit obeys the exact same "never delete, only
//!   supersede" rule as an automatically detected contradiction. Its
//!   `valid_from` is a canonical sequence-form revision claimed inside the
//!   INSERT, one past every orderable revision in the store: a correction is
//!   visible from its own point in time FORWARD and never backward.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use chrono::{DateTime, Utc};
// Only `UserId` (a plain-string id) is named explicitly; the UUID-backed scope
// ids are reconstructed generically via `parse_scope_id`, their concrete type
// inferred from the `Scope` variant — the same trick the registry uses.
use codypendent_protocol::{DataClassification, MemoryId, UserId};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};

use crate::outbox::{self, KnowledgeIndexEvent};
use crate::types::{
    EvidenceRef, MemoryClass, MemoryRecord, RetentionPolicy, Revision, Scope,
    SEQUENCE_REVISION_DIGITS,
};

/// The columns of `memories` that [`memory_from_row`] reads, in a fixed order
/// shared by the SELECT statements. `scope_json`, `embedding_hash`, and
/// `created_at` are written but not part of the reconstructed [`MemoryRecord`]
/// (the scope is rebuilt from the flattened tier/key, exactly as the registry
/// does), so they are omitted here.
const MEMORY_COLUMNS: &str = "id, class, scope_tier, scope_key, statement, \
     structured_value_json, provenance_json, confidence, observed_at, valid_from, \
     valid_until, supersedes_json, sensitivity, retention_json";

/// The dedup threshold (Chapter 06): a same-scope, same-class candidate more than
/// this similar to a live memory is dropped as a duplicate.
const DEDUP_SIMILARITY: f64 = 0.92;

/// A structured memory error; raw `sqlx`/`serde` failures are wrapped, never
/// surfaced verbatim. Curation *outcomes* (redaction, dedup, supersession,
/// rejection) are values of [`Curation`] on the `Ok` path — this type is only
/// for unrecoverable failures.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// A stored row could not be decoded (should never happen; the store wrote
    /// it).
    #[error("corrupt memory row: {0}")]
    Corrupt(String),
    /// An ordered ("as of revision X") query was given a revision that is not
    /// in canonical sequence form, so it cannot be compared as ordered text
    /// without silently mis-ranking (C11). The caller must pass a
    /// [`Revision::sequence`](crate::types::Revision::sequence) revision or use
    /// the live (`at_revision = None`) view.
    #[error(
        "non-orderable revision {0:?}: ordered memory queries require a canonical \
         sequence-form revision (seq:<zero-padded number>), never a git SHA or label"
    )]
    NonOrderableRevision(String),
    /// The requested memory does not exist — or, from a scope-checking caller
    /// (e.g. a daemon handler), exists but is outside the caller's visible
    /// scopes. The two are deliberately indistinguishable: a client must never
    /// be able to use an inspect/edit/delete call as an oracle for "does
    /// memory X exist in a repository I cannot see".
    #[error("memory {0} was not found")]
    NotFound(MemoryId),
    /// A requested mutation itself violates store policy (e.g. an
    /// evidence-free [`correct`](MemoryStore::correct) call, held to the same
    /// provenance bar as [`curate`](MemoryStore::curate)).
    #[error("memory policy rejected the request: {0}")]
    Policy(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
}

/// The governed memory ledger over `memories`. Stateless: the pool is passed to
/// each method rather than held (mirrors [`Registry`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct MemoryStore;

impl MemoryStore {
    /// A memory-store handle.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Insert `record`, appending a `MemoryChanged` outbox row in the same
    /// transaction.
    pub async fn insert(
        &self,
        pool: &SqlitePool,
        record: &MemoryRecord,
    ) -> Result<(), MemoryError> {
        let now = Utc::now();
        let mut tx = pool.begin().await?;
        insert_row(&mut *tx, record, now).await?;
        outbox::enqueue(
            &mut *tx,
            &KnowledgeIndexEvent::MemoryChanged(record.id),
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Fetch a memory by id (superseded records included — supersession never
    /// deletes).
    pub async fn get(
        &self,
        pool: &SqlitePool,
        id: MemoryId,
    ) -> Result<Option<MemoryRecord>, MemoryError> {
        let row = sqlx::query(&format!(
            "SELECT {MEMORY_COLUMNS} FROM memories WHERE id = ?"
        ))
        .bind(id.to_string())
        .fetch_optional(pool)
        .await?;
        row.as_ref().map(memory_from_row).transpose()
    }

    /// Every memory in the given `scopes` that is valid at the requested
    /// revision.
    ///
    /// **Cross-scope isolation is enforced here, in SQL.** Only rows whose
    /// `(scope_tier, scope_key)` is one of `scopes` are considered — a
    /// `Repository(A)` memory is invisible to a `Repository(B)` query.
    ///
    /// Temporal validity:
    /// - `at_revision = Some(rev)` returns records with
    ///   `valid_from <= rev AND (valid_until IS NULL OR rev < valid_until)` —
    ///   i.e. the record that was live at that revision, superseded or not.
    ///   **`rev` must be in canonical sequence form**
    ///   ([`Revision::sequence`](crate::types::Revision::sequence)): the
    ///   comparison is a SQL text-range operation, which is only meaningful for
    ///   the fixed-width zero-padded `seq:` form. An opaque git SHA or logical
    ///   label would mis-compare silently, so it is rejected up front with
    ///   [`MemoryError::NonOrderableRevision`] rather than returning wrong rows
    ///   (C11).
    /// - `at_revision = None` returns only currently-live records
    ///   (`valid_until IS NULL`).
    ///
    /// An empty `scopes` slice matches nothing.
    pub async fn query(
        &self,
        pool: &SqlitePool,
        scopes: &[Scope],
        at_revision: Option<&Revision>,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        // Fail closed on a revision that cannot be ordered by text comparison,
        // before touching the database — a SHA reaching the `valid_from <= ?`
        // range operator is the silent mis-comparison this guards against.
        if let Some(rev) = at_revision {
            if !rev.is_sequence_form() {
                return Err(MemoryError::NonOrderableRevision(rev.0.clone()));
            }
        }

        if scopes.is_empty() {
            return Ok(Vec::new());
        }

        // Build the scope disjunction: each scope contributes a
        // `(scope_tier = ? AND scope_key = ?)` (or `... IS NULL`) clause. This is
        // the SQL cross-repository isolation filter — never a post-hoc heuristic.
        let mut sql = format!("SELECT {MEMORY_COLUMNS} FROM memories WHERE (");
        for (i, scope) in scopes.iter().enumerate() {
            if i > 0 {
                sql.push_str(" OR ");
            }
            match scope.key() {
                Some(_) => sql.push_str("(scope_tier = ? AND scope_key = ?)"),
                None => sql.push_str("(scope_tier = ? AND scope_key IS NULL)"),
            }
        }
        sql.push(')');
        match at_revision {
            Some(_) => {
                sql.push_str(" AND valid_from <= ? AND (valid_until IS NULL OR ? < valid_until)");
            }
            None => sql.push_str(" AND valid_until IS NULL"),
        }
        sql.push_str(" ORDER BY created_at ASC, id ASC");

        let mut q = sqlx::query(&sql);
        for scope in scopes {
            q = q.bind(scope.tier());
            if let Some(key) = scope.key() {
                q = q.bind(key);
            }
        }
        if let Some(rev) = at_revision {
            q = q.bind(rev.0.clone()).bind(rev.0.clone());
        }
        let rows = q.fetch_all(pool).await?;
        let now = Utc::now();
        let mut seen = HashSet::new();
        rows.iter()
            .map(memory_from_row)
            .filter_map(|record| match record {
                Ok(mut record)
                    if memory_is_retained(&record, now)
                        && low_value_memory_reason(record.class, &record.statement).is_none() =>
                {
                    normalize_automatic_memory(&mut record);
                    // Historical versions included a fresh run UUID in every
                    // failure breadcrumb, defeating fuzzy dedup and filling the
                    // browser with the same lesson. Collapse those rows at the
                    // read boundary as well as preventing new duplicates below.
                    let key = format!(
                        "{}:{:?}:{:?}:{}",
                        record.scope.tier(),
                        record.scope.key(),
                        record.class,
                        record.statement.to_ascii_lowercase()
                    );
                    seen.insert(key).then_some(Ok(record))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    /// Supersede `old_id` with `new`: stamp the old record's `valid_until` to
    /// `new.valid_from` and insert `new` with `old_id` recorded in its
    /// `supersedes`. The old row is **never deleted** — a query at the old
    /// revision still returns it. Both writes plus both outbox events land in one
    /// transaction. Returns the stored (`supersedes`-augmented) record.
    pub async fn supersede(
        &self,
        pool: &SqlitePool,
        old_id: MemoryId,
        mut new: MemoryRecord,
    ) -> Result<MemoryRecord, MemoryError> {
        if !new.supersedes.contains(&old_id) {
            new.supersedes.push(old_id);
        }
        let now = Utc::now();
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE memories SET valid_until = ? WHERE id = ?")
            .bind(new.valid_from.0.as_str())
            .bind(old_id.to_string())
            .execute(&mut *tx)
            .await?;
        insert_row(&mut *tx, &new, now).await?;
        outbox::enqueue(&mut *tx, &KnowledgeIndexEvent::MemoryChanged(old_id), now).await?;
        outbox::enqueue(&mut *tx, &KnowledgeIndexEvent::MemoryChanged(new.id), now).await?;
        tx.commit().await?;
        Ok(new)
    }

    /// Forget a single memory: delete the row, enqueue a `MemoryChanged`
    /// tombstone (which drops it from the derived indexes when the indexer sees a
    /// change event for an id no longer in `memories`), durably log a
    /// content-free [`ForgetAudit`] row (see [`forget_audit_log`](Self::forget_audit_log)),
    /// and return that same audit summary — which records *which* id was
    /// removed but never its statement text.
    pub async fn forget(
        &self,
        pool: &SqlitePool,
        id: MemoryId,
    ) -> Result<ForgetAudit, MemoryError> {
        let now = Utc::now();
        let mut tx = pool.begin().await?;
        let result = sqlx::query("DELETE FROM memories WHERE id = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        let removed = result.rows_affected() > 0;
        let forgotten = if removed { vec![id] } else { Vec::new() };
        if removed {
            outbox::enqueue(&mut *tx, &KnowledgeIndexEvent::MemoryChanged(id), now).await?;
            insert_forget_audit(&mut *tx, &forgotten, None, now).await?;
        }
        tx.commit().await?;
        Ok(ForgetAudit {
            forgotten,
            scope: None,
            removed_at: now,
        })
    }

    /// Forget every memory in a scope (user/scope deletion): delete the rows,
    /// enqueue one `MemoryChanged` tombstone per removed id, and return the audit
    /// summary (ids + count + scope, never any statement text).
    pub async fn forget_scope(
        &self,
        pool: &SqlitePool,
        scope: &Scope,
    ) -> Result<ForgetAudit, MemoryError> {
        let now = Utc::now();
        let mut tx = pool.begin().await?;

        // Collect the in-scope ids first — for the tombstones and the audit — then
        // delete by id. The scope match is the same SQL isolation filter as
        // `query`, so a scope deletion can never reach across repositories.
        let ids: Vec<String> = match scope.key() {
            Some(key) => {
                sqlx::query_scalar("SELECT id FROM memories WHERE scope_tier = ? AND scope_key = ?")
                    .bind(scope.tier())
                    .bind(key)
                    .fetch_all(&mut *tx)
                    .await?
            }
            None => {
                sqlx::query_scalar(
                    "SELECT id FROM memories WHERE scope_tier = ? AND scope_key IS NULL",
                )
                .bind(scope.tier())
                .fetch_all(&mut *tx)
                .await?
            }
        };

        let mut forgotten = Vec::with_capacity(ids.len());
        for id_str in &ids {
            let id = MemoryId::from_str(id_str)
                .map_err(|e| MemoryError::Corrupt(format!("id `{id_str}`: {e}")))?;
            outbox::enqueue(&mut *tx, &KnowledgeIndexEvent::MemoryChanged(id), now).await?;
            forgotten.push(id);
        }
        for id_str in &ids {
            sqlx::query("DELETE FROM memories WHERE id = ?")
                .bind(id_str)
                .execute(&mut *tx)
                .await?;
        }
        if !forgotten.is_empty() {
            insert_forget_audit(&mut *tx, &forgotten, Some(scope), now).await?;
        }
        tx.commit().await?;
        Ok(ForgetAudit {
            forgotten,
            scope: Some(scope.clone()),
            removed_at: now,
        })
    }

    /// Correct a **live** memory's statement/value from a client's explicit
    /// edit (the "edit" half of Chapter 06's inspect/edit/delete triad —
    /// 2026-08-13 review F3: memory could be inspected and, via
    /// [`forget`](Self::forget), deleted, but never edited). Implemented as an
    /// attributed [`supersede`](Self::supersede): `id` must currently be the
    /// LIVE record (`valid_until IS NULL`) — a superseded/historical id, or one
    /// that never existed, is refused with [`MemoryError::NotFound`] rather
    /// than resurrected or silently rewritten, exactly as an automatically
    /// detected contradiction is never a silent overwrite. The correction's
    /// own [`EvidenceRef`]s are required — an edit is held to the same
    /// provenance bar as any other durable memory ([`MemoryError::Policy`] when
    /// empty).
    ///
    /// **The correction's `valid_from` is claimed, in canonical sequence form,
    /// inside its own INSERT.** A correction carries no session-ledger sequence
    /// of its own, and the obvious stand-in — an opaque `edit:<uuid>` — is
    /// worse than useless here: `valid_from`/`valid_until` are compared *as
    /// text* by [`query`](Self::query), `"e" < "s"`, and so an `edit:` revision
    /// sorts BEFORE every `seq:` revision. That inverted the store's headline
    /// invariant in both directions at once — the corrected fact appeared in
    /// every historical as-of query and the fact it replaced appeared in none.
    /// The claim is `MAX(sequence-form revision in the store) + 1`, evaluated
    /// in the INSERT itself (the [`crate`]-wide shape: a sequence is never
    /// read, then written), so the correction is visible from its own point in
    /// time FORWARD and never backward, and two concurrent corrections cannot
    /// claim one revision. Revisions that carry no order (an opaque SHA, a
    /// pre-fix `edit:` row) are excluded from the MAX — they cannot contribute
    /// an ordering they do not have.
    pub async fn correct(
        &self,
        pool: &SqlitePool,
        id: MemoryId,
        correction: MemoryCorrection,
    ) -> Result<MemoryRecord, MemoryError> {
        if correction.provenance.is_empty() {
            return Err(MemoryError::Policy(
                "a correction must carry at least one evidence ref".to_string(),
            ));
        }
        let now = Utc::now();
        // One IMMEDIATE transaction for read + claim + supersede: the liveness
        // check and the revision it hands out have to see the same store, and
        // the write lock is taken up front rather than upgraded mid-transaction.
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query(&format!(
            "SELECT {MEMORY_COLUMNS} FROM memories WHERE id = ?"
        ))
        .bind(id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        let existing = row
            .as_ref()
            .map(memory_from_row)
            .transpose()?
            .filter(|record| record.valid_until.is_none())
            .ok_or(MemoryError::NotFound(id))?;
        let mut new = MemoryRecord {
            id: MemoryId::new(),
            class: existing.class,
            scope: existing.scope.clone(),
            statement: correction.statement,
            structured_value: correction.structured_value,
            provenance: correction.provenance,
            confidence: correction.confidence,
            observed_at: now,
            // Placeholder only: `insert_row_claiming_revision` ignores this
            // field and returns the revision the INSERT actually claimed.
            valid_from: Revision(String::new()),
            valid_until: None,
            supersedes: vec![id],
            sensitivity: existing.sensitivity,
            retention: existing.retention.clone(),
        };
        new.valid_from = insert_row_claiming_revision(&mut *tx, &new, now).await?;
        // The superseded record's bound is the correction's own revision, so a
        // query strictly before it still returns the original fact.
        sqlx::query("UPDATE memories SET valid_until = ? WHERE id = ?")
            .bind(new.valid_from.0.as_str())
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        outbox::enqueue(&mut *tx, &KnowledgeIndexEvent::MemoryChanged(id), now).await?;
        outbox::enqueue(&mut *tx, &KnowledgeIndexEvent::MemoryChanged(new.id), now).await?;
        tx.commit().await?;
        Ok(new)
    }

    /// The durable, content-free audit trail of every completed
    /// [`forget`](Self::forget)/[`forget_scope`](Self::forget_scope) call,
    /// newest first (Chapter 06: "audit records that do not retain deleted
    /// sensitive content"). Unlike the immediate [`ForgetAudit`] a `forget`
    /// call hands back to its one caller, this survives past that call — the
    /// durable half of the right-to-forget promise, and the read side a future
    /// "was memory X ever forgotten" surface can query.
    pub async fn forget_audit_log(
        &self,
        pool: &SqlitePool,
        limit: i64,
    ) -> Result<Vec<ForgetAudit>, MemoryError> {
        let rows = sqlx::query(
            "SELECT forgotten_ids_json, scope_tier, scope_key, removed_at \
             FROM memory_forget_audits ORDER BY removed_at DESC, id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(pool)
        .await?;
        rows.iter()
            .map(|row| {
                let ids_json: String = row.try_get("forgotten_ids_json")?;
                let forgotten: Vec<MemoryId> = serde_json::from_str(&ids_json)?;
                let scope_tier: Option<String> = row.try_get("scope_tier")?;
                let scope_key: Option<String> = row.try_get("scope_key")?;
                let scope = scope_tier
                    .map(|tier| scope_from_parts(&tier, scope_key.as_deref()))
                    .transpose()?;
                let removed_at: String = row.try_get("removed_at")?;
                Ok(ForgetAudit {
                    forgotten,
                    scope,
                    removed_at: parse_ts(&removed_at, "removed_at")?,
                })
            })
            .collect()
    }

    /// Run a [`CandidateMemory`] through the curator pipeline (Chapter 06). The
    /// gate order is **normative** and every gate runs before the next:
    ///
    /// 1. **secret / sensitivity filter** — a candidate whose statement (or
    ///    structured value) matches a known secret shape or carries a
    ///    high-entropy token is dropped as [`Curation::Redacted`] and logged.
    ///    This runs *first*, before dedup or provenance, so a secret can never
    ///    leak into the ledger regardless of its other properties.
    /// 2. **scope classification** — the candidate's scope is validated
    ///    ([`classify_scope`]); the cross-repository `User` scope is reserved for
    ///    preference-class facts.
    /// 3. **contradiction** — a same-scope, same-class live memory with the same
    ///    subject but an incompatible value is *superseded* (never deleted),
    ///    yielding [`Curation::Superseded`]. Only an evidence-bearing candidate
    ///    may supersede — an evidence-free one falls through to gate 5.
    /// 4. **dedup** — a same-scope, same-class live memory more than
    ///    [`DEDUP_SIMILARITY`] similar drops the candidate as [`Curation::Duplicate`].
    /// 5. **provenance** — a candidate with zero [`EvidenceRef`]s is rejected as
    ///    [`Curation::Rejected`] (`"evidence-free"`).
    /// 6. **retention** — the candidate's [`RetentionPolicy`] (default 365 days)
    ///    is applied and the record inserted, yielding [`Curation::Accepted`].
    pub async fn curate(
        &self,
        pool: &SqlitePool,
        mut candidate: CandidateMemory,
    ) -> Result<Curation, MemoryError> {
        // (a) secret / sensitivity filter — FIRST, before every other gate.
        if let Some(reason) = detect_secret(&candidate.statement) {
            tracing::warn!(target: "codypendent::memory", %reason, "redacted a secret-bearing memory candidate");
            return Ok(Curation::Redacted { reason });
        }
        if let Some(value) = &candidate.structured_value {
            if let Some(reason) = detect_secret(&value.to_string()) {
                tracing::warn!(target: "codypendent::memory", %reason, "redacted a secret-bearing structured value");
                return Ok(Curation::Redacted { reason });
            }
        }
        if matches!(
            candidate.sensitivity,
            DataClassification::Secret | DataClassification::Unknown
        ) {
            return Ok(Curation::Redacted {
                reason: "candidate sensitivity may not be persisted as reusable memory".into(),
            });
        }

        // Quality gate: raw run lifecycle breadcrumbs and opaque digests are
        // audit facts, not reusable knowledge. They remain in the immutable
        // event ledger/chronicle, while durable memory is reserved for facts a
        // future run can actually act on. This gate also protects existing
        // callers that bypass the observer and submit a low-value candidate.
        if let Some(reason) = low_value_memory_reason(candidate.class, &candidate.statement) {
            return Ok(Curation::Rejected { reason });
        }
        normalize_automatic_candidate(&mut candidate);

        // (b) scope classification.
        let scope = match classify_scope(&candidate) {
            Ok(scope) => scope,
            Err(reason) => return Ok(Curation::Rejected { reason }),
        };

        // Live (currently-valid) memories in this scope — dedup and contradiction
        // compare against these only.
        let existing = self.query(pool, std::slice::from_ref(&scope), None).await?;

        // (c) contradiction → supersession. This must precede fuzzy dedup: a
        // tiny value flip often leaves statements >92% similar.
        // supersede; an evidence-free contradiction is left for gate (e) to
        // reject, so a record without provenance is never inserted.
        if !candidate.provenance.is_empty() {
            if let Some(old) = existing
                .iter()
                .find(|m| m.class == candidate.class && memories_contradict(m, &candidate))
            {
                let record = build_record(&candidate, scope);
                let stored = self.supersede(pool, old.id, record).await?;
                return Ok(Curation::Superseded {
                    old_id: old.id,
                    record: stored,
                });
            }
        }

        // (d) dedup only after contradictions have had a chance to supersede.
        for memory in &existing {
            if memory.class == candidate.class
                && trigram_cosine(&memory.statement, &candidate.statement) > DEDUP_SIMILARITY
            {
                return Ok(Curation::Duplicate {
                    existing_id: memory.id,
                });
            }
        }

        // (e) provenance.
        if candidate.provenance.is_empty() {
            return Ok(Curation::Rejected {
                reason: "evidence-free".to_string(),
            });
        }

        // (f) retention + insert.
        let record = build_record(&candidate, scope);
        self.insert(pool, &record).await?;
        Ok(Curation::Accepted(record))
    }
}

fn memory_is_retained(record: &MemoryRecord, now: DateTime<Utc>) -> bool {
    match record.retention.ttl_days {
        None => true,
        Some(days) => record
            .observed_at
            .checked_add_signed(chrono::Duration::days(i64::from(days)))
            .is_some_and(|expires| expires > now),
    }
}

fn low_value_memory_reason(class: MemoryClass, statement: &str) -> Option<String> {
    if let Some(reason) = crate::learning::strict_text_rejection_reason(statement) {
        return Some(reason);
    }
    let lower = statement.trim().to_ascii_lowercase();
    let automatic_run_breadcrumb = class == MemoryClass::Episodic
        && ((lower.starts_with("run ")
            && (lower.contains(" completed:") || lower.contains(" cancelled")))
            || lower.starts_with("applied changeset "));
    if automatic_run_breadcrumb {
        return Some("routine run lifecycle belongs in the chronicle, not memory".to_owned());
    }
    if class == MemoryClass::Procedural && lower.contains("argument digest") {
        return Some("opaque command digests are not reusable procedures".to_owned());
    }
    let generic_reply = [
        "what can i help you with",
        "what would you like to work on",
        "how can i help",
        "i'm ready to help",
        "i am ready to help",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase));
    generic_reply.then(|| "generic assistant greetings are not durable knowledge".to_owned())
}

fn normalize_automatic_candidate(candidate: &mut CandidateMemory) {
    if candidate.class == MemoryClass::Failure {
        candidate.statement = canonical_failure_statement(&candidate.statement);
    }
}

fn normalize_automatic_memory(record: &mut MemoryRecord) {
    if record.class == MemoryClass::Failure {
        record.statement = canonical_failure_statement(&record.statement);
    }
}

fn canonical_failure_statement(statement: &str) -> String {
    let trimmed = statement.trim();
    if let Some(rest) = trimmed.strip_prefix("Run ") {
        if let Some((_, reason)) = rest.split_once(" failed: ") {
            return format!("Run failed: {reason}");
        }
    }
    if let Some((lesson, run)) = trimmed.rsplit_once(" in run ") {
        if run.len() >= 32 && run.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-') {
            return lesson.to_owned();
        }
    }
    trimmed.to_owned()
}

// --------------------------------------------------------------------------
// Candidate + curation result
// --------------------------------------------------------------------------

/// A proposed memory the observer (or the model, via `memory.propose`) hands to
/// the curator. `scope` is optional — the curator's classifier fills or validates
/// it — and `retention` is optional (the default 365-day policy applies when
/// absent).
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateMemory {
    pub class: MemoryClass,
    pub scope: Option<Scope>,
    pub statement: String,
    pub structured_value: Option<serde_json::Value>,
    /// At least one evidence ref is required for acceptance (evidence-free
    /// candidates are rejected at gate (e)).
    pub provenance: Vec<EvidenceRef>,
    pub confidence: f32,
    pub observed_at: DateTime<Utc>,
    pub valid_from: Revision,
    pub sensitivity: DataClassification,
    pub retention: Option<RetentionPolicy>,
}

/// A client's explicit correction to a live memory (see
/// [`MemoryStore::correct`]). Deliberately narrower than [`CandidateMemory`]:
/// `class`/`scope` are inherited from the record being corrected — an edit
/// changes what a memory says, never what kind of fact it is or where it
/// lives (a change of class/scope is a new memory, not a correction of this
/// one).
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryCorrection {
    pub statement: String,
    pub structured_value: Option<serde_json::Value>,
    /// Evidence for THIS correction (e.g. the user's own edit action).
    /// Required — see [`MemoryStore::correct`].
    pub provenance: Vec<EvidenceRef>,
    pub confidence: f32,
}

/// The outcome of running a candidate through the curator pipeline.
#[derive(Debug, Clone, PartialEq)]
pub enum Curation {
    /// The candidate passed every gate and was inserted.
    Accepted(MemoryRecord),
    /// Dropped by the secret / sensitivity filter (gate a).
    Redacted { reason: String },
    /// Dropped as a near-duplicate of `existing_id` (gate c).
    Duplicate { existing_id: MemoryId },
    /// Contradicted `old_id`, which was superseded (gate d). `record` is the
    /// newly-inserted superseding memory.
    Superseded {
        old_id: MemoryId,
        record: MemoryRecord,
    },
    /// Rejected (e.g. `"evidence-free"`, gate e; or an unclassifiable scope).
    Rejected { reason: String },
}

/// The non-content audit record returned by [`MemoryStore::forget`] /
/// [`MemoryStore::forget_scope`]. It records *which* memories were removed and
/// when — but **never** their statement text (Chapter 06: "audit records that do
/// not retain deleted sensitive content").
#[derive(Debug, Clone, PartialEq)]
pub struct ForgetAudit {
    pub forgotten: Vec<MemoryId>,
    /// The scope deleted, for a `forget_scope`; `None` for a single-id forget.
    pub scope: Option<Scope>,
    pub removed_at: DateTime<Utc>,
}

impl ForgetAudit {
    /// How many rows were removed.
    #[must_use]
    pub fn count(&self) -> usize {
        self.forgotten.len()
    }
}

// --------------------------------------------------------------------------
// Provenance projection (Chapter 06 card)
// --------------------------------------------------------------------------

/// One provenance card: the Chapter 06 "every retrieved memory opens its source"
/// projection. A memory with several evidence refs yields one card per ref so the
/// client can render and open each source.
#[derive(Debug, Clone, PartialEq)]
pub struct ProvenanceCard {
    pub statement: String,
    pub source: EvidenceRef,
    pub revision: Revision,
    pub observed: DateTime<Utc>,
    pub scope: Scope,
    pub confidence: f32,
}

/// Project a memory into its provenance cards (one per [`EvidenceRef`]).
#[must_use]
pub fn provenance_cards(record: &MemoryRecord) -> Vec<ProvenanceCard> {
    record
        .provenance
        .iter()
        .map(|source| ProvenanceCard {
            statement: record.statement.clone(),
            source: source.clone(),
            revision: record.valid_from.clone(),
            observed: record.observed_at,
            scope: record.scope.clone(),
            confidence: record.confidence,
        })
        .collect()
}

// --------------------------------------------------------------------------
// Curator internals
// --------------------------------------------------------------------------

/// Build a [`MemoryRecord`] from a candidate and its classified scope, applying
/// the default retention policy when the candidate carries none.
fn build_record(candidate: &CandidateMemory, scope: Scope) -> MemoryRecord {
    MemoryRecord {
        id: MemoryId::new(),
        class: candidate.class,
        scope,
        statement: candidate.statement.clone(),
        structured_value: candidate.structured_value.clone(),
        provenance: candidate.provenance.clone(),
        confidence: candidate.confidence,
        observed_at: candidate.observed_at,
        valid_from: candidate.valid_from.clone(),
        valid_until: None,
        supersedes: Vec::new(),
        sensitivity: candidate.sensitivity,
        retention: candidate.retention.clone().unwrap_or_default(),
    }
}

/// Classify a candidate's scope (pipeline gate b).
///
/// The default home for a fact is `Repository`; the cross-repository `User` scope
/// is reserved for *preferences*. A candidate that already carries a concrete
/// scope keeps it, except a non-preference candidate is never allowed to remain
/// `User`-scoped. A candidate with no anchoring scope cannot be attributed to a
/// repository and is rejected as unclassifiable.
fn classify_scope(candidate: &CandidateMemory) -> Result<Scope, String> {
    match &candidate.scope {
        Some(Scope::User(_)) if candidate.class != MemoryClass::Preference => {
            Err("user scope is reserved for preference-class memories".to_string())
        }
        Some(scope) => Ok(scope.clone()),
        None => Err("candidate carries no scope to anchor the memory".to_string()),
    }
}

/// Whether `existing` and `candidate` are about the same subject but assert
/// incompatible values — the Chapter 06 contradiction signal. Self-contained:
/// each statement is split into `subject` / `value` on the first `is` / `:` / `=`
/// separator; they contradict when the subjects match and the values differ. A
/// statement with no separator is treated as subject-only, so two separator-less
/// statements never spuriously contradict.
fn contradicts(existing: &str, candidate: &str) -> bool {
    let (es, ev) = subject_and_value(existing);
    let (cs, cv) = subject_and_value(candidate);
    // Both must carry a value (a real separator); equal subjects, differing
    // values.
    ev.is_some() && cv.is_some() && !es.is_empty() && es == cs && ev != cv
}

fn memories_contradict(existing: &MemoryRecord, candidate: &CandidateMemory) -> bool {
    // Failure-class statements are canonicalized to a constant "Run failed: "
    // wrapper (`canonical_failure_statement`) before they ever reach here, so
    // literally every failure lesson shares the same first-`": "` subject —
    // "run failed" — regardless of what actually failed. Comparing on that
    // wrapper made ANY two unrelated failures "contradict" (2026-08-13 review
    // F5, reproduced live: two different `no model configured` causes silently
    // superseded one another). Strip the wrapper first so the comparison runs
    // on the SPECIFIC reason — the part that can actually agree or disagree —
    // exactly as it already does for every other memory class.
    let (existing_text, candidate_text) = if existing.class == MemoryClass::Failure {
        (
            failure_reason(&existing.statement),
            failure_reason(&candidate.statement),
        )
    } else {
        (existing.statement.as_str(), candidate.statement.as_str())
    };
    if contradicts(existing_text, candidate_text) {
        return true;
    }
    match (&existing.structured_value, &candidate.structured_value) {
        (Some(old), Some(new)) if old != new => {
            // A structured value attached to the same normalized statement is
            // an explicit value update even when prose has no `is` separator.
            if normalize(&existing.statement) == normalize(&candidate.statement) {
                return true;
            }
            // Common object-shaped facts identify their subject/key separately
            // from `value`; compare identity while allowing the value to change.
            let identity = |value: &serde_json::Value| {
                value.as_object().and_then(|object| {
                    ["subject", "key", "name"]
                        .into_iter()
                        .find_map(|key| object.get(key).map(|value| (key, value.clone())))
                })
            };
            identity(old).is_some() && identity(old) == identity(new)
        }
        _ => false,
    }
}

/// The specific reason inside a canonicalized failure statement
/// (`"Run failed: <reason>"` -> `"<reason>"`), or the statement itself when it
/// carries no such wrapper (e.g. `"shell.run failed"`, which
/// `canonical_failure_statement` leaves alone). Used only to pick the text
/// [`memories_contradict`] compares two Failure-class memories on, so the
/// wrapper every failure shares can never itself read as a shared "subject".
fn failure_reason(statement: &str) -> &str {
    statement.strip_prefix("Run failed: ").unwrap_or(statement)
}

/// Split a statement into a normalized `(subject, value)` pair on the first
/// subject/value separator, or `(whole, None)` when there is none.
fn subject_and_value(statement: &str) -> (String, Option<String>) {
    let normalized = normalize(statement);
    for sep in [" is ", ": ", " = ", "="] {
        if let Some(idx) = normalized.find(sep) {
            let subject = normalized[..idx].trim().to_string();
            let value = normalized[idx + sep.len()..].trim().to_string();
            return (subject, Some(value));
        }
    }
    (normalized, None)
}

// --------------------------------------------------------------------------
// Dedup similarity: self-contained normalized character-trigram cosine
// --------------------------------------------------------------------------

/// Cosine similarity of two statements over their normalized character-trigram
/// frequency vectors, in `[0, 1]`. Self-contained — it does **not** depend on the
/// retrieval module (which may be absent in this worktree). `1.0` for identical
/// normalized text, decreasing as they diverge.
fn trigram_cosine(a: &str, b: &str) -> f64 {
    let va = trigram_counts(a);
    let vb = trigram_counts(b);
    if va.is_empty() || vb.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f64;
    for (gram, &ca) in &va {
        if let Some(&cb) = vb.get(gram) {
            dot += f64::from(ca) * f64::from(cb);
        }
    }
    let norm = |v: &HashMap<String, u32>| -> f64 {
        v.values()
            .map(|&c| f64::from(c) * f64::from(c))
            .sum::<f64>()
            .sqrt()
    };
    let denom = norm(&va) * norm(&vb);
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

/// The character-trigram frequency map of a statement, over its normalized form
/// padded with boundary spaces so short statements still yield trigrams.
fn trigram_counts(statement: &str) -> HashMap<String, u32> {
    let normalized = normalize(statement);
    let padded: Vec<char> = format!("  {normalized}  ").chars().collect();
    let mut counts: HashMap<String, u32> = HashMap::new();
    if padded.len() < 3 {
        counts.insert(padded.iter().collect::<String>(), 1);
        return counts;
    }
    for window in padded.windows(3) {
        *counts.entry(window.iter().collect::<String>()).or_insert(0) += 1;
    }
    counts
}

/// Normalize a statement for comparison: lowercase and collapse runs of
/// whitespace to a single space, trimmed.
fn normalize(statement: &str) -> String {
    let mut out = String::with_capacity(statement.len());
    let mut prev_space = false;
    for c in statement.trim().chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.extend(c.to_lowercase());
            prev_space = false;
        }
    }
    out
}

// --------------------------------------------------------------------------
// Secret / sensitivity filter (no `regex` crate — hand-rolled scanners)
// --------------------------------------------------------------------------

/// Return a redaction reason if `text` looks like it carries a secret. Combines a
/// set of known credential shapes with a Shannon-entropy heuristic over long
/// tokens. `regex` is not a dependency of this crate, so every shape is matched
/// with a small hand-written scanner.
#[must_use]
pub fn detect_secret(text: &str) -> Option<String> {
    if let Some(kind) = matches_known_secret_shape(text) {
        return Some(format!("matched {kind} secret shape"));
    }
    if let Some(entropy) = high_entropy_token(text) {
        return Some(format!(
            "high-entropy token ({entropy:.1} bits/char) resembles a credential"
        ));
    }
    None
}

/// The first known credential shape `text` matches, if any.
fn matches_known_secret_shape(text: &str) -> Option<&'static str> {
    if has_aws_access_key(text) {
        return Some("aws-access-key");
    }
    if has_slack_token(text) {
        return Some("slack-token");
    }
    if has_github_token(text) {
        return Some("github-token");
    }
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY") {
        return Some("private-key");
    }
    if has_secret_assignment(text) {
        return Some("credential-assignment");
    }
    None
}

/// AWS access key id: `AKIA` followed by 16 uppercase-alphanumeric characters.
fn has_aws_access_key(text: &str) -> bool {
    text.match_indices("AKIA").any(|(i, _)| {
        let tail: String = text[i + 4..].chars().take(16).collect();
        tail.len() == 16
            && tail
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    })
}

/// Slack token: `xox[baprs]-` followed by a run of token characters.
fn has_slack_token(text: &str) -> bool {
    for prefix in ["xoxb-", "xoxa-", "xoxp-", "xoxr-", "xoxs-"] {
        if let Some(i) = text.find(prefix) {
            let run = text[i + prefix.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .count();
            if run >= 8 {
                return true;
            }
        }
    }
    false
}

/// GitHub token: `ghp_` or `github_pat_` followed by a long token run.
fn has_github_token(text: &str) -> bool {
    let ghp = text
        .match_indices("ghp_")
        .any(|(i, _)| token_run(&text[i + 4..]) >= 20);
    let pat = text
        .match_indices("github_pat_")
        .any(|(i, _)| token_run(&text[i + 11..]) >= 20);
    ghp || pat
}

/// The length of the leading `[A-Za-z0-9_]` run of `s`.
fn token_run(s: &str) -> usize {
    s.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .count()
}

/// Generic credential assignment: a `api[_-]?key` / `secret` / `token` /
/// `password` keyword (case-insensitive) followed by `:` or `=` and a value.
fn has_secret_assignment(text: &str) -> bool {
    let lower = text.to_lowercase();
    for keyword in [
        "api_key", "api-key", "apikey", "api key", "secret", "token", "password",
    ] {
        let mut from = 0;
        while let Some(rel) = lower[from..].find(keyword) {
            let start = from + rel;
            let after = lower[start + keyword.len()..].trim_start();
            let mut chars = after.chars();
            if matches!(chars.next(), Some(':' | '=')) && !chars.as_str().trim_start().is_empty() {
                return true;
            }
            from = start + keyword.len();
        }
    }
    false
}

/// Return the Shannon entropy of the highest-entropy long token in `text`, when
/// it exceeds the credential threshold. Uniform hex maxes near 4.0 bits/char, so
/// the `> 4.2` cutoff clears content hashes while catching random base64/mixed
/// secrets.
fn high_entropy_token(text: &str) -> Option<f64> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | '(' | ')'))
        .filter_map(|raw| {
            let token = raw.trim_matches(|c: char| {
                !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/'))
            });
            (token.len() >= 24).then(|| shannon_entropy(token))
        })
        .find(|&entropy| entropy > 4.2)
}

/// Shannon entropy (bits/character) of a token.
fn shannon_entropy(token: &str) -> f64 {
    let chars: Vec<char> = token.chars().collect();
    if chars.is_empty() {
        return 0.0;
    }
    let mut counts: HashMap<char, usize> = HashMap::new();
    for &c in &chars {
        *counts.entry(c).or_insert(0) += 1;
    }
    let len = chars.len() as f64;
    counts
        .values()
        .map(|&n| {
            let p = n as f64 / len;
            -p * p.log2()
        })
        .sum()
}

// --------------------------------------------------------------------------
// Row (de)serialization — mirrors the registry's house pattern.
// --------------------------------------------------------------------------

/// Insert one memory row via the caller's executor (so it can share a
/// transaction with the outbox append). `embedding_hash` is the content hash of
/// the normalized statement (the dedup cache key); `created_at` is `now`.
async fn insert_row(
    executor: impl sqlx::SqliteExecutor<'_>,
    record: &MemoryRecord,
    now: DateTime<Utc>,
) -> Result<(), MemoryError> {
    sqlx::query(
        "INSERT INTO memories \
         (id, class, scope_json, scope_tier, scope_key, statement, structured_value_json, \
          provenance_json, confidence, observed_at, valid_from, valid_until, supersedes_json, \
          sensitivity, retention_json, embedding_hash, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(record.id.to_string())
    .bind(enum_as_db(&record.class)?)
    .bind(scope_to_db(&record.scope)?)
    .bind(record.scope.tier())
    .bind(record.scope.key())
    .bind(&record.statement)
    .bind(
        record
            .structured_value
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?,
    )
    .bind(serde_json::to_string(&record.provenance)?)
    .bind(f64::from(record.confidence))
    .bind(record.observed_at.to_rfc3339())
    .bind(&record.valid_from.0)
    .bind(record.valid_until.as_ref().map(|r| r.0.as_str()))
    .bind(serde_json::to_string(&record.supersedes)?)
    .bind(serde_json::to_string(&record.sensitivity)?)
    .bind(serde_json::to_string(&record.retention)?)
    .bind(embedding_hash(&record.statement))
    .bind(now.to_rfc3339())
    .execute(executor)
    .await?;
    Ok(())
}

/// Insert one memory row whose `valid_from` is claimed BY THE INSERT: the next
/// canonical sequence-form revision past every orderable revision already in
/// `memories` (`valid_from` and `valid_until` alike). `record.valid_from` is
/// ignored; the claimed revision is returned.
///
/// The claim is a sub-SELECT inside the statement, mirroring the ledger's
/// `append_next_event`, because a read-then-write would let two concurrent
/// corrections compute the same "next" revision. Rows whose revision is not
/// sequence form (an opaque git SHA, a pre-fix `edit:<uuid>`) are filtered out
/// of the MAX rather than compared: text-comparing them is exactly the silent
/// mis-ranking [`MemoryError::NonOrderableRevision`] exists to refuse.
async fn insert_row_claiming_revision(
    executor: impl sqlx::SqliteExecutor<'_>,
    record: &MemoryRecord,
    now: DateTime<Utc>,
) -> Result<Revision, MemoryError> {
    let digits = SEQUENCE_REVISION_DIGITS;
    // `seq:` + the fixed-width digits; anything of another length, or with a
    // non-digit after the prefix, is not orderable.
    let revision_len = "seq:".len() + digits;
    let claimed: String = sqlx::query_scalar(&format!(
        "INSERT INTO memories \
         (id, class, scope_json, scope_tier, scope_key, statement, structured_value_json, \
          provenance_json, confidence, observed_at, valid_from, valid_until, supersedes_json, \
          sensitivity, retention_json, embedding_hash, created_at) \
         SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, \
                (SELECT printf('seq:%0{digits}d', \
                               COALESCE(MAX(CAST(substr(revision, 5) AS INTEGER)), 0) + 1) \
                   FROM (SELECT valid_from AS revision FROM memories \
                         UNION ALL \
                         SELECT valid_until AS revision FROM memories) \
                  WHERE revision IS NOT NULL \
                    AND length(revision) = {revision_len} \
                    AND substr(revision, 1, 4) = 'seq:' \
                    AND substr(revision, 5) NOT GLOB '*[^0-9]*'), \
                ?, ?, ?, ?, ?, ? \
         RETURNING valid_from"
    ))
    .bind(record.id.to_string())
    .bind(enum_as_db(&record.class)?)
    .bind(scope_to_db(&record.scope)?)
    .bind(record.scope.tier())
    .bind(record.scope.key())
    .bind(&record.statement)
    .bind(
        record
            .structured_value
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?,
    )
    .bind(serde_json::to_string(&record.provenance)?)
    .bind(f64::from(record.confidence))
    .bind(record.observed_at.to_rfc3339())
    .bind(record.valid_until.as_ref().map(|r| r.0.as_str()))
    .bind(serde_json::to_string(&record.supersedes)?)
    .bind(serde_json::to_string(&record.sensitivity)?)
    .bind(serde_json::to_string(&record.retention)?)
    .bind(embedding_hash(&record.statement))
    .bind(now.to_rfc3339())
    .fetch_one(executor)
    .await?;
    Ok(Revision(claimed))
}

/// Insert one durable, content-free row into `memory_forget_audits` (Chapter
/// 06's "audit records that do not retain deleted sensitive content"), inside
/// the caller's transaction so it commits atomically with the delete(s) and
/// tombstone(s) it documents. `scope` is `Some` only for a `forget_scope` call
/// — a single-id `forget` records no scope (mirrors [`ForgetAudit`] itself).
async fn insert_forget_audit(
    executor: impl sqlx::SqliteExecutor<'_>,
    forgotten: &[MemoryId],
    scope: Option<&Scope>,
    removed_at: DateTime<Utc>,
) -> Result<(), MemoryError> {
    sqlx::query(
        "INSERT INTO memory_forget_audits \
         (id, forgotten_ids_json, scope_tier, scope_key, removed_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(serde_json::to_string(forgotten)?)
    .bind(scope.map(Scope::tier))
    .bind(scope.and_then(Scope::key))
    .bind(removed_at.to_rfc3339())
    .execute(executor)
    .await?;
    Ok(())
}

/// Decode a `memories` row (the [`MEMORY_COLUMNS`] projection) into a
/// [`MemoryRecord`]. The scope is rebuilt from the flattened tier/key columns —
/// exactly as [`Registry`] reconstructs its scope.
fn memory_from_row(row: &SqliteRow) -> Result<MemoryRecord, MemoryError> {
    let id: String = row.try_get("id")?;
    let class: String = row.try_get("class")?;
    let scope_tier: String = row.try_get("scope_tier")?;
    let scope_key: Option<String> = row.try_get("scope_key")?;
    let structured_value_json: Option<String> = row.try_get("structured_value_json")?;
    let provenance_json: String = row.try_get("provenance_json")?;
    let confidence: f64 = row.try_get("confidence")?;
    let observed_at: String = row.try_get("observed_at")?;
    let valid_from: String = row.try_get("valid_from")?;
    let valid_until: Option<String> = row.try_get("valid_until")?;
    let supersedes_json: String = row.try_get("supersedes_json")?;
    let sensitivity_json: String = row.try_get("sensitivity")?;
    let retention_json: String = row.try_get("retention_json")?;

    Ok(MemoryRecord {
        id: MemoryId::from_str(&id).map_err(|e| MemoryError::Corrupt(format!("id `{id}`: {e}")))?,
        class: enum_from_db(&class)?,
        scope: scope_from_parts(&scope_tier, scope_key.as_deref())?,
        statement: row.try_get("statement")?,
        structured_value: structured_value_json
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
        provenance: serde_json::from_str(&provenance_json)?,
        confidence: confidence as f32,
        observed_at: parse_ts(&observed_at, "observed_at")?,
        valid_from: Revision(valid_from),
        valid_until: valid_until.map(Revision),
        supersedes: serde_json::from_str(&supersedes_json)?,
        sensitivity: serde_json::from_str(&sensitivity_json)?,
        retention: serde_json::from_str(&retention_json)?,
    })
}

/// The content hash of a memory's normalized statement — the dedup cache key
/// stored in `embedding_hash`.
fn embedding_hash(statement: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(normalize(statement).as_bytes()))
}

/// Serialize a scalar enum to the bare value stored in its column (mirrors the
/// registry's helper). `MemoryClass::Semantic` → `"semantic"`.
fn enum_as_db<T: serde::Serialize>(value: &T) -> Result<String, MemoryError> {
    Ok(serde_json::to_string(value)?.trim_matches('"').to_string())
}

/// Parse a bare column value back into its scalar enum (inverse of
/// [`enum_as_db`]).
fn enum_from_db<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, MemoryError> {
    Ok(serde_json::from_str(&format!("\"{value}\""))?)
}

/// Parse an RFC 3339 timestamp column into a UTC instant.
fn parse_ts(value: &str, field: &str) -> Result<DateTime<Utc>, MemoryError> {
    DateTime::parse_from_rfc3339(value)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| MemoryError::Corrupt(format!("{field} `{value}`: {e}")))
}

/// Encode a [`Scope`] for the `scope_json` column (the tagged `{tier, key}`
/// shape), mirroring the registry.
fn scope_to_db(scope: &Scope) -> Result<String, MemoryError> {
    let value = match scope.key() {
        Some(key) => serde_json::json!({ "tier": scope.tier(), "key": key }),
        None => serde_json::json!({ "tier": scope.tier() }),
    };
    Ok(serde_json::to_string(&value)?)
}

/// Rebuild a [`Scope`] from its flattened `scope_tier` / `scope_key` columns
/// (mirrors the registry).
fn scope_from_parts(tier: &str, key: Option<&str>) -> Result<Scope, MemoryError> {
    let need = |tier: &str| {
        key.ok_or_else(|| MemoryError::Corrupt(format!("scope tier `{tier}` requires a key")))
    };
    let scope = match tier {
        "system" => Scope::System,
        "organization" => Scope::Organization(parse_scope_id(need(tier)?, tier)?),
        "user" => Scope::User(UserId(need(tier)?.to_string())),
        "workspace" => Scope::Workspace(parse_scope_id(need(tier)?, tier)?),
        "repository" => Scope::Repository(parse_scope_id(need(tier)?, tier)?),
        "branch" => Scope::Branch(parse_scope_id(need(tier)?, tier)?),
        "session" => Scope::Session(parse_scope_id(need(tier)?, tier)?),
        "task" => Scope::Task(parse_scope_id(need(tier)?, tier)?),
        other => {
            return Err(MemoryError::Corrupt(format!(
                "unknown scope tier `{other}`"
            )))
        }
    };
    Ok(scope)
}

/// Parse a UUID-backed scope id from its string column, tagging the tier on
/// error.
fn parse_scope_id<T>(value: &str, tier: &str) -> Result<T, MemoryError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    T::from_str(value).map_err(|e| MemoryError::Corrupt(format!("scope {tier} id `{value}`: {e}")))
}

#[cfg(test)]
mod quality_tests {
    use super::*;

    #[test]
    fn routine_run_breadcrumbs_and_generic_greetings_are_not_memories() {
        assert!(low_value_memory_reason(
            MemoryClass::Episodic,
            "Run 019ff1 completed: What can I help you with?"
        )
        .is_some());
        assert!(low_value_memory_reason(
            MemoryClass::Semantic,
            "I'm ready to help. What would you like to work on?"
        )
        .is_some());
        assert!(low_value_memory_reason(
            MemoryClass::Episodic,
            "Applied changeset cs-12 (42 bytes) in run 019ff1"
        )
        .is_some());
    }

    #[test]
    fn durable_decisions_and_failure_lessons_remain_useful() {
        assert!(low_value_memory_reason(
            MemoryClass::Semantic,
            "Use sqlx migrations for every schema change"
        )
        .is_none());
        assert!(
            low_value_memory_reason(MemoryClass::Failure, "Cline requires re-authentication")
                .is_none()
        );
    }

    /// Regression (2026-08-13 review F5, reproduced live): two DIFFERENT
    /// failure reasons must never "contradict" each other merely because
    /// `canonical_failure_statement` wraps every failure in the same
    /// `"Run failed: "` prefix. Verified against the old behaviour first: with
    /// `memories_contradict` comparing the raw (unstripped) statements — i.e.
    /// reverting to `contradicts(&existing.statement, &candidate.statement)` —
    /// this assertion fails, because both statements' first `": "` split
    /// yields the identical subject `"run failed"`.
    #[test]
    fn two_unrelated_failure_reasons_never_contradict() {
        let scope = Scope::Repository(codypendent_protocol::RepositoryId::new());
        let existing = record_with(
            scope.clone(),
            MemoryClass::Failure,
            "Run failed: no model configured (no models.toml)",
        );
        let candidate = candidate_with(
            scope,
            MemoryClass::Failure,
            "Run failed: no model configured: every candidate failed for Build: \
             unreachable-local: connection check to `http://127.0.0.1:59999/v1` failed",
        );
        assert!(
            !memories_contradict(&existing, &candidate),
            "two unrelated failure causes must not collapse into a contradiction"
        );
    }

    /// A GENUINE failure contradiction — the same inner subject asserting a
    /// different value — must still supersede. This is what keeps the F5 fix
    /// from over-correcting into never detecting a real contradiction again.
    #[test]
    fn a_failure_contradiction_about_the_same_inner_subject_still_supersedes() {
        let scope = Scope::Repository(codypendent_protocol::RepositoryId::new());
        let existing = record_with(
            scope.clone(),
            MemoryClass::Failure,
            "Run failed: node version requirement is 18",
        );
        let candidate = candidate_with(
            scope,
            MemoryClass::Failure,
            "Run failed: node version requirement is 20",
        );
        assert!(
            memories_contradict(&existing, &candidate),
            "the same inner subject with a different value must still contradict"
        );
    }

    fn record_with(scope: Scope, class: MemoryClass, statement: &str) -> MemoryRecord {
        MemoryRecord {
            id: codypendent_protocol::MemoryId::new(),
            class,
            scope,
            statement: statement.to_string(),
            structured_value: None,
            provenance: Vec::new(),
            confidence: 0.6,
            observed_at: Utc::now(),
            valid_from: Revision::sequence(1),
            valid_until: None,
            supersedes: Vec::new(),
            sensitivity: DataClassification::Internal,
            retention: RetentionPolicy::default(),
        }
    }

    fn candidate_with(scope: Scope, class: MemoryClass, statement: &str) -> CandidateMemory {
        CandidateMemory {
            class,
            scope: Some(scope),
            statement: statement.to_string(),
            structured_value: None,
            provenance: Vec::new(),
            confidence: 0.6,
            observed_at: Utc::now(),
            valid_from: Revision::sequence(2),
            sensitivity: DataClassification::Internal,
            retention: None,
        }
    }

    #[test]
    fn volatile_run_ids_are_removed_from_failure_lessons() {
        assert_eq!(
            canonical_failure_statement(
                "Run 019ff5ea-f47d-7111-ac9a-666c8ed136cc failed: cline requires re-authentication"
            ),
            "Run failed: cline requires re-authentication"
        );
        assert_eq!(
            canonical_failure_statement(
                "shell.run failed in run 019ff5ea-f47d-7111-ac9a-666c8ed136cc"
            ),
            "shell.run failed"
        );
    }

    /// The store's headline invariant, in the direction a correction used to
    /// break it: `correct` wrote an `edit:<uuid>` revision, which is compared
    /// AS TEXT against the `seq:` form — and `"e" < "s"`, so the correction
    /// sorted before every session revision. The corrected fact then showed up
    /// in EVERY historical as-of query and the fact it replaced in NONE, the
    /// exact inverse of "a query at the old revision still returns the old
    /// fact". Reverting the claim in `correct` fails this test on the first
    /// assertion below.
    #[tokio::test]
    async fn a_correction_is_visible_from_its_own_revision_forward_and_never_backward() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pool = crate::db::open(&tmp.path().join("codypendent.db"))
            .await
            .expect("open");
        let store = MemoryStore::new();
        let scope = Scope::Repository(codypendent_protocol::RepositoryId::new());
        let evidence = || {
            vec![EvidenceRef::EventRange {
                session_id: codypendent_protocol::SessionId::new(),
                from_sequence: 1,
                to_sequence: 2,
            }]
        };
        let original = MemoryRecord {
            id: codypendent_protocol::MemoryId::new(),
            class: MemoryClass::Semantic,
            scope: scope.clone(),
            statement: "the build requires node 18".to_owned(),
            structured_value: None,
            provenance: evidence(),
            confidence: 0.9,
            observed_at: Utc::now(),
            valid_from: Revision::sequence(7),
            valid_until: None,
            supersedes: Vec::new(),
            sensitivity: DataClassification::Internal,
            retention: RetentionPolicy::default(),
        };
        store.insert(&pool, &original).await.expect("insert");

        let corrected = store
            .correct(
                &pool,
                original.id,
                MemoryCorrection {
                    statement: "the build requires node 20".to_owned(),
                    structured_value: None,
                    provenance: evidence(),
                    confidence: 0.95,
                },
            )
            .await
            .expect("correct");

        let statements = |records: Vec<MemoryRecord>| {
            records
                .into_iter()
                .map(|record| record.statement)
                .collect::<Vec<_>>()
        };

        // As of the ORIGINAL revision: the ORIGINAL fact, and only it.
        let at_original = store
            .query(
                &pool,
                std::slice::from_ref(&scope),
                Some(&original.valid_from),
            )
            .await
            .expect("query at the original revision");
        assert_eq!(
            statements(at_original),
            vec!["the build requires node 18".to_owned()],
            "a query at the old revision still returns the old fact"
        );

        // The correction is anchored at an ORDERABLE point past the fact it
        // replaces — not at an `edit:` revision that sorts before everything.
        assert!(
            corrected.valid_from.is_sequence_form(),
            "a correction must claim a canonical sequence-form revision, got {:?}",
            corrected.valid_from
        );
        assert!(
            corrected.valid_from.0 > original.valid_from.0,
            "the correction must be valid from AFTER the fact it replaces: {:?} vs {:?}",
            corrected.valid_from,
            original.valid_from
        );

        // As of the correction's own revision: the CORRECTED fact, and only it.
        let at_correction = store
            .query(
                &pool,
                std::slice::from_ref(&scope),
                Some(&corrected.valid_from),
            )
            .await
            .expect("query at the correction's revision");
        assert_eq!(
            statements(at_correction),
            vec!["the build requires node 20".to_owned()],
            "the correction is visible from its own point in time forward"
        );

        // And the live view is the corrected fact alone.
        let live = store
            .query(&pool, std::slice::from_ref(&scope), None)
            .await
            .expect("live query");
        assert_eq!(
            statements(live),
            vec!["the build requires node 20".to_owned()]
        );
    }

    /// Two corrections in a row keep climbing: the second is valid from a
    /// revision past the first, so each edit is visible only from its own point
    /// in time and the store's history stays a total order.
    #[tokio::test]
    async fn consecutive_corrections_claim_increasing_revisions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pool = crate::db::open(&tmp.path().join("codypendent.db"))
            .await
            .expect("open");
        let store = MemoryStore::new();
        let scope = Scope::Repository(codypendent_protocol::RepositoryId::new());
        let evidence = vec![EvidenceRef::EventRange {
            session_id: codypendent_protocol::SessionId::new(),
            from_sequence: 1,
            to_sequence: 2,
        }];
        let original = MemoryRecord {
            id: codypendent_protocol::MemoryId::new(),
            class: MemoryClass::Semantic,
            scope: scope.clone(),
            statement: "first".to_owned(),
            structured_value: None,
            provenance: evidence.clone(),
            confidence: 0.9,
            observed_at: Utc::now(),
            valid_from: Revision::sequence(42),
            valid_until: None,
            supersedes: Vec::new(),
            sensitivity: DataClassification::Internal,
            retention: RetentionPolicy::default(),
        };
        store.insert(&pool, &original).await.expect("insert");
        let correction = |statement: &str| MemoryCorrection {
            statement: statement.to_owned(),
            structured_value: None,
            provenance: evidence.clone(),
            confidence: 0.9,
        };

        let second = store
            .correct(&pool, original.id, correction("second"))
            .await
            .expect("first correction");
        let third = store
            .correct(&pool, second.id, correction("third"))
            .await
            .expect("second correction");

        assert_eq!(second.valid_from, Revision::sequence(43));
        assert_eq!(third.valid_from, Revision::sequence(44));
        let at_second = store
            .query(
                &pool,
                std::slice::from_ref(&scope),
                Some(&second.valid_from),
            )
            .await
            .expect("query");
        assert_eq!(
            at_second
                .into_iter()
                .map(|record| record.statement)
                .collect::<Vec<_>>(),
            vec!["second".to_owned()],
            "the middle revision sees the middle fact"
        );
    }
}
