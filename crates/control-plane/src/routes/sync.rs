//! Outbound synchronization: the daemon-to-control-plane batch push, the
//! resumable pull, and the shared-session projection listing.
//!
//! `POST /v1/sync/push` speaks the protocol's [`SyncEnvelope`] — a *batch*
//! carrying its own protocol version and the identity of the daemon that sent
//! it. It previously accepted a flat single-delta body with no version field, so
//! a daemon serializing the protocol type got a 422 and the documented wire
//! contract was fiction.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use codypendent_control_plane_protocol::{
    ids::{DaemonId, OrganizationId, RepositoryId, Sha256Digest, SharedSessionId, SyncReceiptId},
    sync::{
        SharedSession as WireSharedSession, SharedSessionState, SyncBatchResponse, SyncDelta,
        SyncDeltaKind, SyncEnvelope, SyncReceipt as WireSyncReceipt, SyncRejection,
        TombstoneReason,
    },
    version::{CONTROL_PLANE_PROTOCOL_MIN_SUPPORTED, CONTROL_PLANE_PROTOCOL_V1},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    audit::digest_from_bytes,
    auth::{AuthPrincipal, Principal},
    authz::{
        authorize_organization_action, authorize_repository_action, parse_publication_class,
        Action, PublicationClass,
    },
    error::ControlPlaneError,
    state::{AppState, StreamEventMessage},
    store::{SharedSession, StreamEvent, SyncReceipt, Tombstone},
};

/// Most deltas one envelope may carry. A batch is processed inside a single
/// request, so an unbounded one is an unbounded amount of work per request.
const MAX_ENVELOPE_DELTAS: usize = 256;

/// The single rejection code for a delta the daemon may not write: no grant on
/// the repository, another tenant's repository, a repository that does not
/// exist, or a class the ceilings forbid.
///
/// One code and one reason for all four on purpose. A distinguishable
/// "unauthorized" and "not found" here is exactly the existence oracle the
/// route-level 404s were made uniform to prevent — batch rejections are part of
/// the response body and leak just as well as a status code.
const REJECTION_REFUSED: &str = "delta-refused";
const REJECTION_REFUSED_REASON: &str = "delta refused";

/// The delta kind is `Unknown`: emitted by a newer daemon and not projectable by
/// this build. Fail closed — never guess at the closest known projection.
const REJECTION_UNPROJECTABLE: &str = "unprojectable-delta-kind";

/// The delta is structurally unusable (sequence out of range, `payload_hash` not
/// a SHA-256 digest). A property of the delta itself, disclosing nothing about
/// any resource.
const REJECTION_MALFORMED: &str = "malformed-delta";

/// The one refusal for an envelope whose contents disagree with the credentials
/// that carried it. Collapses to the same 404 every other refusal produces.
fn envelope_refused() -> ControlPlaneError {
    ControlPlaneError::forbidden("sync", "sync envelope rejected")
}

fn refused(sequence: u64) -> SyncRejection {
    SyncRejection {
        sequence,
        code: REJECTION_REFUSED.to_string(),
        reason: REJECTION_REFUSED_REASON.to_string(),
    }
}

fn rejection(sequence: u64, code: &str, reason: &str) -> SyncRejection {
    SyncRejection {
        sequence,
        code: code.to_string(),
        reason: reason.to_string(),
    }
}

/// Decode a session state from a delta payload.
///
/// Anything unrecognized — including a non-string — becomes
/// [`SharedSessionState::Unknown`], which is never treated as terminal or as
/// approved. An absent state is `Running`, the protocol's default.
fn parse_session_state(payload: &serde_json::Value) -> SharedSessionState {
    match payload.get("state") {
        None => SharedSessionState::default(),
        Some(value) => serde_json::from_value(value.clone()).unwrap_or(SharedSessionState::Unknown),
    }
}

/// Decode a tombstone reason from a delta payload.
///
/// An absent or unrecognized reason is a full deletion, the most restrictive
/// outcome — the reading the protocol documents for
/// [`TombstoneReason::Unknown`].
fn parse_tombstone_reason(payload: &serde_json::Value) -> TombstoneReason {
    match payload.get("reason") {
        None => TombstoneReason::Deleted,
        Some(value) => serde_json::from_value(value.clone()).unwrap_or(TombstoneReason::Unknown),
    }
}

