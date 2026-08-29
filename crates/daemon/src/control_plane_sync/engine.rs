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
    DaemonId, DataClassification, OrganizationId, PolicyRestrictions, PolicySnapshot,
    PolicyUpdateEvent, PublicationClass, RepositoryId, Sha256Digest, StreamEvent,
    StreamEventPayload, SyncDelta, SyncDeltaKind, SyncEnvelope, CONTROL_PLANE_PROTOCOL_V1,
};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::{
    client::ControlPlaneClient,
    error::ControlPlaneSyncError,
    inbound::{
        get_policy_snapshot, get_repository_stream_cursor, has_inbound_receipt,
        record_inbound_receipt, set_repository_stream_cursor, store_policy_snapshot,
        InboundReceipt, PolicySnapshotRecord,
    },
    outbox::{
        acknowledge_receipt, fetch_pending_deltas, narrow_pending_delta_at_publication_ceiling,
        reactivate_policy_blocked_deltas, reconcile_authoritative_writes_after_policy_refresh,
        record_attempt_error, reject_delta_by_local_policy, reject_delta_locally_invalid,
        reject_delta_permanently, reject_malformed_pending_payloads, OutboxEntry,
    },
    pairing::{
        get_credential, get_pairing, list_active_pairings, revoke_pairing, ControlPlanePairing,
        PairingState, ResolvedRepositoryConsent,
    },
};

const DEFAULT_BATCH_SIZE: i64 = 50;
const MAX_INBOUND_PAGES_PER_CYCLE: usize = 20;
const INITIAL_BACKOFF_SECS: u64 = 1;
const MAX_BACKOFF_SECS: u64 = 60;
const BACKOFF_MULTIPLIER: f64 = 2.0;
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Codes describing an intrinsic property of the submitted delta. Replaying
/// the same immutable outbox row cannot change either outcome, so these rows
/// are dead-lettered instead of permanently occupying the head batch.
///
/// Authorization/policy refusal (`delta-refused`) and unrecognized future codes
/// remain pending: a grant, ceiling, or newer server may make those retryable.
fn is_permanent_delta_rejection(code: &str) -> bool {
    matches!(code, "malformed-delta" | "unprojectable-delta-kind")
}

fn policy_update_payload_hash(policy: &PolicyUpdateEvent) -> Sha256Digest {
    Sha256Digest(hex::encode(Sha256::digest(
        format!(
            "{}:{}:{}",
            policy.policy_version,
            policy.max_publication_class.as_str(),
            policy.max_classification.as_str()
        )
        .as_bytes(),
    )))
}

enum CatalogPolicyDecision {
    Submit,
    Supersede(PublicationClass),
    Reject(String),
}

fn parse_payload_classification(value: &serde_json::Value) -> Option<DataClassification> {
    match value.as_str()?.to_ascii_lowercase().as_str() {
        "public" => Some(DataClassification::Public),
        "internal" => Some(DataClassification::Internal),
        "confidential" => Some(DataClassification::Confidential),
        "secret" => Some(DataClassification::Secret),
        _ => None,
    }
}

fn parse_payload_publication_class(value: &serde_json::Value) -> Option<PublicationClass> {
    match value.as_str()?.to_ascii_lowercase().as_str() {
        "private-local" => Some(PublicationClass::PrivateLocal),
        "metadata-shared" => Some(PublicationClass::MetadataShared),
        "content-shared" => Some(PublicationClass::ContentShared),
        "organization-knowledge" => Some(PublicationClass::OrganizationKnowledge),
        "public-marketplace" => Some(PublicationClass::PublicMarketplace),
        _ => None,
    }
}

