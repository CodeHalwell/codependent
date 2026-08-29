use chrono::{DateTime, Utc};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    auth::{hash_token, random_opaque_token, Principal},
    config::ControlPlaneConfig,
    error::ControlPlaneError,
    storage::ObjectStorageDriver,
    store::Store,
};

/// One short-lived, repository-scoped WebSocket upgrade authorization.
/// Tickets live only in memory, are stored by hash, and are removed before an
/// upgrade is accepted. A logged query string therefore cannot be replayed.
#[derive(Debug, Clone)]
pub(crate) struct WsTicketGrant {
    pub principal: Principal,
    pub organization_id: Uuid,
    pub repository_id: Option<Uuid>,
    pub stream: String,
    pub last_event_id: i64,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StreamEventMessage {
    pub id: i64,
    pub organization_id: Uuid,
    pub repository_id: Option<Uuid>,
    pub stream: String,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<ControlPlaneConfig>,
    pub store: Arc<dyn Store + Send + Sync>,
    pub storage: Arc<dyn ObjectStorageDriver + Send + Sync>,
    pub events_tx: broadcast::Sender<StreamEventMessage>,
    ws_tickets: Arc<Mutex<HashMap<Vec<u8>, WsTicketGrant>>>,
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
            ws_tickets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn issue_ws_ticket(
        &self,
        principal: Principal,
        organization_id: Uuid,
        repository_id: Option<Uuid>,
        stream: String,
        last_event_id: i64,
    ) -> Result<(String, DateTime<Utc>), ControlPlaneError> {
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(30);
        let raw = random_opaque_token("cp_ws_")?;
        let grant = WsTicketGrant {
            principal,
            organization_id,
            repository_id,
            stream,
            last_event_id,
            expires_at,
        };
        let mut tickets = self.ws_tickets.lock().map_err(|_| {
            ControlPlaneError::Internal("websocket ticket registry is unavailable".to_string())
        })?;
        tickets.retain(|_, existing| existing.expires_at > now);
        tickets.insert(hash_token(&raw), grant);
        Ok((raw, expires_at))
    }

    pub(crate) fn consume_ws_ticket(&self, raw: &str) -> Option<WsTicketGrant> {
        let now = Utc::now();
        let mut tickets = self.ws_tickets.lock().ok()?;
        tickets.retain(|_, existing| existing.expires_at > now);
        tickets.remove(&hash_token(raw))
    }
}