/// `tombstones.reason` has a CHECK constraint listing exactly `deleted`,
/// `narrowed` and `revoked`. `Unknown` is stored as the deletion it is treated
/// as, rather than being written verbatim and failing the constraint.
fn tombstone_reason_to_db_str(reason: TombstoneReason) -> &'static str {
    match reason {
        TombstoneReason::Narrowed => "narrowed",
        TombstoneReason::Revoked => "revoked",
        TombstoneReason::Deleted => "deleted",
        _ => "deleted",
    }
}

/// The payload a *subscriber* may see for a delta stored at `class`.
///
/// The projection below redacts `shared_sessions.title` beneath
/// `content-shared`, but the stream event embedded `delta.payload` verbatim, so
/// every subscriber authorized to read the repository was handed back exactly
/// the field the projection had just removed. Half a redaction is no redaction:
/// the class filter has to be applied on both paths, and the stream is the one
/// that fans out.
///
/// An ALLOW-list, not a deny-list. `payload` is free-form JSON from the daemon,
/// so naming the fields to strip would leave every field a newer daemon adds
/// forwarded by default. Beneath `content-shared` only the bounded operational
/// values this build derives itself are emitted — which is the whole of what
/// `metadata-shared` is defined to carry ("bounded operational metadata only.
/// No titles/content").
fn stream_payload_for_class(
    kind: SyncDeltaKind,
    payload: &serde_json::Value,
    class: PublicationClass,
) -> serde_json::Value {
    // `permits_in_ceiling` rather than `>=`: `Unknown` is the last declared
    // variant, so derived ordering would place it above every named class and
    // un-redact exactly the content it must not.
    if PublicationClass::ContentShared.permits_in_ceiling(class) {
        return payload.clone();
    }
    match kind {
        SyncDeltaKind::SessionSummary => {
            serde_json::json!({ "state": parse_session_state(payload).as_str() })
        }
        SyncDeltaKind::Tombstone => serde_json::json!({
            "reason": tombstone_reason_to_db_str(parse_tombstone_reason(payload)),
        }),
        // No projection exists for these kinds in this build, so nothing about
        // them is known to be bounded operational metadata. Emit nothing.
        _ => serde_json::json!({}),
    }
}

/// Project a stored receipt row onto the wire type.
fn receipt_to_wire(
    row: &SyncReceipt,
    duplicate: bool,
) -> Result<WireSyncReceipt, ControlPlaneError> {
    let daemon_sequence = u64::try_from(row.daemon_sequence)
        .map_err(|_| ControlPlaneError::Internal("stored sync sequence is negative".to_string()))?;

    Ok(WireSyncReceipt {
        id: SyncReceiptId::from_uuid(row.id),
        daemon_id: DaemonId::from_uuid(row.daemon_id),
        daemon_sequence,
        // A stored kind this build cannot name decodes to `Unknown` rather than
        // to the nearest match.
        delta_kind: serde_json::from_value(serde_json::Value::String(row.delta_kind.clone()))
            .unwrap_or(SyncDeltaKind::Unknown),
        payload_hash: digest_from_bytes(&row.payload_hash)?,
        // The class the control plane actually stored, which may be narrower
        // than the one the daemon asked for.
        class: parse_publication_class(&row.class),
        accepted_at: row.accepted_at,
        duplicate,
    })
}

/// Project a stored shared-session row onto the wire type.
fn shared_session_to_wire(row: SharedSession) -> WireSharedSession {
    WireSharedSession {
        id: SharedSessionId::from_uuid(row.id),
        organization_id: OrganizationId::from_uuid(row.organization_id),
        repository_id: RepositoryId::from_uuid(row.repository_id),
        daemon_id: DaemonId::from_uuid(row.daemon_id),
        remote_session_key: row.remote_session_key,
        class: parse_publication_class(&row.class),
        title: row.title,
        state: serde_json::from_value(serde_json::Value::String(row.state))
            .unwrap_or(SharedSessionState::Unknown),
        started_at: row.started_at,
        last_activity_at: row.last_activity_at,
        tombstoned_at: row.tombstoned_at,
        updated_at: row.updated_at,
    }
}

