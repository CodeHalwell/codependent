//! Persistence and migration compatibility tests.

use codypendent_daemon::{db, instance};
use sqlx::migrate::{MigrationType, Migrator};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::Row;
use std::borrow::Cow;
use std::str::FromStr;

const MIGRATIONS: Migrator = sqlx::migrate!("../../migrations");

async fn pre_0040_fixture(path: &std::path::Path) {
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .expect("fixture database URL")
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("open fixture database");
    let migrations = MIGRATIONS
        .iter()
        .filter(|migration| migration.version < 40)
        .cloned()
        .collect::<Vec<_>>();
    let migrator = Migrator {
        migrations: Cow::Owned(migrations),
        ..Migrator::DEFAULT
    };
    migrator
        .run(&pool)
        .await
        .expect("apply migrations through 0039");

    sqlx::query(
        "INSERT INTO sessions (id, workspace_id, title, state, created_at, updated_at) \
         VALUES ('pre-0040-session', 'workspace-a', 'Release fixture', 'open', \
                 '2026-08-01T00:00:00Z', '2026-08-01T00:00:01Z')",
    )
    .execute(&pool)
    .await
    .expect("seed pre-0040 session");
    sqlx::query(
        "INSERT INTO events \
         (session_id, sequence, occurred_at, actor, body, schema_version) \
         VALUES ('pre-0040-session', 1, '2026-08-01T00:00:01Z', 'fixture', '{}', 1)",
    )
    .execute(&pool)
    .await
    .expect("seed pre-0040 event");
    pool.close().await;
}

async fn table_columns(pool: &sqlx::SqlitePool, table: &str) -> Vec<String> {
    sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(pool)
        .await
        .expect("inspect table")
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect()
}

#[tokio::test]
async fn migration_0054_tracks_null_safe_run_sync_revisions() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let db_path = tmp.path().join("run-sync-revision.db");
    let pool = db::open_database(&db_path).await.expect("open database");
    let now = "2026-08-29T00:00:00Z";

    let applied: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 54 AND success = 1",
    )
    .fetch_one(&pool)
    .await
    .expect("read migration bookkeeping");
    assert_eq!(applied, 1);
    assert!(table_columns(&pool, "runs")
        .await
        .iter()
        .any(|column| column == "sync_revision"));

    sqlx::query(
        "INSERT INTO sessions (id, title, state, created_at, updated_at) \
         VALUES ('sync-revision-session', 'Revision fixture', 'open', ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(&pool)
    .await
    .expect("insert session");
    sqlx::query(
        "INSERT INTO runs \
         (id, session_id, objective, state, mode, model_policy, budget_json) \
         VALUES ('sync-revision-run', 'sync-revision-session', 'fixture', \
                 'Running', 'Build', 'hosted-default', '{}')",
    )
    .execute(&pool)
    .await
    .expect("insert run");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT sync_revision FROM runs WHERE id = 'sync-revision-run'",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );

    sqlx::query("UPDATE runs SET prompt_tokens = 12 WHERE id = 'sync-revision-run'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE runs SET prompt_tokens = 12 WHERE id = 'sync-revision-run'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE runs SET prompt_tokens = NULL WHERE id = 'sync-revision-run'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT sync_revision FROM runs WHERE id = 'sync-revision-run'",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        2,
        "NULL-to-value and value-to-NULL each advance, while an unchanged value does not"
    );
    assert!(
        sqlx::query("UPDATE runs SET sync_revision = -1 WHERE id = 'sync-revision-run'")
            .execute(&pool)
            .await
            .is_err(),
        "the revision must remain nonnegative"
    );
}

#[tokio::test]
async fn instance_identity_survives_restart() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let db_path = tmp.path().join("codypendent.db");

    let pool1 = db::open_database(&db_path)
        .await
        .expect("open db first time");
    let boot1 = instance::record_boot(&pool1).await.expect("first boot");
    assert_eq!(boot1.boot_count, 1);
    pool1.close().await;

    let pool2 = db::open_database(&db_path)
        .await
        .expect("open db second time");
    let boot2 = instance::record_boot(&pool2).await.expect("second boot");

    assert_eq!(
        boot2.instance_id, boot1.instance_id,
        "instance identity must persist"
    );
    assert_eq!(
        boot2.boot_count, 2,
        "boot count must increment across restarts"
    );
}

