//! SQLite (WAL mode) — the authoritative local metadata and event store
//! (ADR-003). Migrations are embedded at compile time from `migrations/` at
//! the repository root and run on every startup.

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

pub async fn open_database(path: &Path) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path) {
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o600);
            let _ = std::fs::set_permissions(path, permissions);
        }
        let wal_path = format!("{}-wal", path.display());
        if let Ok(metadata) = std::fs::metadata(&wal_path) {
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o600);
            let _ = std::fs::set_permissions(&wal_path, permissions);
        }
        let shm_path = format!("{}-shm", path.display());
        if let Ok(metadata) = std::fs::metadata(&shm_path) {
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o600);
            let _ = std::fs::set_permissions(&shm_path, permissions);
        }
    }

    sqlx::migrate!("../../migrations").run(&pool).await?;
    Ok(pool)
}
