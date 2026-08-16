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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn database_file_is_private_on_creation_and_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("private.db");

        let pool = open_database(&path).await.unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        pool.close().await;

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let reopened = open_database(&path).await.unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        reopened.close().await;
    }
}