fn catalog_policy_decision(
    entry: &OutboxEntry,
    repository_consent: &ResolvedRepositoryConsent,
    organization_policy: Option<&PolicySnapshotRecord>,
) -> Result<CatalogPolicyDecision, ControlPlaneSyncError> {
    let repository_identity = entry
        .payload
        .get("repository_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            ControlPlaneSyncError::PolicyViolation(
                "repository-scoped sync delta has no repository id".to_string(),
            )
        })?;
    let policy = repository_consent
        .repository_policy_for(repository_identity)
        .ok_or_else(|| {
            ControlPlaneSyncError::PolicyViolation(format!(
                "sync delta repository identity `{repository_identity}` is outside the pairing's resolved consent"
            ))
        })?;

    // Tombstones are deletion control messages, not new publication. Once the
    // repository identity has passed authenticated consent resolution they
    // must remain sendable after a policy narrows, otherwise already-shared
    // objects can never be retracted.
    if entry.delta_kind == "tombstone" {
        return Ok(CatalogPolicyDecision::Submit);
    }

    let mut max_publication_class = policy.max_publication_class;
    let mut max_classification = policy.max_classification;
    if let Some(organization_policy) = organization_policy {
        if organization_policy.max_publication_class == PublicationClass::Unknown
            || organization_policy.max_classification == DataClassification::Unknown
        {
            return Ok(CatalogPolicyDecision::Reject(
                "organization policy contains an unrecognized publication or classification ceiling"
                    .to_string(),
            ));
        }
        max_publication_class =
            max_publication_class.intersect(organization_policy.max_publication_class);
        max_classification = max_classification.intersect(organization_policy.max_classification);
    }

    match entry.delta_kind.as_str() {
        "artifact-summary" => {
            let classification = entry
                .payload
                .get("classification")
                .and_then(parse_payload_classification)
                .ok_or_else(|| {
                    ControlPlaneSyncError::PolicyViolation(
                        "artifact sync delta has no recognized data classification".to_string(),
                    )
                })?;
            if !classification.permits(max_classification) {
                return Ok(CatalogPolicyDecision::Reject(format!(
                    "artifact classification `{}` exceeds repository `{}` ceiling `{}`",
                    classification.as_str(),
                    policy.repository_id,
                    max_classification.as_str()
                )));
            }
        }
        "graph-batch" => {
            let facts = entry
                .payload
                .get("facts")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    ControlPlaneSyncError::PolicyViolation(
                        "graph sync delta has no fact array".to_string(),
                    )
                })?;
            for fact in facts {
                let fact_class = fact
                    .get("class")
                    .and_then(parse_payload_publication_class)
                    .ok_or_else(|| {
                        ControlPlaneSyncError::PolicyViolation(
                            "graph sync fact has no recognized publication class".to_string(),
                        )
                    })?;
                if !fact_class.permits_in_ceiling(max_publication_class) {
                    return Ok(CatalogPolicyDecision::Reject(format!(
                        "graph fact publication class `{}` exceeds repository `{}` ceiling `{}`",
                        fact_class.as_str(),
                        policy.repository_id,
                        max_publication_class.as_str()
                    )));
                }
                let classification = fact
                    .get("classification")
                    .and_then(parse_payload_classification)
                    .ok_or_else(|| {
                        ControlPlaneSyncError::PolicyViolation(
                            "graph sync fact has no recognized data classification".to_string(),
                        )
                    })?;
                if !classification.permits(max_classification) {
                    return Ok(CatalogPolicyDecision::Reject(format!(
                        "graph fact classification `{}` exceeds repository `{}` ceiling `{}`",
                        classification.as_str(),
                        policy.repository_id,
                        max_classification.as_str()
                    )));
                }
            }
        }
        _ => {}
    }

    if entry.class.permits_in_ceiling(max_publication_class) {
        return Ok(CatalogPolicyDecision::Submit);
    }
    if max_publication_class.allows_off_device()
        && matches!(
            entry.delta_kind.as_str(),
            "session-summary" | "artifact-summary"
        )
    {
        return Ok(CatalogPolicyDecision::Supersede(max_publication_class));
    }
    Ok(CatalogPolicyDecision::Reject(format!(
        "delta publication class `{}` exceeds repository `{}` ceiling `{}` and cannot be safely narrowed",
        entry.class.as_str(),
        policy.repository_id,
        max_publication_class.as_str()
    )))
}