/// Accept one delta from an authenticated daemon's batch.
///
/// The two error channels are different failures. The outer `Err` is an
/// infrastructure fault (the database is unreachable, a stored row is corrupt)
/// and aborts the whole batch, because continuing would report acceptances the
/// control plane cannot stand behind. The inner `Err` is a
/// [`SyncRejection`]: this delta is refused, the rest of the batch is not.
#[allow(clippy::too_many_arguments)]
async fn accept_delta(
    state: &AppState,
    principal: &Principal,
    daemon_id: Uuid,
    org_id: Uuid,
    org_ceiling: PublicationClass,
    daemon_ceiling: PublicationClass,
    delta: &SyncDelta,
    now: DateTime<Utc>,
) -> Result<Result<WireSyncReceipt, SyncRejection>, ControlPlaneError> {
    if !delta.kind.is_projectable() {
        return Ok(Err(rejection(
            delta.sequence,
            REJECTION_UNPROJECTABLE,
            "delta kind is not recognized by this control plane",
        )));
    }

    let Ok(sequence) = i64::try_from(delta.sequence) else {
        return Ok(Err(rejection(
            delta.sequence,
            REJECTION_MALFORMED,
            "sequence is out of range",
        )));
    };

    // `Sha256Digest` is `#[serde(transparent)]` over `String`, so a digest that
    // arrived over the wire has never been validated. Validate before it becomes
    // a stored hash that nothing can reproduce.
    let Ok(payload_hash) = Sha256Digest::new(delta.payload_hash.0.clone()) else {
        return Ok(Err(rejection(
            delta.sequence,
            REJECTION_MALFORMED,
            "payload_hash is not a sha-256 digest",
        )));
    };
    let payload_hash_bytes = hex::decode(payload_hash.as_str()).map_err(|e| {
        ControlPlaneError::Internal(format!("validated digest failed to decode: {e}"))
    })?;

    // `repository_id` is attacker-controlled request input. Every write this
    // handler performs is attributed to a repository, so the repository must be
    // named and the principal authorized on it before anything is recorded. An
    // absent repository is refused rather than defaulted to organization-wide.
    let Some(repository_id) = delta.repository_id else {
        return Ok(Err(refused(delta.sequence)));
    };
    let repo_id = repository_id.as_uuid();

    // Resolves the repository within the principal's organization and checks the
    // grant. Another tenant's repository is indistinguishable from one that does
    // not exist.
    match authorize_repository_action(
        state.store.as_ref(),
        principal,
        org_id,
        repo_id,
        Action::SyncPush,
    )
    .await
    {
        Ok(_) => {}
        Err(ControlPlaneError::NotFound { .. }) => return Ok(Err(refused(delta.sequence))),
        Err(other) => return Err(other),
    }

    // Tenant-scoped in the query: the organization is in the WHERE clause, so a
    // repository belonging to another tenant is never fetched and then judged.
    let Some(repo) = state.store.get_repository_in_org(org_id, repo_id).await? else {
        return Ok(Err(refused(delta.sequence)));
    };

    // Publication ceiling is a three-way intersection: organization ∩ repository
    // ∩ daemon, then clamped by what the daemon actually requested (Design §4,
    // §8.3, §12.3). The daemon ceiling alone is not the ceiling — a permissive
    // daemon must never widen its organization's or repository's policy. An
    // unrecognized class on any side collapses the intersection to private-local.
    let effective_class = org_ceiling
        .intersect(parse_publication_class(&repo.max_publication_class))
        .intersect(daemon_ceiling)
        .intersect(delta.class);

    // private-local means "never leaves the machine". Publishing it to the
    // control plane would contradict the class, and an unrecognized class
    // narrows to private-local, so refuse instead of persisting.
    if !effective_class.allows_off_device() {
        return Ok(Err(refused(delta.sequence)));
    }

    let receipt_id = Uuid::now_v7();
    let is_new = state
        .store
        .record_sync_receipt(SyncReceipt {
            id: receipt_id,
            daemon_id,
            daemon_sequence: sequence,
            delta_kind: delta.kind.as_str().to_string(),
            payload_hash: payload_hash_bytes,
            class: effective_class.as_str().to_string(),
            accepted_at: now,
        })
        .await?;

    if !is_new {
        // Idempotent redelivery. The projection is not re-applied, and the
        // receipt reported is the one that was actually written — its id, its
        // stored class, its acceptance time. Minting a fresh receipt here would
        // report an effect that never happened.
        let stored = state
            .store
            .get_sync_receipt(daemon_id, sequence)
            .await?
            .ok_or_else(|| {
                ControlPlaneError::Internal(
                    "sync receipt disappeared between insert and read".to_string(),
                )
            })?;
        return Ok(Ok(receipt_to_wire(&stored, true)?));
    }

    match delta.kind {
        SyncDeltaKind::SessionSummary => {
            // `permits_in_ceiling` rather than `>=`: `Unknown` is the last
            // declared variant, so derived ordering would place it above every
            // named class and un-redact exactly the content it must not.
            let title = if PublicationClass::ContentShared.permits_in_ceiling(effective_class) {
                delta
                    .payload
                    .get("title")
                    .and_then(|t| t.as_str())
                    .map(ToString::to_string)
            } else {
                None // Redacted below content-shared per §3.2
            };

            state
                .store
                .upsert_shared_session(SharedSession {
                    id: Uuid::now_v7(),
                    organization_id: org_id,
                    repository_id: repo_id,
                    daemon_id,
                    remote_session_key: delta.subject_id.clone(),
                    class: effective_class.as_str().to_string(),
                    title,
                    state: parse_session_state(&delta.payload).as_str().to_string(),
                    started_at: now,
                    last_activity_at: Some(now),
                    tombstoned_at: None,
                    updated_at: now,
                })
                .await?;
        }
        SyncDeltaKind::Tombstone => {
            state
                .store
                .create_tombstone(Tombstone {
                    id: Uuid::now_v7(),
                    organization_id: org_id,
                    subject_kind: delta.kind.as_str().to_string(),
                    subject_key: delta.subject_id.clone(),
                    reason: tombstone_reason_to_db_str(parse_tombstone_reason(&delta.payload))
                        .to_string(),
                    created_at: now,
                    applied_at: Some(now),
                })
                .await?;
        }
        _ => {}
    }

    // Persist before publish. Always stamped with the authorized repository so
    // delivery can be scoped to it: an event with no repository_id is
    // undeliverable without leaking it to every subscriber in the organization.
    let appended = state
        .store
        .append_stream_event(StreamEvent {
            id: 0,
            organization_id: org_id,
            repository_id: Some(repo_id),
            stream: "sync".to_string(),
            payload: serde_json::json!({
                "delta_kind": delta.kind.as_str(),
                "subject_id": delta.subject_id,
                "class": effective_class.as_str(),
                "payload": stream_payload_for_class(delta.kind, &delta.payload, effective_class),
            }),
            created_at: now,
        })
        .await?;

    let _ = state.events_tx.send(StreamEventMessage {
        id: appended.id,
        organization_id: org_id,
        repository_id: Some(repo_id),
        stream: appended.stream,
        payload: appended.payload,
    });

    Ok(Ok(WireSyncReceipt {
        id: SyncReceiptId::from_uuid(receipt_id),
        daemon_id: DaemonId::from_uuid(daemon_id),
        daemon_sequence: delta.sequence,
        delta_kind: delta.kind,
        payload_hash,
        class: effective_class,
        accepted_at: now,
        duplicate: false,
    }))
}

