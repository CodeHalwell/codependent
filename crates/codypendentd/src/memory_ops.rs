//! Daemon-side handlers for inspecting, correcting, forgetting, and opening
//! the source of a memory (2026-08-13 review, memory-docs vertical, F3/F4).
//!
//! `MemoryStore::forget`/`forget_scope`/`correct` (`codypendent-knowledge`)
//! already implement the Chapter 06 inspect/edit/delete triad in full, and
//! [`provenance_cards`](codypendent_knowledge::provenance_cards) already
//! projects "every retrieved memory opens its source" — but nothing a client
//! can reach ever called any of them: `crates/protocol/src/command.rs` has no
//! memory command at all, and the TUI's memory surface reads SQLite directly,
//! read-only.
//!
//! This module is the daemon-side half a protocol command can call once one
//! exists: it adds the ONE thing the bare store methods deliberately do
//! **not** do — verifying the target memory (or scope) actually falls inside
//! the caller's visible scopes before acting — and resolves an
//! [`EvidenceRef`] to the real content behind a provenance card. Protocol and
//! TUI wiring are proposed in
//! `.impl/proposals/agent-security-from-agent-memory.md` and
//! `.impl/proposals/agent-tui-from-agent-memory.md`.
//!
//! ## No enumeration oracle
//!
//! A memory outside the caller's visible scopes is refused **identically** to
//! one that does not exist at all (`MemoryError::NotFound`), for
//! [`inspect`], [`correct`], and [`forget`] alike — a client must never be
//! able to use these calls as an oracle for "does memory X exist in a
//! repository I cannot see" (brief rule 2: a check that gates access must be
//! enforced where the resource is fetched, and must fail identically for "not
//! allowed" and "does not exist"). [`forget_scope`] is the one exception: the
//! caller already names the scope outright, so refusing a scope it may not
//! touch leaks nothing new.

use codypendent_daemon::artifacts::ArtifactStore;
use codypendent_daemon::ledger;
use codypendent_knowledge::{
    local_user_scope, EvidenceRef, ForgetAudit, MemoryCorrection, MemoryError, MemoryRecord,
    MemoryStore, Scope,
};
use codypendent_protocol::{MemoryId, RepositoryId, SessionEvent};
use sqlx::SqlitePool;
use tokio::io::AsyncReadExt;

/// The scopes a memory inspect/edit/delete request may act on for a daemon
/// serving `repository` — byte-for-byte the same widened set
/// [`emit_context`](crate::executor::RuntimeExecutor) builds
/// (`[System, local_user_scope(), Repository(repository)]`). Callers use this
/// rather than re-deriving their own scope list specifically to avoid the
/// class of bug this same review found one layer up (F1: two call sites
/// computing a repository identity two different ways and silently
/// disagreeing) — every caller of this module gets "what may I touch" from
/// this one function.
#[must_use]
pub fn visible_scopes(repository: RepositoryId) -> Vec<Scope> {
    vec![
        Scope::System,
        local_user_scope(),
        Scope::Repository(repository),
    ]
}

/// Whether `record` is inside `visible_scopes` — the one check a bare
/// `MemoryStore` method cannot make on its own (the store has no notion of
/// "caller").
fn in_scope(record: &MemoryRecord, visible_scopes: &[Scope]) -> bool {
    visible_scopes.contains(&record.scope)
}

/// Inspect one memory, if it is visible to the caller. `Ok(None)` for both
/// "does not exist" and "exists outside `visible_scopes`" — see the module
/// docs.
pub async fn inspect(
    pool: &SqlitePool,
    id: MemoryId,
    visible_scopes: &[Scope],
) -> Result<Option<MemoryRecord>, MemoryError> {
    Ok(MemoryStore::new()
        .get(pool, id)
        .await?
        .filter(|record| in_scope(record, visible_scopes)))
}

/// Correct a live, in-scope memory (see
/// [`MemoryStore::correct`](codypendent_knowledge::MemoryStore::correct)).
/// Refuses with [`MemoryError::NotFound`] identically whether `id` is absent,
/// historical (already superseded), or simply outside `visible_scopes`.
pub async fn correct(
    pool: &SqlitePool,
    id: MemoryId,
    visible_scopes: &[Scope],
    correction: MemoryCorrection,
) -> Result<MemoryRecord, MemoryError> {
    let store = MemoryStore::new();
    let visible = store
        .get(pool, id)
        .await?
        .is_some_and(|record| in_scope(&record, visible_scopes));
    if !visible {
        return Err(MemoryError::NotFound(id));
    }
    store.correct(pool, id, correction).await
}

/// Forget one memory (see
/// [`MemoryStore::forget`](codypendent_knowledge::MemoryStore::forget)).
/// Refuses with [`MemoryError::NotFound`] identically whether `id` is absent
/// or simply outside `visible_scopes` — never distinguished.
pub async fn forget(
    pool: &SqlitePool,
    id: MemoryId,
    visible_scopes: &[Scope],
) -> Result<ForgetAudit, MemoryError> {
    let store = MemoryStore::new();
    let visible = store
        .get(pool, id)
        .await?
        .is_some_and(|record| in_scope(&record, visible_scopes));
    if !visible {
        return Err(MemoryError::NotFound(id));
    }
    store.forget(pool, id).await
}

