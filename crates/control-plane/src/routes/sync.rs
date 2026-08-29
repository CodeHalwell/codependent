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
    events::{StreamEvent as WireStreamEvent, StreamEventPayload, StreamKind, SyncDeltaEvent},
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
        authorize_organization_action, authorize_repository_action, parse_data_classification,
        parse_publication_class, Action, DataClassification, PublicationClass,
    },
    error::ControlPlaneError,
    state::{AppState, StreamEventMessage},
    store::{
        SharedSession, SharedSessionTombstone, StreamEvent as StoredStreamEvent,
        SyncDeltaApplication, SyncDeltaOutcome, SyncProjection, SyncReceipt, Tombstone,
    },
};

/// Most deltas one envelope may carry. A batch is processed inside a single
/// request, so an unbounded one is an unbounded amount of work per request.
const MAX_ENVELOPE_DELTAS: usize = 256;

/// Maximum size of the opaque identifier a daemon may attach to one delta.
///
/// The identifier is persisted in projections and every stream echo. Leaving it
/// unbounded lets one otherwise-small delta amplify into several unbounded
/// database values. It remains opaque: the control plane validates only the
/// properties required to store and replay it safely.
const MAX_SYNC_SUBJECT_ID_BYTES: usize = 1_024;
const MAX_TOMBSTONE_SUBJECT_KIND_BYTES: usize = 64;

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

fn is_valid_sync_subject_id(subject_id: &str) -> bool {
    !subject_id.trim().is_empty()
        && subject_id.len() <= MAX_SYNC_SUBJECT_ID_BYTES
        && !subject_id.chars().any(char::is_control)
}