/// `POST /v1/sync/push` — accept a batch of outbound deltas from a paired
/// daemon.
pub async fn push_sync_envelope(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Json(envelope): Json<SyncEnvelope>,
) -> Result<Json<SyncBatchResponse>, ControlPlaneError> {
    let (daemon_id, org_id, daemon_max_class) = match &principal {
        Principal::Daemon {
            daemon_id,
            organization_id,
            max_publication_class,
            ..
        } => (*daemon_id, *organization_id, max_publication_class.clone()),
        _ => {
            return Err(ControlPlaneError::Forbidden {
                resource: "sync".to_string(),
                message: "only paired daemons can push sync deltas".to_string(),
            })
        }
    };

    // A major version this build does not implement is refused outright: the
    // deltas inside would be interpreted under contract terms that have changed.
    if !envelope
        .protocol_version
        .is_compatible_with(&CONTROL_PLANE_PROTOCOL_V1)
        || envelope.protocol_version < CONTROL_PLANE_PROTOCOL_MIN_SUPPORTED
    {
        return Err(ControlPlaneError::BadRequest(format!(
            "unsupported control plane protocol version {}; this control plane accepts {} through {}",
            envelope.protocol_version,
            CONTROL_PLANE_PROTOCOL_MIN_SUPPORTED,
            CONTROL_PLANE_PROTOCOL_V1,
        )));
    }

    // The envelope names its own daemon and organization. Those are claims, and
    // the credentials are the fact: a daemon must not be able to file a batch as
    // another daemon or into another tenant. Nothing is looked up to decide
    // this, so the refusal cannot confirm that the named ids exist.
    if envelope.daemon_id.as_uuid() != daemon_id || envelope.organization_id.as_uuid() != org_id {
        return Err(envelope_refused());
    }

    if envelope.deltas.len() > MAX_ENVELOPE_DELTAS {
        return Err(ControlPlaneError::BadRequest(format!(
            "sync envelope carries more than {MAX_ENVELOPE_DELTAS} deltas"
        )));
    }

    let org = state
        .store
        .get_organization(org_id)
        .await?
        .ok_or_else(|| ControlPlaneError::not_found("organization", "no such organization"))?;
    let org_ceiling = parse_publication_class(&org.max_publication_class);
    let daemon_ceiling = parse_publication_class(&daemon_max_class);

    let now = Utc::now();
    let mut receipts = Vec::with_capacity(envelope.deltas.len());
    let mut rejected_deltas = Vec::new();

    for delta in &envelope.deltas {
        match accept_delta(
            &state,
            &principal,
            daemon_id,
            org_id,
            org_ceiling,
            daemon_ceiling,
            delta,
            now,
        )
        .await?
        {
            Ok(receipt) => receipts.push(receipt),
            Err(rejection) => rejected_deltas.push(rejection),
        }
    }

    // The daemon's own high-water mark, read back from the store so a batch in
    // which everything was a replay still reports the truth. `None` means no
    // delta from this daemon has ever been accepted; the protocol field is a
    // plain `u64`, so that is reported as 0 — the identity of an empty maximum
    // over sequence numbers, not a measurement of anything.
    let latest_sequence = match state.store.latest_sync_sequence(daemon_id).await? {
        Some(seq) => u64::try_from(seq).map_err(|_| {
            ControlPlaneError::Internal("stored sync sequence is negative".to_string())
        })?,
        None => 0,
    };

    Ok(Json(SyncBatchResponse {
        receipts,
        latest_sequence,
        rejected_deltas,
    }))
}

