//! Background bidirectional synchronization engine with exponential backoff.
//!
//! Enforces:
//! - Offline tolerance: Works 100% when unpaired or offline.
//! - Zero socket or network usage when unpaired.
//! - Outbound sync of sessions, runs, artifacts, published graphs, audit events, and tombstones.
//! - Inbound sync of shared policies, streams, and receipts.
//! - Exponential backoff on connectivity failures.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use codypendent_control_plane_protocol::{
    PolicyRestrictions, PolicySnapshot, Sha256Digest, StreamEvent, StreamEventPayload,
};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::{
    client::{ControlPlaneClient, SyncDeltaPushRequest},
    error::ControlPlaneSyncError,
    inbound::{
        get_stream_cursor, has_inbound_receipt, record_inbound_receipt, set_stream_cursor,
        store_policy_snapshot, InboundReceipt,
    },
    outbox::{acknowledge_receipt, fetch_pending_deltas, record_attempt_error},
    pairing::{
        get_pairing, list_active_pairings, revoke_pairing, ControlPlanePairing, PairingState,
    },
};

const DEFAULT_BATCH_SIZE: i64 = 50;
const INITIAL_BACKOFF_SECS: u64 = 1;
const MAX_BACKOFF_SECS: u64 = 60;
const BACKOFF_MULTIPLIER: f64 = 2.0;
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Metrics and count summary of a single synchronization iteration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncSummary {
    pub pushed_deltas: usize,
    pub acknowledged_deltas: usize,
    pub failed_deltas: usize,
    pub pulled_events: usize,
}

/// The daemon-side background sync engine.
#[derive(Clone)]
pub struct SyncEngine {
    pool: SqlitePool,
    token_cache: Arc<RwLock<HashMap<String, String>>>,
    backoff: Arc<Mutex<HashMap<String, Duration>>>,
}

impl SyncEngine {
    /// Create a new sync engine.
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            token_cache: Arc::new(RwLock::new(HashMap::new())),
            backoff: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Cache an access token in memory for a pairing.
    pub async fn set_pairing_token(&self, pairing_id: &str, token: &str) {
        let mut cache = self.token_cache.write().await;
        cache.insert(pairing_id.to_string(), token.to_string());
    }

    /// Get cached access token for a pairing.
    pub async fn get_pairing_token(&self, pairing_id: &str) -> Option<String> {
        let cache = self.token_cache.read().await;
        cache.get(pairing_id).cloned()
    }

    /// Get the current backoff delay for a pairing.
    pub async fn get_backoff_delay(&self, pairing_id: &str) -> Duration {
        let backoff = self.backoff.lock().await;
        backoff
            .get(pairing_id)
            .copied()
            .unwrap_or(Duration::from_secs(INITIAL_BACKOFF_SECS))
    }

    /// Increment backoff delay on failure.
    pub async fn record_failure_backoff(&self, pairing_id: &str) -> Duration {
        let mut backoff = self.backoff.lock().await;
        let current = backoff
            .get(pairing_id)
            .copied()
            .unwrap_or(Duration::from_secs(INITIAL_BACKOFF_SECS));
        let next_secs = (current.as_secs_f64() * BACKOFF_MULTIPLIER)
            .min(MAX_BACKOFF_SECS as f64)
            .max(INITIAL_BACKOFF_SECS as f64);
        let next = Duration::from_secs_f64(next_secs);
        backoff.insert(pairing_id.to_string(), next);
        next
    }

    /// Reset backoff delay on success.
    pub async fn reset_backoff(&self, pairing_id: &str) {
        let mut backoff = self.backoff.lock().await;
        backoff.remove(pairing_id);
    }