fn parse_tombstone_subject<'a>(
    payload: &'a serde_json::Value,
    expected_subject_id: &str,
) -> Option<(&'a str, &'a str)> {
    let subject_kind = payload.get("subject_kind")?.as_str()?;
    let subject_key = payload.get("subject_key")?.as_str()?;
    if subject_kind.trim().is_empty()
        || subject_kind.len() > MAX_TOMBSTONE_SUBJECT_KIND_BYTES
        || subject_kind.chars().any(char::is_control)
        || !is_valid_sync_subject_id(subject_key)
        || subject_key != expected_subject_id
    {
        return None;
    }
    Some((subject_kind, subject_key))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClassifiedDeltaDecision {
    Allowed,
    Malformed,
    Refused,
}

/// Independently enforce data sensitivity at the receiving trust boundary.
///
/// Publication class and data classification are separate ceilings. A daemon
/// that labels the envelope `metadata-shared` must not be able to smuggle an
/// Internal artifact into a repository capped at Public, and a graph batch is
/// only as safe as its most sensitive fact. Missing/newer labels fail closed.
fn classified_delta_decision(
    kind: SyncDeltaKind,
    payload: &serde_json::Value,
    publication_ceiling: PublicationClass,
    classification_ceiling: DataClassification,
) -> ClassifiedDeltaDecision {
    fn classification(value: Option<&serde_json::Value>) -> Option<DataClassification> {
        let parsed = parse_data_classification(value?.as_str()?);
        (parsed != DataClassification::Unknown).then_some(parsed)
    }

    match kind {
        SyncDeltaKind::ArtifactSummary => {
            let Some(classification) = classification(payload.get("classification")) else {
                return ClassifiedDeltaDecision::Malformed;
            };
            if classification.permits(classification_ceiling) {
                ClassifiedDeltaDecision::Allowed
            } else {
                ClassifiedDeltaDecision::Refused
            }
        }
        SyncDeltaKind::GraphBatch => {
            let Some(facts) = payload.get("facts").and_then(serde_json::Value::as_array) else {
                return ClassifiedDeltaDecision::Malformed;
            };
            for fact in facts {
                let Some(classification) = classification(fact.get("classification")) else {
                    return ClassifiedDeltaDecision::Malformed;
                };
                let Some(raw_class) = fact.get("class").and_then(serde_json::Value::as_str) else {
                    return ClassifiedDeltaDecision::Malformed;
                };
                let fact_class = parse_publication_class(raw_class);
                if fact_class == PublicationClass::Unknown {
                    return ClassifiedDeltaDecision::Malformed;
                }
                if !fact_class.allows_off_device()
                    || !fact_class.permits_in_ceiling(publication_ceiling)
                    || !classification.permits(classification_ceiling)
                {
                    return ClassifiedDeltaDecision::Refused;
                }
            }
            ClassifiedDeltaDecision::Allowed
        }
        _ => ClassifiedDeltaDecision::Allowed,
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
            "subject_kind": payload.get("subject_kind").and_then(|value| value.as_str()),
            "subject_key": payload.get("subject_key").and_then(|value| value.as_str()),
            "reason": tombstone_reason_to_db_str(parse_tombstone_reason(payload)),
        }),
        SyncDeltaKind::RunSummary => serde_json::json!({
            "run_id": payload.get("run_id").and_then(|value| value.as_str()),
            "session_id": payload.get("session_id").and_then(|value| value.as_str()),
            "repository_id": payload.get("repository_id").and_then(|value| value.as_str()),
            "state": payload.get("state").and_then(|value| value.as_str()),
            "started_at": payload.get("started_at").and_then(|value| value.as_str()),
            "completed_at": payload.get("completed_at").and_then(|value| value.as_str()),
            "prompt_tokens": payload.get("prompt_tokens").and_then(|value| value.as_i64()),
            "completion_tokens": payload.get("completion_tokens").and_then(|value| value.as_i64()),
            "cost_micros": payload.get("cost_micros").and_then(|value| value.as_i64()),
            "sync_revision": payload.get("sync_revision").and_then(|value| value.as_i64()),
        }),
        SyncDeltaKind::ArtifactSummary => serde_json::json!({
            "artifact_id": payload.get("artifact_id").and_then(|value| value.as_str()),
            "repository_id": payload.get("repository_id").and_then(|value| value.as_str()),
            "name": payload.get("name").and_then(|value| value.as_str()),
            "content_hash": payload.get("content_hash").and_then(|value| value.as_str()),
            "byte_length": payload.get("byte_length").and_then(|value| value.as_i64()),
            "media_type": payload.get("media_type").and_then(|value| value.as_str()),
        }),
        SyncDeltaKind::GraphBatch => {
            let facts = payload
                .get("facts")
                .and_then(serde_json::Value::as_array)
                .map(|facts| {
                    facts
                        .iter()
                        .map(|fact| serde_json::json!({
                            "subject_kind": fact.get("subject_kind").and_then(|value| value.as_str()),
                            "subject_id": fact.get("subject_id").and_then(|value| value.as_str()),
                            "class": fact.get("class").and_then(|value| value.as_str()),
                            "classification": fact.get("classification").and_then(|value| value.as_str()),
                            "content_hash": fact.get("content_hash").and_then(|value| value.as_str()),
                        }))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            serde_json::json!({
                "batch_id": payload.get("batch_id").and_then(|value| value.as_str()),
                "repository_id": payload.get("repository_id").and_then(|value| value.as_str()),
                "facts": facts,
            })
        }
        SyncDeltaKind::ApprovalDecision => {
            let detail = payload.get("detail").unwrap_or(&serde_json::Value::Null);
            serde_json::json!({
                "event_id": payload.get("event_id").and_then(|value| value.as_str()),
                "action": payload.get("action").and_then(|value| value.as_str()),
                "actor_kind": payload.get("actor_kind").and_then(|value| value.as_str()),
                "target_kind": payload.get("target_kind").and_then(|value| value.as_str()),
                "target_id": payload.get("target_id").and_then(|value| value.as_str()),
                "digest": payload.get("digest").and_then(|value| value.as_str()),
                "detail": {
                    "decision": detail.get("decision").and_then(|value| value.as_str()),
                    "scope": detail.get("scope").and_then(|value| value.as_str()),
                    "run_id": detail.get("run_id").and_then(|value| value.as_str()),
                    "session_id": detail.get("session_id").and_then(|value| value.as_str()),
                    "repository_id": detail.get("repository_id").and_then(|value| value.as_str()),
                },
            })
        }
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
    org_classification_ceiling: DataClassification,
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

    if !is_valid_sync_subject_id(&delta.subject_id) {
        return Ok(Err(rejection(
            delta.sequence,
            REJECTION_MALFORMED,
            "subject_id is empty, oversized, or contains control characters",
        )));
    }

    let tombstone_subject = if delta.kind == SyncDeltaKind::Tombstone {
        let Some(subject) = parse_tombstone_subject(&delta.payload, &delta.subject_id) else {
            return Ok(Err(rejection(
                delta.sequence,
                REJECTION_MALFORMED,
                "tombstone subject_kind and matching subject_key are required",
            )));
        };
        Some(subject)
    } else {
        None
    };

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
    let serialized_payload = serde_json::to_vec(&delta.payload).map_err(|error| {
        ControlPlaneError::Internal(format!("failed to serialize sync payload: {error}"))
    })?;
    let calculated_payload_hash = Sha256Digest::from_bytes(&serialized_payload);
    if payload_hash != calculated_payload_hash {
        return Ok(Err(rejection(
            delta.sequence,
            REJECTION_MALFORMED,
            "payload_hash does not match payload",
        )));
    }
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
    let effective_class = if delta.kind == SyncDeltaKind::Tombstone {
        // A retraction is a deletion control message, not a new publication.
        // Repository/organization policy may have narrowed to private-local
        // precisely because previously shared data must now disappear. Keep
        // authorization and consent checks above, but allow the minimal
        // metadata tombstone through so narrowing cannot strand remote data.
        PublicationClass::MetadataShared.intersect(delta.class)
    } else {
        org_ceiling
            .intersect(parse_publication_class(&repo.max_publication_class))
            .intersect(daemon_ceiling)
            .intersect(delta.class)
    };

    // private-local means "never leaves the machine". Publishing it to the
    // control plane would contradict the class, and an unrecognized class
    // narrows to private-local, so refuse instead of persisting.
    if !effective_class.allows_off_device() {
        return Ok(Err(refused(delta.sequence)));
    }

    let repository_classification_ceiling = parse_data_classification(&repo.max_classification);
    let effective_classification_ceiling = if org_classification_ceiling
        == DataClassification::Unknown
        || repository_classification_ceiling == DataClassification::Unknown
    {
        DataClassification::Unknown
    } else {
        org_classification_ceiling.intersect(repository_classification_ceiling)
    };
    match classified_delta_decision(
        delta.kind,
        &delta.payload,
        effective_class,
        effective_classification_ceiling,
    ) {
        ClassifiedDeltaDecision::Allowed => {}
        ClassifiedDeltaDecision::Malformed => {
            return Ok(Err(rejection(
                delta.sequence,
                REJECTION_MALFORMED,
                "classified delta payload is missing a recognized class or classification",
            )))
        }
        ClassifiedDeltaDecision::Refused => return Ok(Err(refused(delta.sequence))),
    }

    let receipt_id = Uuid::now_v7();
    let receipt = SyncReceipt {
        id: receipt_id,
        daemon_id,
        daemon_sequence: sequence,
        delta_kind: delta.kind.as_str().to_string(),
        payload_hash: payload_hash_bytes,
        class: effective_class.as_str().to_string(),
        accepted_at: now,
    };

    let projection = match delta.kind {
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

            SyncProjection::SharedSession(Box::new(SharedSession {
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
            }))
        }
        SyncDeltaKind::Tombstone => {
            let (subject_kind, subject_key) =
                tombstone_subject.expect("validated every tombstone before projection");
            let shared_session = (subject_kind == "session").then(|| SharedSessionTombstone {
                organization_id: org_id,
                repository_id: repo_id,
                daemon_id,
                remote_session_key: subject_key.to_string(),
                tombstoned_at: now,
            });
            SyncProjection::Tombstone {
                record: Box::new(Tombstone {
                    id: Uuid::now_v7(),
                    organization_id: org_id,
                    subject_kind: subject_kind.to_string(),
                    subject_key: subject_key.to_string(),
                    reason: tombstone_reason_to_db_str(parse_tombstone_reason(&delta.payload))
                        .to_string(),
                    created_at: now,
                    applied_at: Some(now),
                }),
                shared_session,
            }
        }
        _ => SyncProjection::None,
    };

    // Always stamped with the authorized repository so delivery can be scoped
    // to it: an event with no repository_id is undeliverable without leaking it
    // to every subscriber in the organization.
    // The daemon's payload may carry a local alias solely so its offline
    // outbox can later resolve the control-plane repository UUID. Never echo
    // that machine-local identity to subscribers: the authorized route scope
    // is the canonical repository identity on this side of the boundary.
    let mut event_source_payload = delta.payload.clone();
    if let Some(object) = event_source_payload.as_object_mut() {
        object.insert(
            "repository_id".to_string(),
            serde_json::Value::String(repo_id.to_string()),
        );
    }
    let event = StoredStreamEvent {
        id: 0,
        organization_id: org_id,
        repository_id: Some(repo_id),
        stream: "sync".to_string(),
        payload: serde_json::json!({
            "delta_kind": delta.kind.as_str(),
            "subject_id": delta.subject_id,
            "class": effective_class.as_str(),
            "payload": stream_payload_for_class(delta.kind, &event_source_payload, effective_class),
        }),
        created_at: now,
    };

    // Receipt, projection and event commit together. Recording the receipt
    // first, in its own autocommit, meant a failure before the projection
    // landed left proof of an effect that never happened — and the daemon's
    // retry was then answered with that receipt and dropped the delta.
    let appended = match state
        .store
        .apply_sync_delta(SyncDeltaApplication {
            receipt,
            projection,
            event,
        })
        .await?
    {
        SyncDeltaOutcome::Applied(event) => *event,
        SyncDeltaOutcome::Duplicate => {
            // Idempotent redelivery. The projection is not re-applied, and the
            // receipt reported is the one that was actually written — its id,
            // its stored class, its acceptance time. Minting a fresh receipt
            // here would report an effect that never happened.
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
    };

    // Publish only after the commit: a subscriber must never be told about an
    // effect that a rollback then erased. The other order — publish, then
    // fail to commit — is unrecoverable, while a crash here leaves the event
    // durable and readable through the stream-event query.
    let _ = state.events_tx.send(StreamEventMessage {
        id: appended.id,
        organization_id: org_id,
        repository_id: Some(repo_id),
        stream: appended.stream,
        payload: appended.payload,
        created_at: appended.created_at,
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
    let org_classification_ceiling = parse_data_classification(&org.max_classification);
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
            org_classification_ceiling,
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

/// Decode the persisted envelope written by [`accept_delta`].
///
/// A stored sync payload is evidence only when every required field has the
/// exact shape the writer emits. Defaults are unsafe here: inventing an empty
/// subject or an unknown class would turn a corrupt row into a known effect on
/// the wire. Missing, malformed, or newer fields therefore collapse the whole
/// payload to [`StreamEventPayload::Unknown`].
fn stored_sync_delta_to_wire(payload: &serde_json::Value) -> Option<SyncDeltaEvent> {
    let object = payload.as_object()?;

    let delta_kind = serde_json::from_value(object.get("delta_kind")?.clone()).ok()?;
    if !SyncDeltaKind::is_projectable(delta_kind) {
        return None;
    }

    let subject_id = object.get("subject_id")?.as_str()?;
    if !is_valid_sync_subject_id(subject_id) {
        return None;
    }

    let class = parse_publication_class(object.get("class")?.as_str()?);
    if class == PublicationClass::Unknown {
        return None;
    }

    Some(SyncDeltaEvent {
        delta_kind,
        subject_id: subject_id.to_string(),
        class,
        // `null` is a valid opaque JSON payload, but absence is malformed.
        payload: object.get("payload")?.clone(),
    })
}

fn stream_event_to_wire(row: StoredStreamEvent) -> Result<WireStreamEvent, ControlPlaneError> {
    let id = u64::try_from(row.id).map_err(|_| {
        ControlPlaneError::Internal("stored stream event id is negative".to_string())
    })?;
    let stream = serde_json::from_value(serde_json::Value::String(row.stream.clone()))
        .unwrap_or(StreamKind::Unknown);
    let payload = if stream == StreamKind::Sync {
        if row.repository_id.is_none() {
            StreamEventPayload::Unknown
        } else {
            stored_sync_delta_to_wire(&row.payload)
                .map(StreamEventPayload::SyncDelta)
                .unwrap_or(StreamEventPayload::Unknown)
        }
    } else {
        serde_json::from_value(row.payload).unwrap_or(StreamEventPayload::Unknown)
    };

    Ok(WireStreamEvent {
        id,
        organization_id: OrganizationId::from_uuid(row.organization_id),
        repository_id: row.repository_id.map(RepositoryId::from_uuid),
        stream,
        payload,
        created_at: row.created_at,
    })
}

/// `GET /v1/sync/pull` — replay durable repository events, or the paired
/// organization's repository-independent policy stream.
///
/// Stored events are projected onto the shared protocol type at the HTTP
/// boundary. Sync echoes use the protocol's explicit redacted sync-delta
/// payload, while malformed or newer non-sync payloads fail closed to
/// `Unknown` rather than being guessed into a known effect.
pub async fn pull_sync_events(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Query(query): Query<PullQuery>,
) -> Result<Json<Vec<WireStreamEvent>>, ControlPlaneError> {
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

    let stream = query.stream.unwrap_or_else(|| "sync".to_string());
    let repository_scope = if stream == "policy" {
        if query.repository_id.is_some() {
            return Err(ControlPlaneError::BadRequest(
                "repository_id must be absent for the organization policy stream".to_string(),
            ));
        }
        authorize_organization_action(state.store.as_ref(), &principal, org_id, Action::SyncPull)
            .await?;
        None
    } else {
        // Every non-policy stream remains repository-scoped. Pulling one with
        // no repository would expose all repository events in the organization.
        let repo_id = query.repository_id.ok_or_else(|| {
            ControlPlaneError::BadRequest("repository_id is required".to_string())
        })?;
        authorize_repository_action(
            state.store.as_ref(),
            &principal,
            org_id,
            repo_id,
            Action::SyncPull,
        )
        .await?;
        Some(repo_id)
    };
    let after_id = query.after_id.unwrap_or(0);
    let limit = query.limit.unwrap_or(50).min(100);

    let mut events = state
        .store
        .query_stream_events(org_id, repository_scope, &stream, after_id, limit)
        .await?;
    if stream == "policy" {
        // A store query with no repository is organization-wide. Even if a
        // malformed producer writes a repository-scoped event into the policy
        // stream, it must not be reinterpreted as organization policy.
        events.retain(|event| event.repository_id.is_none());
    }

    let events = events
        .into_iter()
        .map(stream_event_to_wire)
        .collect::<Result<Vec<_>, _>>()?;

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

    #[test]
    fn classified_artifacts_and_graph_facts_fail_closed_at_the_receiver() {
        let public = DataClassification::Public;
        let metadata = PublicationClass::MetadataShared;

        assert_eq!(
            classified_delta_decision(
                SyncDeltaKind::ArtifactSummary,
                &serde_json::json!({ "classification": "public" }),
                metadata,
                public,
            ),
            ClassifiedDeltaDecision::Allowed
        );
        assert_eq!(
            classified_delta_decision(
                SyncDeltaKind::ArtifactSummary,
                &serde_json::json!({ "classification": "internal" }),
                metadata,
                public,
            ),
            ClassifiedDeltaDecision::Refused
        );
        assert_eq!(
            classified_delta_decision(
                SyncDeltaKind::ArtifactSummary,
                &serde_json::json!({}),
                metadata,
                public,
            ),
            ClassifiedDeltaDecision::Malformed
        );

        let graph = |class: &str, classification: &str| {
            serde_json::json!({
                "facts": [{ "class": class, "classification": classification }]
            })
        };
        assert_eq!(
            classified_delta_decision(
                SyncDeltaKind::GraphBatch,
                &graph("metadata-shared", "public"),
                metadata,
                public,
            ),
            ClassifiedDeltaDecision::Allowed
        );
        assert_eq!(
            classified_delta_decision(
                SyncDeltaKind::GraphBatch,
                &graph("content-shared", "public"),
                metadata,
                public,
            ),
            ClassifiedDeltaDecision::Refused
        );
        assert_eq!(
            classified_delta_decision(
                SyncDeltaKind::GraphBatch,
                &graph("metadata-shared", "future-label"),
                metadata,
                public,
            ),
            ClassifiedDeltaDecision::Malformed
        );
    }

    #[test]
    fn tombstone_subjects_are_explicit_bounded_and_match_the_delta_subject() {
        let payload = serde_json::json!({
            "subject_kind": "session",
            "subject_key": "sess_1",
            "reason": "deleted",
        });
        assert_eq!(
            parse_tombstone_subject(&payload, "sess_1"),
            Some(("session", "sess_1"))
        );

        for malformed in [
            serde_json::json!({ "subject_key": "sess_1" }),
            serde_json::json!({ "subject_kind": "session" }),
            serde_json::json!({ "subject_kind": "session", "subject_key": "other" }),
            serde_json::json!({ "subject_kind": "\n", "subject_key": "sess_1" }),
            serde_json::json!({
                "subject_kind": "x".repeat(MAX_TOMBSTONE_SUBJECT_KIND_BYTES + 1),
                "subject_key": "sess_1",
            }),
        ] {
            assert_eq!(parse_tombstone_subject(&malformed, "sess_1"), None);
        }
    }

    #[test]
    fn metadata_stream_payloads_keep_only_useful_bounded_fields() {
        let run = stream_payload_for_class(
            SyncDeltaKind::RunSummary,
            &serde_json::json!({
                "run_id": "run_1",
                "session_id": "sess_1",
                "repository_id": "repo_1",
                "state": "completed",
                "prompt_tokens": 12,
                "sync_revision": 7,
                "secret": { "nested": "must not escape" },
            }),
            PublicationClass::MetadataShared,
        );
        assert_eq!(run["run_id"], "run_1");
        assert_eq!(run["prompt_tokens"], 12);
        assert_eq!(run["sync_revision"], 7);
        assert!(run.get("secret").is_none());

        let graph = stream_payload_for_class(
            SyncDeltaKind::GraphBatch,
            &serde_json::json!({
                "batch_id": "batch_1",
                "repository_id": "repo_1",
                "facts": [{
                    "subject_kind": "node",
                    "subject_id": "node_1",
                    "class": "metadata-shared",
                    "classification": "internal",
                    "content_hash": "abc",
                    "source": "private/source.rs",
                }],
            }),
            PublicationClass::MetadataShared,
        );
        assert_eq!(graph["facts"][0]["subject_id"], "node_1");
        assert!(graph["facts"][0].get("source").is_none());
    }

    #[test]
    fn malformed_stored_sync_rows_never_become_known_delta_events() {
        fn stored_event(
            payload: serde_json::Value,
            repository_id: Option<Uuid>,
        ) -> StoredStreamEvent {
            StoredStreamEvent {
                id: 1,
                organization_id: Uuid::now_v7(),
                repository_id,
                stream: "sync".to_string(),
                payload,
                created_at: Utc::now(),
            }
        }

        let repo_id = Uuid::now_v7();
        let valid = serde_json::json!({
            "delta_kind": "session-summary",
            "subject_id": "sess_1",
            "class": "metadata-shared",
            "payload": null,
        });
        assert!(matches!(
            stream_event_to_wire(stored_event(valid.clone(), Some(repo_id)))
                .expect("valid stored event")
                .payload,
            StreamEventPayload::SyncDelta(SyncDeltaEvent {
                payload: serde_json::Value::Null,
                ..
            })
        ));

        let malformed = [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({
                "subject_id": "sess_1",
                "class": "metadata-shared",
                "payload": {},
            }),
            serde_json::json!({
                "delta_kind": 7,
                "subject_id": "sess_1",
                "class": "metadata-shared",
                "payload": {},
            }),
            serde_json::json!({
                "delta_kind": "future-kind",
                "subject_id": "sess_1",
                "class": "metadata-shared",
                "payload": {},
            }),
            serde_json::json!({
                "delta_kind": "session-summary",
                "class": "metadata-shared",
                "payload": {},
            }),
            serde_json::json!({
                "delta_kind": "session-summary",
                "subject_id": 7,
                "class": "metadata-shared",
                "payload": {},
            }),
            serde_json::json!({
                "delta_kind": "session-summary",
                "subject_id": "  ",
                "class": "metadata-shared",
                "payload": {},
            }),
            serde_json::json!({
                "delta_kind": "session-summary",
                "subject_id": "sess_1",
                "payload": {},
            }),
            serde_json::json!({
                "delta_kind": "session-summary",
                "subject_id": "sess_1",
                "class": 7,
                "payload": {},
            }),
            serde_json::json!({
                "delta_kind": "session-summary",
                "subject_id": "sess_1",
                "class": "future-class",
                "payload": {},
            }),
            serde_json::json!({
                "delta_kind": "session-summary",
                "subject_id": "sess_1",
                "class": "metadata-shared",
            }),
            serde_json::json!({
                "delta_kind": "session-summary",
                "subject_id": "a".repeat(MAX_SYNC_SUBJECT_ID_BYTES + 1),
                "class": "metadata-shared",
                "payload": {},
            }),
        ];

        for payload in malformed {
            let projected = stream_event_to_wire(stored_event(payload.clone(), Some(repo_id)))
                .expect("malformed payload still yields a stream envelope");
            assert_eq!(
                projected.payload,
                StreamEventPayload::Unknown,
                "malformed stored sync payload was projected as a known effect: {payload}"
            );
        }

        let missing_scope = stream_event_to_wire(stored_event(valid, None))
            .expect("missing repository still yields a stream envelope");
        assert_eq!(missing_scope.payload, StreamEventPayload::Unknown);
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
            store::{
                memory::MemoryStore, Membership, Organization, Repository, RoleGrant, Store as _,
                User,
            },
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
            .create_user(User {
                id: user_id,
                display_name: "Pairing user".to_string(),
                primary_email: None,
                state: "active".to_string(),
                created_at: now,
                updated_at: now,
            })
            .await
            .expect("user");
        store
            .add_membership(Membership {
                organization_id: org_id,
                user_id,
                state: "active".to_string(),
                joined_at: Some(now),
                created_at: now,
            })
            .await
            .expect("membership");
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
            credential_purpose: "sync".to_string(),
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
