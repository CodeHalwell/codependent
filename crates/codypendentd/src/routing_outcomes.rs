//! The daemon's implementation of `codypendent_runtime::agent::RoutingOutcomeSink`
//! (outcome 11): folds a finished run's result into the model's stored
//! per-task-class success table — the map `codypendent_routing`'s classifier
//! actually routes on.
//!
//! The writer (`ModelProfileStore::record_outcome`) and the reader (the router)
//! both existed; nothing called the writer, so `performance.task_class_success`
//! was permanently empty and the nine-class classifier could never change a
//! decision. This is the missing caller. It lives behind the runtime's
//! pool-erased seam because `codypendent-runtime` cannot name `SqlitePool`
//! (ADR-009), the same reason `RunJournal` and `ArtifactSink` are seams.

use async_trait::async_trait;
use codypendent_daemon::model_profiles::ModelProfileStore;
use codypendent_runtime::agent::{RoutingOutcome, RoutingOutcomeSink};
use sqlx::SqlitePool;

/// Folds terminal run outcomes into the model-profile store on the daemon's pool.
pub struct PoolRoutingOutcomes {
    pool: SqlitePool,
}

impl PoolRoutingOutcomes {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RoutingOutcomeSink for PoolRoutingOutcomes {
    async fn record(&self, outcome: RoutingOutcome<'_>) -> Result<(), String> {
        // `record_outcome` returns `Ok(false)` when no profile row exists for
        // (model, endpoint) — a model that was never benched. That is the
        // designed no-op, not an error: creating a row here would make a model
        // with no MEASURED capabilities routable off a bare success count,
        // which is exactly the fabricated-evidence failure the profile store
        // exists to prevent.
        ModelProfileStore::new()
            .record_outcome(
                &self.pool,
                outcome.model,
                outcome.endpoint,
                outcome.task_class,
                outcome.success,
                &outcome.run_id.to_string(),
            )
            .await
            .map(|_folded| ())
            .map_err(|error| error.to_string())
    }
}