    /// Execute a single bidirectional synchronization cycle for a specific pairing.
    pub async fn sync_pairing_once(
        &self,
        pairing_id: &str,
    ) -> Result<SyncSummary, ControlPlaneSyncError> {
        let pairing = match get_pairing(&self.pool, pairing_id).await? {
            Some(p) if p.state == PairingState::Active => p,
            Some(_) => {
                return Err(ControlPlaneSyncError::Revoked(
                    "pairing is not active".to_string(),
                ))
            }
            None => return Err(ControlPlaneSyncError::Unpaired),
        };

        let token = self.get_pairing_token(pairing_id).await;
        let client = ControlPlaneClient::new(&pairing.endpoint, token)?;

        let mut summary = SyncSummary::default();

        // 1. OUTBOUND SYNC: drain pending outbox rows
        let pending = fetch_pending_deltas(&self.pool, pairing_id, DEFAULT_BATCH_SIZE).await?;
        for entry in pending {
            summary.pushed_deltas += 1;
            let push_req = SyncDeltaPushRequest {
                daemon_sequence: entry.sequence,
                delta_kind: entry.delta_kind.clone(),
                repository_id: entry
                    .payload
                    .get("repository_id")
                    .and_then(|r| r.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok()),
                subject_id: entry.subject_id.clone(),
                class: entry.class.as_str().to_string(),
                payload: entry.payload.clone(),
                payload_hash: entry.payload_hash.clone(),
            };

            match client.push_sync_delta(&push_req).await {
                Ok(resp) => {
                    acknowledge_receipt(
                        &self.pool,
                        pairing_id,
                        entry.sequence,
                        &resp.receipt_id.to_string(),
                        resp.accepted_at,
                    )
                    .await?;
                    summary.acknowledged_deltas += 1;
                }
                Err(ControlPlaneSyncError::Revoked(reason)) => {
                    // Control plane explicitly rejected credentials
                    warn!(pairing_id = %pairing_id, reason = %reason, "pairing rejected by remote control plane; marking revoked");
                    revoke_pairing(&self.pool, pairing_id, &reason).await?;
                    return Err(ControlPlaneSyncError::Revoked(reason));
                }
                Err(err) => {
                    let err_msg = err.to_string();
                    warn!(pairing_id = %pairing_id, sequence = entry.sequence, error = %err_msg, "outbound delta push failed");
                    record_attempt_error(&self.pool, &entry.id, &err_msg).await?;
                    self.record_failure_backoff(pairing_id).await;
                    return Err(err);
                }
            }
        }

        // 2. INBOUND SYNC: pull stream events (sync, policy, notifications, etc.)
        let streams = ["sync", "policy", "notifications", "approvals", "schedules"];
        for stream in streams {
            let cursor_str = get_stream_cursor(&self.pool, pairing_id, stream).await?;
            let after_id = cursor_str
                .as_deref()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);

            match client.pull_sync_events(stream, after_id, 50).await {
                Ok(events) => {
                    for event in events {
                        summary.pulled_events += 1;
                        let event_id_str = event.id.to_string();
                        let already_processed =
                            has_inbound_receipt(&self.pool, pairing_id, &event_id_str).await?;

                        if !already_processed {
                            // Apply domain-specific inbound effect
                            self.apply_inbound_event(&pairing, &event).await?;

                            let receipt = InboundReceipt {
                                pairing_id: pairing_id.to_string(),
                                remote_message_id: event_id_str.clone(),
                                message_kind: stream.to_string(),
                                local_effect_id: Some(format!("effect_{}", event.id)),
                                outcome_hash: hex::encode(Sha256::digest(event_id_str.as_bytes())),
                                received_at: Utc::now(),
                            };

                            record_inbound_receipt(&self.pool, &receipt).await?;
                            set_stream_cursor(&self.pool, pairing_id, stream, &event_id_str)
                                .await?;
                        }
                    }
                }
                Err(ControlPlaneSyncError::Revoked(reason)) => {
                    revoke_pairing(&self.pool, pairing_id, &reason).await?;
                    return Err(ControlPlaneSyncError::Revoked(reason));
                }
                Err(err) => {
                    debug!(pairing_id = %pairing_id, stream = %stream, error = %err, "inbound event pull skipped due to network");
                    // We don't fail the whole cycle if inbound pull encounters a transient issue
                }
            }
        }

        // Success: reset backoff
        self.reset_backoff(pairing_id).await;
        Ok(summary)
    }

    async fn apply_inbound_event(
        &self,
        pairing: &ControlPlanePairing,
        event: &StreamEvent,
    ) -> Result<(), ControlPlaneSyncError> {
        match &event.payload {
            StreamEventPayload::PolicyUpdate(policy_update) => {
                let snapshot = PolicySnapshot {
                    policy_version: policy_update.policy_version,
                    max_publication_class: policy_update.max_publication_class,
                    max_classification: policy_update.max_classification,
                    restrictions: PolicyRestrictions::default(),
                    received_at: Utc::now(),
                    payload_hash: Sha256Digest(hex::encode(Sha256::digest(
                        format!(
                            "{}:{}",
                            policy_update.policy_version,
                            policy_update.max_publication_class.as_str()
                        )
                        .as_bytes(),
                    ))),
                };
                store_policy_snapshot(&self.pool, &pairing.id, &snapshot).await?;
                info!(pairing_id = %pairing.id, version = policy_update.policy_version, "updated policy snapshot from remote control plane");
            }
            _ => {
                // Other stream events recorded via receipt
            }
        }
        Ok(())
    }

    /// Run one iteration across all active pairings. Returns number of active pairings synced.
    pub async fn sync_all_active_once(&self) -> Result<usize, ControlPlaneSyncError> {
        let pairings = list_active_pairings(&self.pool).await?;
        if pairings.is_empty() {
            // Offline/unpaired: zero network calls made
            return Ok(0);
        }

        let mut synced = 0;
        for pairing in &pairings {
            match self.sync_pairing_once(&pairing.id).await {
                Ok(_) => synced += 1,
                Err(err) => {
                    debug!(pairing_id = %pairing.id, error = %err, "sync iteration for pairing encountered error");
                }
            }
        }
        Ok(synced)
    }

    /// Start the background sync loop.
    pub fn start_background_loop(
        self,
        mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!("started daemon control plane sync background worker");
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(DEFAULT_POLL_INTERVAL) => {
                        let _ = self.sync_all_active_once().await;
                    }
                    Ok(()) = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            info!("stopping daemon control plane sync background worker");
                            break;
                        }
                    }
                }
            }
        })
    }
}
