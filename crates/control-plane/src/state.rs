use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{config::ControlPlaneConfig, storage::ObjectStorageDriver, store::Store};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StreamEventMessage {
    pub id: i64,
    pub organization_id: Uuid,
    pub repository_id: Option<Uuid>,
    pub stream: String,
    pub payload: serde_json::Value,
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<ControlPlaneConfig>,
    pub store: Arc<dyn Store + Send + Sync>,
    pub storage: Arc<dyn ObjectStorageDriver + Send + Sync>,
    pub events_tx: broadcast::Sender<StreamEventMessage>,
}

impl AppState {
    pub fn new(
        config: ControlPlaneConfig,
        store: Arc<dyn Store + Send + Sync>,
        storage: Arc<dyn ObjectStorageDriver + Send + Sync>,
    ) -> Self {
        let (events_tx, _) = broadcast::channel(1024);
        Self {
            config: Arc::new(config),
            store,
            storage,
            events_tx,
        }
    }
}