#[derive(Debug, Deserialize)]
pub struct PullQuery {
    pub repository_id: Option<Uuid>,
    pub stream: Option<String>,
    pub after_id: Option<i64>,
    pub limit: Option<usize>,
}

/// `GET /v1/sync/pull` — replay durable events for one authorized repository.
///
/// This returns the stored [`StreamEvent`] rather than the protocol's, and that
/// is a known gap rather than an oversight: the protocol's `StreamEventPayload`
/// is a closed enum of notification/approval/schedule/runner/policy payloads with
/// no variant for a synchronization echo, so projecting onto it would erase every
/// payload this stream actually carries into `Unknown`. Requires a protocol
/// change (a sync-delta payload variant) before it can be projected honestly.
pub async fn pull_sync_events(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Query(query): Query<PullQuery>,
) -> Result<Json<Vec<StreamEvent>>, ControlPlaneError> {
    let org_id = match &principal {
        Principal::Daemon {
            organization_id, ..
        } => *organization_id,
        _ => {
            return Err(ControlPlaneError::Forbidden {
                resource: "sync".to_string(),
                message: "only daemons can pull sync events".to_string(),
            })
        }
    };

    // Streams are repository-scoped at delivery. Pulling with no repository
    // returned every repository's events in the organization to any subscriber,
    // so the subscription must name one repository and be authorized on it.
    let repo_id = query
        .repository_id
        .ok_or_else(|| ControlPlaneError::BadRequest("repository_id is required".to_string()))?;

    authorize_repository_action(
        state.store.as_ref(),
        &principal,
        org_id,
        repo_id,
        Action::SyncPull,
    )
    .await?;

    let stream = query.stream.unwrap_or_else(|| "sync".to_string());
    let after_id = query.after_id.unwrap_or(0);
    let limit = query.limit.unwrap_or(50).min(100);

    let events = state
        .store
        .query_stream_events(org_id, Some(repo_id), &stream, after_id, limit)
        .await?;

    Ok(Json(events))
}

