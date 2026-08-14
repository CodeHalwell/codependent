//! Opening a migrated SQLite pool for the knowledge fabric.
//!
//! In production the daemon owns the pool and this crate operates on it; this
//! helper exists for the `index rebuild` CLI path and for this crate's own
//! tests, so knowledge never has to depend on `codypendent-daemon` (which
//! depends on knowledge — the same inversion the runtime uses).

use std::path::Path;
use std::str::FromStr;

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

/// Open (creating if absent) the metadata database at `path`, in WAL mode, and
/// run every migration through the head. Mirrors the daemon's `open_database`;
/// the migrations directory is shared at the workspace root.
pub async fn open(path: &Path) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        // Match the daemon's pool: foreign keys ON, so referential integrity
        // (code_edges → code_nodes, document_authorship → documents, …) is
        // enforced here and in the `index rebuild` CLI exactly as in production.
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;
    Ok(pool)
}

/// Open an EXISTING metadata database **read-only**: no creation, no journal
/// change, no migrations, no writes of any kind.
///
/// [`open`] mutates by design — it creates the file if absent, switches it to
/// WAL, and runs every migration through the head. That is right for the owner
/// of the database and wrong for anything merely inspecting it: `codypendent
/// doctor` documents itself as read-only and called [`open`] to run a `COUNT`,
/// so diagnosing a daemon migrated its live database, diagnosing a *missing*
/// database created one, and against a database the user cannot write it
/// reported the graph unreadable where a plain `SELECT` would have answered.
///
/// A caller that only reads uses this. The connection carries
/// `SQLITE_OPEN_READONLY`, so a stray write fails loudly here rather than
/// silently changing a file a diagnostic promised not to touch.
pub async fn open_read_only(path: &Path) -> anyhow::Result<SqlitePool> {
    // Deliberately no `journal_mode`/`synchronous`: both are PRAGMAs that write
    // to the database file, which is exactly what this opener exists to avoid.
    // A WAL database is readable without them.
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(false)
        .read_only(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A read-only open must not conjure a database.** [`open`] carries
    /// `create_if_missing(true)`, so a diagnostic that reached for it created a
    /// daemon database out of the act of asking whether one existed.
    #[tokio::test]
    async fn open_read_only_refuses_a_missing_database_and_creates_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("codypendent.db");
        assert!(open_read_only(&path).await.is_err());
        assert!(
            std::fs::read_dir(dir.path())
                .expect("read dir")
                .next()
                .is_none(),
            "a read-only open left something behind in {}",
            dir.path().display()
        );
    }

    /// **And it must not migrate one either.** A zero-byte file is a valid empty
    /// SQLite database: after a read-only open it is still zero bytes, with no
    /// `-wal`/`-shm` beside it. [`open`] would have run every migration and
    /// switched the journal to WAL — against a database it does not own.
    #[tokio::test]
    async fn open_read_only_neither_migrates_nor_journals_the_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("codypendent.db");
        std::fs::File::create(&path).expect("empty database file");

        let pool = open_read_only(&path).await.expect("open an empty database");
        // The schema is genuinely absent — nothing created it on the way in.
        let missing = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM code_nodes")
            .fetch_one(&pool)
            .await
            .expect_err("an unmigrated database has no code_nodes");
        assert!(
            missing.to_string().contains("code_nodes"),
            "unexpected error: {missing}"
        );
        pool.close().await;

        assert_eq!(std::fs::metadata(&path).expect("still there").len(), 0);
        for suffix in ["-wal", "-shm"] {
            let sidecar = dir.path().join(format!("codypendent.db{suffix}"));
            assert!(!sidecar.exists(), "left {} behind", sidecar.display());
        }
    }
}
