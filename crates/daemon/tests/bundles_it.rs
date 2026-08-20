//! Export and import against a REAL migrated database.
//!
//! `bundles.rs` had no tests at all, and every query in it named a
//! `session_events` table with `actor_json`/`body_json` columns that no
//! migration has ever created — the ledger is `events`, with `actor`/`body`.
//! `ExportBundle` (with transcripts), `ImportBundle` and
//! `MutateSessionLifecycle::Export` are three shipped, role-gated commands, and
//! all three failed unconditionally the moment they touched a database. The CLI
//! tests exercise a mock socket that never reaches this code.
//!
//! One test that runs the real query against the real schema is the whole
//! difference, so that is what these are.

use codypendent_daemon::{artifacts::ArtifactStore, bundles, db};
use codypendent_protocol::session::{SessionExportFormat, SessionExportOptions};
use codypendent_protocol::SessionId;
use sqlx::SqlitePool;

async fn seed_session_with_events(pool: &SqlitePool, session_id: SessionId, owner_uid: u32) {
    sqlx::query(
        "INSERT INTO sessions \
         (id, title, state, created_at, updated_at, revision, owner_uid) \
         VALUES (?, 'exported session', 'open', ?, ?, 0, ?)",
    )
    .bind(session_id.to_string())
    .bind("2026-08-20T10:00:00Z")
    .bind("2026-08-20T10:00:00Z")
    .bind(i64::from(owner_uid))
    .execute(pool)
    .await
    .expect("seed session");

    // Written through the ledger's real column names, so a query that invents
    // its own cannot pass by accident.
    for (sequence, body) in [
        (
            1_i64,
            r#"{"type":"RunStarted","run_id":"01a01a00-0000-7000-8000-000000000001","objective":"rewrite the parser","mode":{"type":"Build"}}"#,
        ),
        (
            2,
            r#"{"type":"ModelStreamDelta","run_id":"01a01a00-0000-7000-8000-000000000001","text":"working on it"}"#,
        ),
    ] {
        sqlx::query(
            "INSERT INTO events \
             (session_id, sequence, occurred_at, actor, body, schema_version) \
             VALUES (?, ?, ?, ?, ?, 1)",
        )
        .bind(session_id.to_string())
        .bind(sequence)
        .bind("2026-08-20T10:00:00Z")
        .bind(r#"{"type":"System"}"#)
        .bind(body)
        .execute(pool)
        .await
        .expect("seed event");
    }
}

#[tokio::test]
async fn exporting_a_session_reads_the_real_ledger_table() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = db::open_database(&temp.path().join("bundles.db"))
        .await
        .expect("open database");
    let artifacts = ArtifactStore::new(temp.path().join("artifacts"));
    let session_id = SessionId::new();
    seed_session_with_events(&pool, session_id, 1000).await;

    let reference = bundles::export_session_lifecycle(
        &pool,
        &artifacts,
        1000,
        session_id,
        &SessionExportOptions {
            format: SessionExportFormat::Markdown,
            include_artifacts: false,
            include_internal_sessions: false,
        },
    )
    .await
    .expect("export must reach the ledger, not a table nobody created");

    // The transcript really came out, rather than an empty shell.
    let bytes = artifacts
        .read_bytes(&pool, reference.id)
        .await
        .expect("read the artifact");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("rewrite the parser"),
        "the exported transcript must carry the session's events, got: {text}"
    );
}