#[derive(Debug, Deserialize)]
pub struct SessionsQuery {
    pub repository_id: Option<Uuid>,
    pub limit: Option<usize>,
}

pub async fn list_shared_sessions(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path(org_id): Path<Uuid>,
    Query(query): Query<SessionsQuery>,
) -> Result<Json<Vec<WireSharedSession>>, ControlPlaneError> {
    authorize_organization_action(state.store.as_ref(), &principal, org_id, Action::Read).await?;

    // Organization membership is not authorization for every repository in it.
    // Listing with repository_id absent returned sessions from repositories the
    // caller holds no grant on, so the repository must be named and authorized.
    let repo_id = query
        .repository_id
        .ok_or_else(|| ControlPlaneError::BadRequest("repository_id is required".to_string()))?;

    authorize_repository_action(
        state.store.as_ref(),
        &principal,
        org_id,
        repo_id,
        Action::Read,
    )
    .await?;

    let limit = query.limit.unwrap_or(50).min(100);
    let sessions = state
        .store
        .list_shared_sessions(org_id, Some(repo_id), limit)
        .await?;

    Ok(Json(
        sessions.into_iter().map(shared_session_to_wire).collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unrecognized_session_state_is_never_read_as_a_named_state() {
        assert_eq!(
            parse_session_state(&serde_json::json!({})),
            SharedSessionState::Running,
            "an absent state is the protocol default"
        );
        assert_eq!(
            parse_session_state(&serde_json::json!({ "state": "completed" })),
            SharedSessionState::Completed
        );
        for payload in [
            serde_json::json!({ "state": "active" }),
            serde_json::json!({ "state": 7 }),
            serde_json::json!({ "state": null }),
        ] {
            assert_eq!(parse_session_state(&payload), SharedSessionState::Unknown);
        }
    }

    #[test]
    fn an_unrecognized_tombstone_reason_is_stored_as_the_deletion_it_is_treated_as() {
        assert_eq!(
            parse_tombstone_reason(&serde_json::json!({})),
            TombstoneReason::Deleted
        );
        assert_eq!(
            parse_tombstone_reason(&serde_json::json!({ "reason": "narrowed" })),
            TombstoneReason::Narrowed
        );
        let unknown = parse_tombstone_reason(&serde_json::json!({ "reason": "shredded" }));
        assert_eq!(unknown, TombstoneReason::Unknown);
        // The column's CHECK constraint has no `unknown`, and the most
        // restrictive reading of an unknown reason is a full deletion.
        assert_eq!(tombstone_reason_to_db_str(unknown), "deleted");
    }

    /// The projection redacts `shared_sessions.title` below `content-shared`.
    /// The stream event did not: it embedded `delta.payload` verbatim, so the
    /// title the projection had just removed was broadcast to every subscriber
    /// authorized to read the repository — and persisted into `stream_events`,
    /// where the WebSocket replay hands it out again on reconnect.
    ///
    /// Drives the real handler so the assertion is about what subscribers
    /// actually receive, not about a helper in isolation.
    #[tokio::test]
    async fn a_title_redacted_from_the_projection_is_redacted_from_the_stream_too() {
        use crate::{
            config::ControlPlaneConfig,
            storage::MemoryStorageDriver,
            store::{memory::MemoryStore, Organization, Repository, RoleGrant, Store as _},
        };
        use axum::{extract::State, Json};
        use codypendent_control_plane_protocol::ids::{OrganizationId, RepositoryId, Sha256Digest};
        use std::sync::Arc;

        let org_id = Uuid::now_v7();
        let repo_id = Uuid::now_v7();
        let daemon_id = Uuid::now_v7();
        let user_id = Uuid::now_v7();
        let now = Utc::now();

        let store = Arc::new(MemoryStore::new());
        store
            .create_organization(Organization {
                id: org_id,
                slug: "acme".to_string(),
                display_name: "Acme".to_string(),
                // Wide at the organization and repository, so the only thing
                // narrowing the delta is the daemon's own pairing ceiling.
                max_publication_class: "content-shared".to_string(),
                max_classification: "internal".to_string(),
                data_residency: None,
                retention_days: None,
                policy_version: 1,
                created_at: now,
            })
            .await
            .expect("organization");
        store
            .create_repository(Repository {
                id: repo_id,
                organization_id: org_id,
                federated_id: "a".repeat(64),
                display_name: "Core".to_string(),
                max_publication_class: "content-shared".to_string(),
                max_classification: "internal".to_string(),
                policy_version: 1,
                created_at: now,
            })
            .await
            .expect("repository");
        store
            .create_role_grant(RoleGrant {
                id: Uuid::now_v7(),
                organization_id: org_id,
                user_id: Some(user_id),
                team_id: None,
                repository_id: None,
                role: "contributor".to_string(),
                action_scope: None,
                granted_by: user_id,
                granted_at: now,
                expires_at: None,
                revoked_at: None,
            })
            .await
            .expect("grant");

        let config = ControlPlaneConfig::from_env_with_jwt_secret(
            "ctrl-plane-unit-test-signing-key-0123456789abcdef",
        )
        .expect("test signing secret");
        let state = AppState::new(
            config,
            store.clone() as Arc<dyn crate::store::Store + Send + Sync>,
            Arc::new(MemoryStorageDriver::new()),
        );
        // Subscribe BEFORE the push: this is the live fan-out a WebSocket
        // subscriber sits on.
        let mut subscriber = state.events_tx.subscribe();

        let payload = serde_json::json!({
            "title": "Rotate the production signing key",
            "state": "completed",
        });
        let envelope = SyncEnvelope {
            protocol_version: CONTROL_PLANE_PROTOCOL_V1,
            daemon_id: DaemonId::from_uuid(daemon_id),
            organization_id: OrganizationId::from_uuid(org_id),
            sent_at: now,
            deltas: vec![SyncDelta {
                id: "delta-1".to_string(),
                sequence: 1,
                kind: SyncDeltaKind::SessionSummary,
                repository_id: Some(RepositoryId::from_uuid(repo_id)),
                subject_id: "sess_1".to_string(),
                payload: payload.clone(),
                class: PublicationClass::ContentShared,
                payload_hash: Sha256Digest::from_bytes(&serde_json::to_vec(&payload).unwrap()),
                created_at: now,
            }],
        };

        let principal = Principal::Daemon {
            daemon_id,
            organization_id: org_id,
            paired_by: user_id,
            // The pairing ceiling, and the whole point: it clamps the delta to
            // metadata-shared, below content-shared.
            max_publication_class: "metadata-shared".to_string(),
        };

        let response = push_sync_envelope(
            State(state.clone()),
            crate::auth::AuthPrincipal(principal),
            Json(envelope),
        )
        .await
        .expect("push accepted");
        assert_eq!(response.0.receipts.len(), 1);
        assert_eq!(
            response.0.receipts[0].class,
            PublicationClass::MetadataShared
        );

        // The projection redacts.
        let sessions = store
            .list_shared_sessions(org_id, Some(repo_id), 10)
            .await
            .expect("sessions");
        assert_eq!(sessions[0].title, None);

        // So must the live broadcast...
        let broadcast = subscriber.try_recv().expect("one stream event");
        let broadcast_text = serde_json::to_string(&broadcast.payload).expect("serialize");
        assert!(
            !broadcast_text.contains("Rotate the production signing key"),
            "the stream event handed subscribers the title the projection redacted: \
             {broadcast_text}"
        );

        // ...and the persisted row the WebSocket replays on reconnect.
        let replayed = store
            .query_stream_events(org_id, Some(repo_id), "sync", 0, 10)
            .await
            .expect("stream events");
        let replay_text = serde_json::to_string(&replayed).expect("serialize");
        assert!(
            !replay_text.contains("Rotate the production signing key"),
            "the stored stream event replays the redacted title: {replay_text}"
        );
    }

    #[test]
    fn every_refusal_of_a_delta_is_byte_identical() {
        // A repository in another tenant, one that does not exist, one the
        // daemon holds no grant on and a class the ceilings forbid all reach
        // `refused`. If they ever stopped being the same bytes, the difference
        // would be an existence oracle inside the batch response.
        let a = refused(1);
        let b = refused(2);
        assert_eq!(a.code, b.code);
        assert_eq!(a.reason, b.reason);
        assert_eq!(a.code, REJECTION_REFUSED);
    }
}