/// Forget every memory in `scope` (see
/// [`MemoryStore::forget_scope`](codypendent_knowledge::MemoryStore::forget_scope)).
/// Refuses a `scope` the caller may not touch with [`MemoryError::Policy`] —
/// the caller already names the scope outright, so refusing accurately here
/// leaks nothing a client did not already assert.
pub async fn forget_scope(
    pool: &SqlitePool,
    scope: Scope,
    visible_scopes: &[Scope],
) -> Result<ForgetAudit, MemoryError> {
    if !visible_scopes.contains(&scope) {
        return Err(MemoryError::Policy(
            "scope is not visible to this caller".to_string(),
        ));
    }
    MemoryStore::new().forget_scope(pool, &scope).await
}

/// What [`open_evidence`] resolves an [`EvidenceRef`] to — the content behind
/// a provenance card. Closes Chapter 06's "every retrieved memory opens its
/// source" exit criterion (2026-08-13 review F4):
/// [`provenance_cards`](codypendent_knowledge::provenance_cards) already
/// projects the card, but nothing ever fetched the artifact/event range it
/// names — both existing display surfaces stopped at an opaque id string.
#[derive(Debug, Clone, PartialEq)]
pub enum EvidenceContent {
    /// The session ledger events in `[from_sequence, to_sequence]` (both ends
    /// inclusive, matching how `EvidenceRef::EventRange` is constructed
    /// throughout the fabric — see `crate::observer`).
    Events(Vec<SessionEvent>),
    /// The stored artifact's raw bytes and media type.
    Artifact { media_type: String, bytes: Vec<u8> },
}

