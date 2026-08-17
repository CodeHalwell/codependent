use clap::Parser;
use codypendent_control_plane::{
    AppState, ControlPlaneConfig, MemoryStorageDriver, MemoryStore, ObjectStorageDriver, PgStore,
    S3StorageDriver, StorageConfig, Store,
};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(
    name = "codypendent-control-plane",
    about = "Codypendent Control Plane Server"
)]
struct Args {
    #[arg(short, long, env = "DATABASE_URL")]
    database_url: Option<String>,

    #[arg(short, long, env = "LISTEN_ADDR", default_value = "0.0.0.0:8080")]
    listen_addr: std::net::SocketAddr,

    #[arg(long, default_value_t = false)]
    migrate_only: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "codypendent_control_plane=debug,tower_http=debug,info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = Args::parse();
    // Fail closed: without an explicitly configured signing secret the service
    // cannot authenticate anyone, so it must not start.
    let mut config = ControlPlaneConfig::from_env().map_err(|e| {
        tracing::error!(error = %e, "Refusing to start with an unusable configuration");
        e
    })?;
    if let Some(db_url) = args.database_url {
        config.database_url = Some(db_url);
    }
    config.listen_addr = args.listen_addr;

    let store: Arc<dyn Store + Send + Sync> = if let Some(ref db_url) = config.database_url {
        tracing::info!("Connecting to PostgreSQL database...");
        let pool = PgPoolOptions::new()
            .max_connections(20)
            .connect(db_url)
            .await?;

        let pg_store = PgStore::new(pool);
        tracing::info!("Running PostgreSQL migrations...");
        pg_store.run_migrations().await?;
        tracing::info!("PostgreSQL migrations applied successfully.");

        if args.migrate_only {
            tracing::info!("Migration-only run requested. Exiting.");
            return Ok(());
        }

        Arc::new(pg_store)
    } else {
        tracing::warn!("No DATABASE_URL configured. Running in in-memory storage mode.");
        Arc::new(MemoryStore::new())
    };

    let storage: Arc<dyn ObjectStorageDriver + Send + Sync> = match &config.storage {
        StorageConfig::S3 {
            endpoint,
            bucket,
            region,
            access_key,
            secret_key,
            use_path_style,
        } => {
            tracing::info!(bucket = %bucket, region = %region, "Configuring S3 object storage driver");
            Arc::new(S3StorageDriver::new(
                endpoint.clone(),
                bucket.clone(),
                region.clone(),
                access_key.clone(),
                secret_key.clone(),
                *use_path_style,
            ))
        }
        StorageConfig::Memory => {
            tracing::info!("Configuring in-memory object storage driver");
            Arc::new(MemoryStorageDriver::new())
        }
    };

    let state = AppState::new(config.clone(), store, storage);
    codypendent_control_plane::serve(config.listen_addr, state).await?;

    Ok(())
}
