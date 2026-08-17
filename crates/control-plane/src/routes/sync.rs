use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth::{AuthPrincipal, Principal},
    authz::{authorize_organization_action, authorize_repository_action, Action, PublicationClass},
    error::ControlPlaneError,
    state::{AppState, StreamEventMessage},
    store::{SharedSession, StreamEvent, SyncReceipt, Tombstone},
};

#[derive(Debug, Deserialize)]
pub struct SyncDeltaPushRequest {
    pub daemon_sequence: i64,
    pub delta_kind: String, // 'session-summary' | 'inbox-entry' | 'tombstone' | ...
    pub repository_id: Option<Uuid>,
    pub subject_id: String,
    pub class: String,
    pub payload: serde_json::Value,
    pub payload_hash: String,
}

#[derive(Debug, Serialize)]
pub struct SyncDeltaPushResponse {
    pub receipt_id: Uuid,
    pub daemon_sequence: i64,
    pub accepted_at: chrono::DateTime<Utc>,
    pub duplicate: bool,
}

pub async fn push_sync_delta(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Json(req): Json<SyncDeltaPushRequest>,
) -> Result<Json<SyncDeltaPushResponse>, ControlPlaneError> {
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

    // `repository_id` is attacker-controlled request input. Every write this
    // handler performs is attributed to a repository, so the repository must be
    // named and the principal must be authorized on it before anything is
    // recorded. An absent repository_id is refused rather than defaulted.
    let repo_id = req
        .repository_id
        .ok_or_else(|| ControlPlaneError::BadRequest("repository_id is required".to_string()))?;

    // Resolves the repository within the principal's organization and checks the
    // grant. A repository in another tenant is indistinguishable from one that
    // does not exist (both 404).
    authorize_repository_action(
        state.store.as_ref(),
        &principal,
        org_id,
        repo_id,
        Action::SyncPush,
    )
    .await?;

    // Publication ceiling is a three-way intersection: organization ∩ repository
    // ∩ daemon, then clamped by what the daemon actually requested (Design §4,
    // §8.3, §12.3). The daemon ceiling alone is not the ceiling — a permissive
    // daemon must never widen its organization's or repository's policy.
    let org = state
        .store
        .get_organization(org_id)
        .await?
        .ok_or_else(|| ControlPlaneError::not_found("organization", "no such organization"))?;

    // Tenant-scoped in the query: the organization is in the WHERE clause, so a
    // repository belonging to another tenant is never fetched and then judged.
    let repo = state
        .store
        .get_repository_in_org(org_id, repo_id)
        .await?
        .ok_or_else(|| ControlPlaneError::not_found("repository", "no such repository"))?;

    let requested_class = PublicationClass::from_str(&req.class);
    let effective_class = PublicationClass::from_str(&org.max_publication_class)
        .intersect(PublicationClass::from_str(&repo.max_publication_class))
        .intersect(PublicationClass::from_str(&daemon_max_class))
        .intersect(requested_class);

    // private-local means "never leaves the machine". Publishing it to the
    // control plane would contradict the class, and an unrecognised class parses
    // to private-local, so refuse instead of persisting. The refusal is shaped
    // like every other repository refusal so it discloses nothing.
    if effective_class <= PublicationClass::PrivateLocal {
        return Err(ControlPlaneError::forbidden(
            "repository",
            "no such repository",
        ));
    }

    let payload_hash_bytes = hex::decode(&req.payload_hash)
        .map_err(|_| ControlPlaneError::BadRequest("invalid hex in payload_hash".to_string()))?;

    let now = Utc::now();
    let receipt_id = Uuid::now_v7();

    let receipt = SyncReceipt {
        id: receipt_id,
        daemon_id,
        daemon_sequence: req.daemon_sequence,
        delta_kind: req.delta_kind.clone(),
        payload_hash: payload_hash_bytes,
        class: effective_class.as_str().to_string(),
        accepted_at: now,
    };

    let is_new = state.store.record_sync_receipt(receipt).await?;

    if !is_new {
        // Idempotent duplicate delivery
        return Ok(Json(SyncDeltaPushResponse {
            receipt_id,
            daemon_sequence: req.daemon_sequence,
            accepted_at: now,
            duplicate: true,
        }));
    }

    // Apply projection based on delta_kind
    match req.delta_kind.as_str() {
        "session-summary" => {
            let title = if effective_class >= PublicationClass::ContentShared {
                req.payload
                    .get("title")
                    .and_then(|t| t.as_str())
                    .map(ToString::to_string)
            } else {
                None // Redacted for metadata-only per §3.2
            };

            let session_state = req
                .payload
                .get("state")
                .and_then(|s| s.as_str())
                .unwrap_or("active")
                .to_string();

            let session = SharedSession {
                id: Uuid::now_v7(),
                organization_id: org_id,
                repository_id: repo_id,
                daemon_id,
                remote_session_key: req.subject_id.clone(),
                class: effective_class.as_str().to_string(),
                title,
                state: session_state,
                started_at: now,
                last_activity_at: Some(now),
                tombstoned_at: None,
                updated_at: now,
            };
            state.store.upsert_shared_session(session).await?;
        }
        "tombstone" => {
            let reason = req
                .payload
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("deleted")
                .to_string();

            let tombstone = Tombstone {
                id: Uuid::now_v7(),
                organization_id: org_id,
                subject_kind: req.delta_kind.clone(),
                subject_key: req.subject_id.clone(),
                reason,
                created_at: now,
                applied_at: Some(now),
            };
            state.store.create_tombstone(tombstone).await?;
        }
        _ => {}
    }

    // Append to stream_events and broadcast
    // Always stamped with the authorized repository so delivery can be scoped to
    // it. An event with no repository_id is undeliverable without leaking it to
    // every subscriber in the organization.
    let stream_event = StreamEvent {
        id: 0,
        organization_id: org_id,
        repository_id: Some(repo_id),
        stream: "sync".to_string(),
        payload: serde_json::json!({
            "delta_kind": req.delta_kind,
            "subject_id": req.subject_id,
            "class": effective_class.as_str(),
            "payload": req.payload,
        }),
        created_at: now,
    };

    let appended = state.store.append_stream_event(stream_event).await?;
    let _ = state.events_tx.send(StreamEventMessage {
        id: appended.id,
        organization_id: org_id,
        repository_id: Some(repo_id),
        stream: appended.stream,
        payload: appended.payload,
    });

    Ok(Json(SyncDeltaPushResponse {
        receipt_id,
        daemon_sequence: req.daemon_sequence,
        accepted_at: now,
        duplicate: false,
    }))
}

#[derive(Debug, Deserialize)]
pub struct PullQuery {
    pub repository_id: Option<Uuid>,
    pub stream: Option<String>,
    pub after_id: Option<i64>,
    pub limit: Option<usize>,
}

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
) -> Result<Json<Vec<SharedSession>>, ControlPlaneError> {
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

    Ok(Json(sessions))
}