/// Resolve one [`EvidenceRef`] to its actual content: the session ledger
/// events it names, or the artifact bytes it names. This is the fetch a
/// client's "open source" action performs once wired to a protocol command —
/// today nothing calls it, so a `RevealSource` action can render only the
/// evidence ref's own opaque ids.
pub async fn open_evidence(
    pool: &SqlitePool,
    artifacts: &ArtifactStore,
    evidence: &EvidenceRef,
) -> anyhow::Result<EvidenceContent> {
    match evidence {
        EvidenceRef::EventRange {
            session_id,
            from_sequence,
            to_sequence,
        } => {
            // `load_events_between`'s contract is `after < sequence <= through`
            // (exclusive start); `from_sequence` is inclusive, so the lower
            // bound is shifted down by one to include it.
            let events = ledger::load_events_between(
                pool,
                *session_id,
                from_sequence.saturating_sub(1),
                *to_sequence,
            )
            .await?;
            Ok(EvidenceContent::Events(events))
        }
        EvidenceRef::Artifact { artifact, .. } => {
            let mut file = artifacts.open(pool, artifact.id).await?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).await?;
            Ok(EvidenceContent::Artifact {
                media_type: artifact.media_type.clone(),
                bytes,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_daemon::artifacts::Provenance;
    use codypendent_daemon::db;
    use codypendent_knowledge::{CandidateMemory, Curation, MemoryClass};
    use codypendent_protocol::{Actor, DataClassification, EventBody, SessionId};

    async fn temp_pool() -> (tempfile::TempDir, SqlitePool) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pool = db::open_database(&tmp.path().join("codypendent.db"))
            .await
            .expect("open db");
        (tmp, pool)
    }

    fn some_evidence() -> Vec<codypendent_knowledge::EvidenceRef> {
        vec![EvidenceRef::EventRange {
            session_id: SessionId::new(),
            from_sequence: 1,
            to_sequence: 1,
        }]
    }

    /// Insert a live memory directly in `scope` and return its id.
    async fn seed_memory(pool: &SqlitePool, scope: Scope, statement: &str) -> MemoryId {
        let candidate = CandidateMemory {
            class: MemoryClass::Semantic,
            scope: Some(scope),
            statement: statement.to_string(),
            structured_value: None,
            provenance: some_evidence(),
            confidence: 0.8,
            observed_at: chrono::Utc::now(),
            valid_from: codypendent_knowledge::Revision::sequence(1),
            sensitivity: DataClassification::Internal,
            retention: None,
        };
        match MemoryStore::new().curate(pool, candidate).await.unwrap() {
            Curation::Accepted(record) => record.id,
            other => panic!("expected the seed memory to be accepted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn inspect_is_identical_for_absent_and_out_of_scope() {
        let (_tmp, pool) = temp_pool().await;
        let visible_repo = RepositoryId::new();
        let other_repo = RepositoryId::new();
        let visible = visible_scopes(visible_repo);

        let in_scope_id = seed_memory(&pool, Scope::Repository(visible_repo), "a").await;
        let out_of_scope_id = seed_memory(&pool, Scope::Repository(other_repo), "b").await;

        assert!(inspect(&pool, in_scope_id, &visible)
            .await
            .unwrap()
            .is_some());
        assert!(inspect(&pool, out_of_scope_id, &visible)
            .await
            .unwrap()
            .is_none());
        assert!(inspect(&pool, MemoryId::new(), &visible)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn forget_refuses_an_out_of_scope_memory_and_deletes_nothing() {
        let (_tmp, pool) = temp_pool().await;
        let visible_repo = RepositoryId::new();
        let other_repo = RepositoryId::new();
        let visible = visible_scopes(visible_repo);
        let id = seed_memory(
            &pool,
            Scope::Repository(other_repo),
            "secret to another repo",
        )
        .await;

        let error = forget(&pool, id, &visible).await.unwrap_err();
        assert!(matches!(error, MemoryError::NotFound(refused) if refused == id));

        // Nothing was deleted — a scope check that ran AFTER the delete would
        // be a much worse bug than one that never ran at all.
        assert!(MemoryStore::new().get(&pool, id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn forget_removes_an_in_scope_memory() {
        let (_tmp, pool) = temp_pool().await;
        let repo = RepositoryId::new();
        let visible = visible_scopes(repo);
        let id = seed_memory(&pool, Scope::Repository(repo), "safe to forget").await;

        let audit = forget(&pool, id, &visible).await.unwrap();
        assert_eq!(audit.forgotten, vec![id]);
        assert!(MemoryStore::new().get(&pool, id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn correct_refuses_an_out_of_scope_memory() {
        let (_tmp, pool) = temp_pool().await;
        let visible_repo = RepositoryId::new();
        let other_repo = RepositoryId::new();
        let visible = visible_scopes(visible_repo);
        let id = seed_memory(&pool, Scope::Repository(other_repo), "not yours").await;

        let error = correct(
            &pool,
            id,
            &visible,
            MemoryCorrection {
                statement: "rewritten".to_string(),
                structured_value: None,
                provenance: some_evidence(),
                confidence: 0.9,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, MemoryError::NotFound(refused) if refused == id));
    }

    #[tokio::test]
    async fn forget_scope_refuses_a_scope_the_caller_cannot_see() {
        let (_tmp, pool) = temp_pool().await;
        let visible_repo = RepositoryId::new();
        let other_repo = RepositoryId::new();
        let visible = visible_scopes(visible_repo);
        seed_memory(&pool, Scope::Repository(other_repo), "not yours").await;

        let error = forget_scope(&pool, Scope::Repository(other_repo), &visible)
            .await
            .unwrap_err();
        assert!(matches!(error, MemoryError::Policy(_)));
        // The other repository's memory survives untouched.
        assert_eq!(
            MemoryStore::new()
                .query(&pool, &[Scope::Repository(other_repo)], None)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn open_evidence_reads_back_the_real_event_range_and_artifact_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::open_database(&tmp.path().join("codypendent.db"))
            .await
            .unwrap();
        let artifacts = ArtifactStore::new(tmp.path().join("artifacts"));
        let session_id = SessionId::new();
        ledger::create_session(&pool, session_id, "evidence")
            .await
            .unwrap();

        let e1 = ledger::append_next_event(
            &pool,
            session_id,
            &Actor::System,
            &EventBody::NoteAppended {
                text: "first note".to_string(),
                run_id: None,
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        let e2 = ledger::append_next_event(
            &pool,
            session_id,
            &Actor::System,
            &EventBody::NoteAppended {
                text: "second note".to_string(),
                run_id: None,
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        // A third event OUTSIDE the requested range must not come back.
        ledger::append_next_event(
            &pool,
            session_id,
            &Actor::System,
            &EventBody::NoteAppended {
                text: "third note, out of range".to_string(),
                run_id: None,
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();

        let range = EvidenceRef::EventRange {
            session_id,
            from_sequence: e1.sequence,
            to_sequence: e2.sequence,
        };
        match open_evidence(&pool, &artifacts, &range).await.unwrap() {
            EvidenceContent::Events(events) => {
                assert_eq!(
                    events.len(),
                    2,
                    "exactly the requested [from, to]: {events:?}"
                );
                assert_eq!(events[0].sequence, e1.sequence);
                assert_eq!(events[1].sequence, e2.sequence);
            }
            other => panic!("expected Events, got {other:?}"),
        }

        let stored = artifacts
            .put(
                &pool,
                "text/plain",
                DataClassification::Internal,
                Provenance::system("test"),
                b"the real evidence bytes",
            )
            .await
            .unwrap();
        let artifact_ref = EvidenceRef::Artifact {
            artifact: stored,
            source_path: Some("notes.txt".to_string()),
        };
        match open_evidence(&pool, &artifacts, &artifact_ref)
            .await
            .unwrap()
        {
            EvidenceContent::Artifact { media_type, bytes } => {
                assert_eq!(media_type, "text/plain");
                assert_eq!(bytes, b"the real evidence bytes");
            }
            other => panic!("expected Artifact, got {other:?}"),
        }
    }
}
