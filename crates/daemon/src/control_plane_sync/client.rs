//! Outbound HTTP client for control plane API interactions.
//!
//! Note: Outbound only. The daemon initiates all connections; no local listening port is ever opened.

use std::time::Duration;

use chrono::{DateTime, Utc};
use codypendent_control_plane_protocol::StreamEvent;
use reqwest::{header, Client, StatusCode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::error::ControlPlaneSyncError;

/// Request to complete a pairing challenge with the control plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletePairingRequest {
    pub pairing_code: String,
    pub display_name: String,
    pub consent_manifest: String,
    pub max_publication_class: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepts_remote_approvals: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepts_runner_dispatch: Option<bool>,
}

/// Response returned by the control plane upon successful pairing completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletePairingResponse {
    pub daemon_id: Uuid,
    pub organization_id: Uuid,
    pub token: String,
}

/// Request to push a single sync delta to the control plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncDeltaPushRequest {
    pub daemon_sequence: i64,
    pub delta_kind: String,
    pub repository_id: Option<Uuid>,
    pub subject_id: String,
    pub class: String,
    pub payload: serde_json::Value,
    pub payload_hash: String,
}

/// Response from the control plane confirming receipt of a sync delta.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncDeltaPushResponse {
    pub receipt_id: Uuid,
    pub daemon_sequence: i64,
    pub accepted_at: DateTime<Utc>,
    pub duplicate: bool,
}

/// Client interacting with a remote control plane service.
#[derive(Debug, Clone)]
pub struct ControlPlaneClient {
    endpoint: String,
    http: Client,
    token: Option<String>,
}

impl ControlPlaneClient {
    /// Create a new client targeting a specific control plane endpoint.
    pub fn new(endpoint: &str, token: Option<String>) -> Result<Self, ControlPlaneSyncError> {
        let trimmed_endpoint = endpoint.trim().trim_end_matches('/').to_string();
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .build()?;

        Ok(Self {
            endpoint: trimmed_endpoint,
            http,
            token,
        })
    }

    /// Set or update the bearer access token.
    #[must_use]
    pub fn with_token(mut self, token: String) -> Self {
        self.token = Some(token);
        self
    }

    /// Complete a pairing handshake by exchanging a pairing challenge code.
    pub async fn complete_pairing(
        &self,
        request: &CompletePairingRequest,
    ) -> Result<CompletePairingResponse, ControlPlaneSyncError> {
        let url = format!("{}/v1/auth/pairing/complete", self.endpoint);
        let resp = self.http.post(&url).json(request).send().await?;

        if resp.status().is_success() {
            let data = resp.json::<CompletePairingResponse>().await?;
            Ok(data)
        } else if resp.status() == StatusCode::UNAUTHORIZED
            || resp.status() == StatusCode::FORBIDDEN
        {
            let body = resp.text().await.unwrap_or_default();
            Err(ControlPlaneSyncError::RemoteRejected(format!(
                "pairing challenge rejected: {body}"
            )))
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(ControlPlaneSyncError::RemoteRejected(format!(
                "pairing failed with status {status}: {body}"
            )))
        }
    }

    /// Push an outbound sync delta to the control plane.
    pub async fn push_sync_delta(
        &self,
        delta: &SyncDeltaPushRequest,
    ) -> Result<SyncDeltaPushResponse, ControlPlaneSyncError> {
        let url = format!("{}/v1/sync/push", self.endpoint);
        let mut builder = self.http.post(&url);

        if let Some(ref tok) = self.token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {tok}"));
        }

        let resp = builder.json(delta).send().await?;

        if resp.status().is_success() {
            let data = resp.json::<SyncDeltaPushResponse>().await?;
            Ok(data)
        } else if resp.status() == StatusCode::UNAUTHORIZED {
            Err(ControlPlaneSyncError::Revoked(
                "control plane rejected daemon credentials (revoked or expired)".to_string(),
            ))
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(ControlPlaneSyncError::RemoteRejected(format!(
                "sync push failed with status {status}: {body}"
            )))
        }
    }

    /// Pull stream events from the control plane starting from a cursor.
    pub async fn pull_sync_events(
        &self,
        stream: &str,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<StreamEvent>, ControlPlaneSyncError> {
        let url = format!("{}/v1/sync/events", self.endpoint);
        let mut builder = self.http.get(&url).query(&[
            ("stream", stream),
            ("after_id", &after_id.to_string()),
            ("limit", &limit.to_string()),
        ]);

        if let Some(ref tok) = self.token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {tok}"));
        }

        let resp = builder.send().await?;

        if resp.status().is_success() {
            let events = resp.json::<Vec<StreamEvent>>().await?;
            Ok(events)
        } else if resp.status() == StatusCode::UNAUTHORIZED {
            Err(ControlPlaneSyncError::Revoked(
                "control plane rejected daemon credentials (revoked or expired)".to_string(),
            ))
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(ControlPlaneSyncError::RemoteRejected(format!(
                "sync pull failed with status {status}: {body}"
            )))
        }
    }
}