#[tokio::test]
async fn migration_0040_upgrades_a_pre_0040_database_without_losing_history() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let db_path = tmp.path().join("pre-0040.db");
    pre_0040_fixture(&db_path).await;

    let pool = db::open_database(&db_path)
        .await
        .expect("upgrade pre-0040 database");

    let applied: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 40 AND success = 1",
    )
    .fetch_one(&pool)
    .await
    .expect("read migration bookkeeping");
    assert_eq!(
        applied, 1,
        "the session-library migration must be version 0040"
    );
    let event_body: String = sqlx::query_scalar(
        "SELECT body FROM events WHERE session_id = 'pre-0040-session' AND sequence = 1",
    )
    .fetch_one(&pool)
    .await
    .expect("historical event survives upgrade");
    assert_eq!(event_body, "{}");
}

#[tokio::test]
async fn migration_0040_adds_session_metadata_and_search_source_bookkeeping() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let db_path = tmp.path().join("shape.db");
    let pool = db::open_database(&db_path).await.expect("open database");

    let session_columns = table_columns(&pool, "sessions").await;
    for expected in [
        "internal",
        "parent_session_id",
        "parent_run_id",
        "pinned",
        "archived_at",
        "repository_id",
        "repository",
        "workspace",
        "last_activity_at",
        "last_run_id",
        "run_state",
    ] {
        assert!(
            session_columns.iter().any(|column| column == expected),
            "sessions must contain `{expected}`; actual columns: {session_columns:?}"
        );
    }

    let source_columns = table_columns(&pool, "session_search_sources").await;
    for expected in [
        "session_id",
        "source_type",
        "source_id",
        "content_hash",
        "indexed_at",
    ] {
        assert!(
            source_columns.iter().any(|column| column == expected),
            "session_search_sources must contain `{expected}`; actual columns: {source_columns:?}"
        );
    }

    sqlx::query(
        "INSERT INTO sessions (id, title, state, created_at, updated_at) \
         VALUES ('library-session', 'Library', 'open', '2026-08-01T00:00:00Z', \
                 '2026-08-01T00:00:01Z')",
    )
    .execute(&pool)
    .await
    .expect("legacy-shaped insert uses additive defaults");
    let defaults: (i64, i64, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT internal, pinned, archived_at, tombstoned_at FROM sessions \
         WHERE id = 'library-session'",
    )
    .fetch_one(&pool)
    .await
    .expect("read safe session-library defaults");
    assert_eq!(defaults, (0, 0, None, None));

    sqlx::query(
        "INSERT INTO session_search_sources \
         (session_id, source_type, source_id, content_hash, indexed_at) \
         VALUES ('library-session', 'title', 'session-title', 'sha256:fixed', \
                 '2026-08-01T00:00:01Z')",
    )
    .execute(&pool)
    .await
    .expect("insert stable source bookkeeping");
    let duplicate = sqlx::query(
        "INSERT INTO session_search_sources \
         (session_id, source_type, source_id, content_hash, indexed_at) \
         VALUES ('library-session', 'title', 'session-title', 'sha256:other', \
                 '2026-08-01T00:00:02Z')",
    )
    .execute(&pool)
    .await;
    assert!(duplicate.is_err(), "stable source identity must be unique");
}

#[tokio::test]
async fn migration_0040_is_reopen_idempotent_and_forward_only() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let db_path = tmp.path().join("reopen.db");
    pre_0040_fixture(&db_path).await;

    let first = db::open_database(&db_path).await.expect("first upgrade");
    first.close().await;
    let reopened = db::open_database(&db_path)
        .await
        .expect("reopen upgraded database");

    let applied: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 40")
            .fetch_one(&reopened)
            .await
            .expect("read migration bookkeeping");
    assert_eq!(applied, 1, "reopen must not reapply migration 0040");
    let history_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE session_id = 'pre-0040-session'")
            .fetch_one(&reopened)
            .await
            .expect("read historical rows");
    assert_eq!(
        history_rows, 1,
        "forward migration must not destroy history"
    );

    let migration = MIGRATIONS
        .iter()
        .find(|migration| migration.version == 40)
        .expect("migration 0040 must exist");
    assert!(
        matches!(migration.migration_type, MigrationType::Simple),
        "migration 0040 must be forward-only; destructive down migrations are unsupported"
    );
}
