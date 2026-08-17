use codypendent_control_plane::PgStore;
use sqlx::postgres::PgPoolOptions;
use std::path::Path;

#[tokio::test]
async fn control_plane_migrations_files_exist_and_are_ordered() {
    let migrations_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    assert!(migrations_dir.exists(), "migrations dir must exist");

    let expected_files = [
        "0001_identity.sql",
        "0002_organizations.sql",
        "0003_workloads.sql",
        "0004_sync.sql",
        "0005_audit.sql",
    ];

    for file in expected_files {
        let path = migrations_dir.join(file);
        assert!(path.exists(), "Migration file {} must exist", file);
        let content = std::fs::read_to_string(&path).expect("failed to read migration file");
        assert!(
            !content.trim().is_empty(),
            "Migration {} must not be empty",
            file
        );
    }
}

#[tokio::test]
async fn control_plane_migrations_apply_to_an_empty_database() {
    let db_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set; skipping live PostgreSQL migration execution");
            return;
        }
    };

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to PostgreSQL");

    let pg_store = PgStore::new(pool);
    // 1. Initial migration run
    pg_store
        .run_migrations()
        .await
        .expect("Failed to apply migrations to PostgreSQL");

    // 2. Second migration run (must be idempotent no-op)
    pg_store
        .run_migrations()
        .await
        .expect("Re-running migrations must succeed idempotently");
}