fn outbox_entry_to_delta(
    entry: &OutboxEntry,
    repository_consent: &ResolvedRepositoryConsent,
) -> Result<SyncDelta, ControlPlaneSyncError> {
    let sequence = u64::try_from(entry.sequence).map_err(|_| {
        ControlPlaneSyncError::PolicyViolation("outbox sequence is negative".to_string())
    })?;
    let kind = match entry.delta_kind.as_str() {
        "session-summary" => SyncDeltaKind::SessionSummary,
        "run-summary" => SyncDeltaKind::RunSummary,
        "artifact-summary" => SyncDeltaKind::ArtifactSummary,
        "inbox-entry" => SyncDeltaKind::InboxEntry,
        "graph-batch" => SyncDeltaKind::GraphBatch,
        "tombstone" => SyncDeltaKind::Tombstone,
        "approval-decision" => SyncDeltaKind::ApprovalDecision,
        "usage-aggregate" => SyncDeltaKind::UsageAggregate,
        _ => {
            return Err(ControlPlaneSyncError::PolicyViolation(
                "outbox contains an unsupported delta kind".to_string(),
            ))
        }
    };
    if matches!(
        entry.class,
        codypendent_control_plane_protocol::PublicationClass::PrivateLocal
            | codypendent_control_plane_protocol::PublicationClass::Unknown
    ) {
        return Err(ControlPlaneSyncError::PolicyViolation(format!(
            "outbox publication class `{}` is not eligible for off-device sync",
            entry.class.as_str()
        )));
    }
    let repository_id = entry
        .payload
        .get("repository_id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            ControlPlaneSyncError::PolicyViolation(
                "repository-scoped sync delta has no repository id".to_string(),
            )
        })
        .and_then(|value| {
            repository_consent.repository_id_for(value).ok_or_else(|| {
                ControlPlaneSyncError::PolicyViolation(format!(
                    "sync delta repository identity `{value}` is outside the pairing's resolved consent"
                ))
            })
        })?;
    let payload_hash = Sha256Digest::new(entry.payload_hash.clone()).map_err(|_| {
        ControlPlaneSyncError::PolicyViolation(
            "outbox payload hash is not a valid SHA-256 digest".to_string(),
        )
    })?;

    Ok(SyncDelta {
        id: entry.id.clone(),
        sequence,
        kind,
        repository_id: Some(RepositoryId::from_uuid(repository_id)),
        subject_id: entry.subject_id.clone(),
        payload: entry.payload.clone(),
        class: entry.class,
        payload_hash,
        created_at: entry.created_at,
    })
}

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
    next_attempt: Arc<Mutex<HashMap<String, tokio::time::Instant>>>,
}

impl SyncEngine {
    /// Create a new sync engine.
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            token_cache: Arc::new(RwLock::new(HashMap::new())),
            backoff: Arc::new(Mutex::new(HashMap::new())),
            next_attempt: Arc::new(Mutex::new(HashMap::new())),
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

    /// Remove a cached token after revocation, expiry, or metadata mismatch.
    pub async fn remove_pairing_token(&self, pairing_id: &str) {
        self.token_cache.write().await.remove(pairing_id);
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
        drop(backoff);
        self.next_attempt
            .lock()
            .await
            .insert(pairing_id.to_string(), tokio::time::Instant::now() + next);
        next
    }

    /// Whether a background iteration may make a network attempt for this
    /// pairing. Manual `sync_pairing_once` calls remain immediate; only the
    /// scheduler consults this gate.
    async fn pairing_is_due(&self, pairing_id: &str) -> bool {
        self.next_attempt
            .lock()
            .await
            .get(pairing_id)
            .is_none_or(|deadline| tokio::time::Instant::now() >= *deadline)
    }

    /// Reset backoff delay on success.
    pub async fn reset_backoff(&self, pairing_id: &str) {
        let mut backoff = self.backoff.lock().await;
        backoff.remove(pairing_id);
        drop(backoff);
        self.next_attempt.lock().await.remove(pairing_id);
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

        let credential = get_credential(&self.pool, pairing_id).await?;
        let token = self.get_pairing_token(pairing_id).await;
        let Some(credential) = credential else {
            self.remove_pairing_token(pairing_id).await;
            self.record_failure_backoff(pairing_id).await;
            return Err(ControlPlaneSyncError::CredentialUnavailable(
                "active pairing has no credential metadata".to_string(),
            ));
        };
        let metadata_is_usable = credential.expires_at > Utc::now()
            && credential.purpose == "sync"
            && (credential.audience == "control-plane" || credential.audience == pairing.endpoint);
        let token_is_verified = token.as_ref().is_some_and(|token| {
            hex::encode(Sha256::digest(token.as_bytes())) == credential.credential_hash
        });
        if !metadata_is_usable || !token_is_verified {
            self.remove_pairing_token(pairing_id).await;
            self.record_failure_backoff(pairing_id).await;
            return Err(ControlPlaneSyncError::CredentialUnavailable(
                "active pairing credential is absent, expired, or unverifiable".to_string(),
            ));
        }
        let token = token.expect("verified token was checked as present");
        let client = ControlPlaneClient::new(&pairing.endpoint, Some(token))?;
        let organization_id = Uuid::parse_str(&pairing.organization_id).map_err(|_| {
            ControlPlaneSyncError::InvalidConsentManifest(
                "pairing organization id is invalid".to_string(),
            )
        })?;
        let repository_catalog = match client.list_repositories(organization_id).await {
            Ok(catalog) => catalog,
            Err(ControlPlaneSyncError::Revoked(reason)) => {
                revoke_pairing(&self.pool, pairing_id, &reason).await?;
                return Err(ControlPlaneSyncError::Revoked(reason));
            }
            Err(err) => {
                debug!(pairing_id = %pairing_id, error = %err, "repository catalog fetch failed");
                self.record_failure_backoff(pairing_id).await;
                return Err(err);
            }
        };
        let repository_consent = pairing
            .resolve_repository_consent(&self.pool, &repository_catalog)
            .await?;

        let mut summary = SyncSummary::default();

        // Refresh organization policy before considering even the first
        // outbound byte. Repository catalog ceilings are only one half of the
        // effective decision; the stricter organization snapshot must apply to
        // queued rows in this same cycle.
        self.pull_inbound_stream(&client, &pairing, None, "policy", &mut summary)
            .await?;
        let organization_policy = get_policy_snapshot(&self.pool, pairing_id).await?;

        let reactivated = reactivate_policy_blocked_deltas(&self.pool, pairing_id).await?;
        if reactivated > 0 {
            debug!(pairing_id = %pairing_id, reactivated, "reconsidering catalog-policy-blocked deltas");
        }
        if repository_consent.mapping_changed() {
            let reconciled =
                reconcile_authoritative_writes_after_policy_refresh(&self.pool, pairing_id).await?;
            debug!(pairing_id = %pairing_id, reconciled, "reconciled newly resolved consent-scoped authoritative writes");
        }

        // 1. OUTBOUND SYNC: drain pending outbox rows
        let mut submitted_entries = Vec::new();
        let mut deltas = Vec::new();
        loop {
            loop {
                let malformed =
                    reject_malformed_pending_payloads(&self.pool, pairing_id, DEFAULT_BATCH_SIZE)
                        .await?;
                summary.failed_deltas += malformed;
                if malformed < DEFAULT_BATCH_SIZE as usize {
                    break;
                }
            }
            let pending = fetch_pending_deltas(&self.pool, pairing_id, DEFAULT_BATCH_SIZE).await?;
            if pending.is_empty() {
                break;
            }
            for entry in pending {
                match outbox_entry_to_delta(&entry, &repository_consent) {
                    Ok(delta) => match catalog_policy_decision(
                        &entry,
                        &repository_consent,
                        organization_policy.as_ref(),
                    ) {
                        Ok(CatalogPolicyDecision::Submit) => {
                            submitted_entries.push(entry);
                            deltas.push(delta);
                        }
                        Ok(CatalogPolicyDecision::Supersede(narrowed_class)) => {
                            narrow_pending_delta_at_publication_ceiling(
                                &self.pool,
                                &entry,
                                narrowed_class,
                            )
                            .await?;
                        }
                        Ok(CatalogPolicyDecision::Reject(reason)) => {
                            reject_delta_by_local_policy(
                                &self.pool,
                                pairing_id,
                                entry.sequence,
                                &reason,
                                Utc::now(),
                            )
                            .await?;
                            summary.failed_deltas += 1;
                        }
                        Err(ControlPlaneSyncError::PolicyViolation(reason)) => {
                            reject_delta_locally_invalid(
                                &self.pool,
                                pairing_id,
                                entry.sequence,
                                &reason,
                                Utc::now(),
                            )
                            .await?;
                            summary.failed_deltas += 1;
                        }
                        Err(err) => return Err(err),
                    },
                    Err(ControlPlaneSyncError::PolicyViolation(reason)) => {
                        reject_delta_locally_invalid(
                            &self.pool,
                            pairing_id,
                            entry.sequence,
                            &reason,
                            Utc::now(),
                        )
                        .await?;
                        summary.failed_deltas += 1;
                    }
                    Err(err) => return Err(err),
                }
            }
            // If this page contained at least one valid row, submit it now.
            // When a full head page was entirely malformed, its durable
            // rejection exposes the next page so a later valid row cannot be
            // starved forever.
            if !deltas.is_empty() {
                break;
            }
        }

        if !deltas.is_empty() {
            let daemon_id = Uuid::parse_str(&pairing.id).map_err(|_| {
                ControlPlaneSyncError::InvalidConsentManifest(
                    "pairing id is not the control-plane daemon id".to_string(),
                )
            })?;
            summary.pushed_deltas = deltas.len();
            let envelope = SyncEnvelope {
                protocol_version: CONTROL_PLANE_PROTOCOL_V1,
                daemon_id: DaemonId::from_uuid(daemon_id),
                organization_id: OrganizationId::from_uuid(organization_id),
                sent_at: Utc::now(),
                deltas,
            };

            match client.push_sync_envelope(&envelope).await {
                Ok(response) => {
                    let mut submitted_outcomes = 0;
                    for receipt in response.receipts {
                        let sequence = i64::try_from(receipt.daemon_sequence).map_err(|_| {
                            ControlPlaneSyncError::RemoteRejected(
                                "sync receipt sequence is out of range".to_string(),
                            )
                        })?;
                        let Some(entry) = submitted_entries
                            .iter()
                            .find(|entry| entry.sequence == sequence)
                        else {
                            return Err(ControlPlaneSyncError::RemoteRejected(
                                "sync response contains a receipt outside the submitted batch"
                                    .to_string(),
                            ));
                        };
                        if receipt.payload_hash.as_str() != entry.payload_hash {
                            return Err(ControlPlaneSyncError::RemoteRejected(
                                "sync response receipt hash does not match the submitted delta"
                                    .to_string(),
                            ));
                        }
                        acknowledge_receipt(
                            &self.pool,
                            pairing_id,
                            sequence,
                            &receipt.id.to_string(),
                            receipt.accepted_at,
                        )
                        .await?;
                        summary.acknowledged_deltas += 1;
                        submitted_outcomes += 1;
                    }
                    for rejection in response.rejected_deltas {
                        let sequence = i64::try_from(rejection.sequence).map_err(|_| {
                            ControlPlaneSyncError::RemoteRejected(
                                "sync rejection sequence is out of range".to_string(),
                            )
                        })?;
                        let Some(entry) = submitted_entries
                            .iter()
                            .find(|entry| entry.sequence == sequence)
                        else {
                            return Err(ControlPlaneSyncError::RemoteRejected(
                                "sync response contains a rejection outside the submitted batch"
                                    .to_string(),
                            ));
                        };
                        if is_permanent_delta_rejection(&rejection.code) {
                            reject_delta_permanently(
                                &self.pool,
                                pairing_id,
                                sequence,
                                &rejection.code,
                                &rejection.reason,
                                Utc::now(),
                            )
                            .await?;
                        } else {
                            let message =
                                format!("control plane refused delta with code {}", rejection.code);
                            record_attempt_error(&self.pool, &entry.id, &message).await?;
                        }
                        summary.failed_deltas += 1;
                        submitted_outcomes += 1;
                    }
                    if submitted_outcomes != submitted_entries.len() {
                        return Err(ControlPlaneSyncError::RemoteRejected(
                            "sync response omitted a submitted delta".to_string(),
                        ));
                    }
                }
                Err(ControlPlaneSyncError::Revoked(reason)) => {
                    // Control plane explicitly rejected credentials
                    warn!(pairing_id = %pairing_id, reason = %reason, "pairing rejected by remote control plane; marking revoked");
                    revoke_pairing(&self.pool, pairing_id, &reason).await?;
                    return Err(ControlPlaneSyncError::Revoked(reason));
                }
                Err(err) => {
                    let err_msg = err.to_string();
                    warn!(pairing_id = %pairing_id, error = %err_msg, "outbound delta batch push failed");
                    for entry in &submitted_entries {
                        record_attempt_error(&self.pool, &entry.id, &err_msg).await?;
                    }
                    self.record_failure_backoff(pairing_id).await;
                    return Err(err);
                }
            }
        }

        // 2. INBOUND REPOSITORY SYNC: organization policy was deliberately
        // pulled before outbound. Every remaining stream is repository-scoped
        // and remains separately authorized/cursored.
        let streams = [
            "sync",
            "notifications",
            "approvals",
            "schedules",
            "runner-events",
        ];
        for repository_id in repository_consent.repository_ids().iter().copied() {
            for stream in streams {
                self.pull_inbound_stream(
                    &client,
                    &pairing,
                    Some(repository_id),
                    stream,
                    &mut summary,
                )
                .await?;
            }
        }

        // Success: reset backoff
        self.reset_backoff(pairing_id).await;
        Ok(summary)
    }

    async fn pull_inbound_stream(
        &self,
        client: &ControlPlaneClient,
        pairing: &ControlPlanePairing,
        repository_id: Option<Uuid>,
        stream: &str,
        summary: &mut SyncSummary,
    ) -> Result<(), ControlPlaneSyncError> {
        let repository_key = repository_id.map_or_else(String::new, |id| id.to_string());
        let cursor_str =
            get_repository_stream_cursor(&self.pool, &pairing.id, &repository_key, stream).await?;
        let mut after_id = cursor_str
            .as_deref()
            .and_then(|cursor| cursor.parse::<i64>().ok())
            .unwrap_or(0);

        for page_index in 0..MAX_INBOUND_PAGES_PER_CYCLE {
            let events = match client
                .pull_sync_events(repository_id, stream, after_id, DEFAULT_BATCH_SIZE as usize)
                .await
            {
                Ok(events) => events,
                Err(ControlPlaneSyncError::Revoked(reason)) => {
                    revoke_pairing(&self.pool, &pairing.id, &reason).await?;
                    return Err(ControlPlaneSyncError::Revoked(reason));
                }
                Err(err) => {
                    debug!(pairing_id = %pairing.id, stream = %stream, error = %err, "inbound event pull skipped due to network");
                    self.record_failure_backoff(&pairing.id).await;
                    return Err(err);
                }
            };
            let page_is_full = events.len() >= DEFAULT_BATCH_SIZE as usize;
            let prior_after_id = after_id;
            for event in events {
                summary.pulled_events += 1;
                let event_id_str = event.id.to_string();
                let already_processed =
                    has_inbound_receipt(&self.pool, &pairing.id, &event_id_str).await?;

                if !already_processed {
                    self.apply_inbound_event(pairing, &event).await?;
                    let receipt = InboundReceipt {
                        pairing_id: pairing.id.clone(),
                        remote_message_id: event_id_str.clone(),
                        message_kind: stream.to_string(),
                        local_effect_id: Some(format!("effect_{}", event.id)),
                        outcome_hash: hex::encode(Sha256::digest(event_id_str.as_bytes())),
                        received_at: Utc::now(),
                    };
                    record_inbound_receipt(&self.pool, &receipt).await?;
                }
                set_repository_stream_cursor(
                    &self.pool,
                    &pairing.id,
                    &repository_key,
                    stream,
                    &event_id_str,
                )
                .await?;
                after_id = i64::try_from(event.id).map_err(|_| {
                    ControlPlaneSyncError::RemoteRejected(
                        "inbound stream cursor is out of local range".to_string(),
                    )
                })?;
            }

            if !page_is_full {
                return Ok(());
            }
            if after_id <= prior_after_id {
                return Err(ControlPlaneSyncError::RemoteRejected(format!(
                    "control-plane `{stream}` stream returned a full page without advancing its cursor"
                )));
            }
            if page_index + 1 == MAX_INBOUND_PAGES_PER_CYCLE {
                return Err(ControlPlaneSyncError::RemoteRejected(format!(
                    "control-plane `{stream}` stream did not quiesce within {MAX_INBOUND_PAGES_PER_CYCLE} pages"
                )));
            }
        }
        unreachable!("bounded inbound pagination loop always returns")
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
                    payload_hash: policy_update_payload_hash(policy_update),
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
            if !self.pairing_is_due(&pairing.id).await {
                debug!(pairing_id = %pairing.id, "control-plane sync deferred by pairing backoff");
                continue;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_intrinsically_permanent_delta_rejections_are_dead_lettered() {
        assert!(is_permanent_delta_rejection("malformed-delta"));
        assert!(is_permanent_delta_rejection("unprojectable-delta-kind"));
        assert!(!is_permanent_delta_rejection("delta-refused"));
        assert!(!is_permanent_delta_rejection("temporarily-unavailable"));
        assert!(!is_permanent_delta_rejection("future-code"));
    }

    #[test]
    fn policy_snapshot_hash_includes_the_classification_ceiling() {
        let public = PolicyUpdateEvent {
            policy_version: 7,
            max_publication_class: PublicationClass::MetadataShared,
            max_classification: DataClassification::Public,
        };
        let internal = PolicyUpdateEvent {
            max_classification: DataClassification::Internal,
            ..public.clone()
        };

        assert_ne!(
            policy_update_payload_hash(&public),
            policy_update_payload_hash(&internal)
        );
        assert_eq!(
            policy_update_payload_hash(&public),
            policy_update_payload_hash(&public)
        );
    }

    #[tokio::test]
    async fn a_recorded_pairing_backoff_suppresses_network_eligibility_until_reset() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_lazy("sqlite::memory:")
            .expect("lazy pool");
        let engine = SyncEngine::new(pool);

        assert!(engine.pairing_is_due("pairing-a").await);
        let delay = engine.record_failure_backoff("pairing-a").await;
        assert_eq!(delay, Duration::from_secs(2));
        assert!(
            !engine.pairing_is_due("pairing-a").await,
            "the outer poll must not retry a pairing before its own deadline"
        );
        engine.reset_backoff("pairing-a").await;
        assert!(engine.pairing_is_due("pairing-a").await);
    }
}
