use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth::{create_user_token, hash_token, AuthPrincipal, Principal},
    error::{identity_link_refused, ControlPlaneError, ErrorResponse},
    state::AppState,
    store::{Daemon, PairingChallenge, UserIdentity, UserRefreshToken, WorkloadCredential},
};

/// `POST /v1/auth/login`
///
/// Interactive login requires a configured identity provider and a full
/// authorization-code exchange (provider redirect, PKCE verifier, `state` and
/// `nonce` persisted in the `auth_flows` table, then identity lookup against
/// `user_identities`). None of that exists yet, and no provider is configured.
///
/// Until it does, this endpoint refuses unconditionally. It deliberately takes
/// no request body and touches no store: there is no code path here that can
/// create a user, mint an access token, or issue a refresh token. Minting
/// authority for an unauthenticated caller would hand a valid session to
/// anyone who can reach the port.
pub async fn login() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(ErrorResponse {
            r#type: "not_implemented".to_string(),
            resource: Some("identity_provider".to_string()),
            message: "no identity provider is configured; interactive login is unavailable"
                .to_string(),
        }),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Debug, Serialize)]
pub struct RefreshResponse {
    pub access_token: String,
    pub refresh_token: String,
}

pub async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshRequest>,
) -> Result<Json<RefreshResponse>, ControlPlaneError> {
    let token_hash = hash_token(&req.refresh_token);
    let record = state.store.lookup_refresh_token(&token_hash).await?;

    let record = match record {
        Some(r) => r,
        None => {
            return Err(ControlPlaneError::Unauthorized(
                "invalid refresh token".to_string(),
            ))
        }
    };

    let now = Utc::now();

    // Design §5.1: If token has been revoked or expired, check for replay theft
    if record.revoked_at.is_some() {
        // Token reuse detected! Revoke the entire refresh chain
        state.store.revoke_refresh_token_chain(&token_hash).await?;
        return Err(ControlPlaneError::Unauthorized(
            "refresh token reuse detected; chain revoked".to_string(),
        ));
    }

    if record.expires_at <= now {
        return Err(ControlPlaneError::Unauthorized(
            "refresh token expired".to_string(),
        ));
    }

    // Revoke old refresh token
    state.store.revoke_refresh_token(record.id).await?;

    // Load user to get latest display name / email
    let user = state
        .store
        .get_user(record.user_id)
        .await?
        .ok_or_else(|| ControlPlaneError::Unauthorized("user not found".to_string()))?;

    // Issue new access token
    let access_token = create_user_token(
        user.id,
        user.primary_email,
        user.display_name,
        &state.config.jwt_secret,
        3600,
    )?;

    // Mint and save new rotated refresh token
    let new_raw_refresh = format!("cprt_{}", Uuid::now_v7());
    let new_token_hash = hash_token(&new_raw_refresh);

    let new_refresh_record = UserRefreshToken {
        id: Uuid::now_v7(),
        user_id: user.id,
        token_hash: new_token_hash,
        rotated_from: Some(record.id),
        issued_at: now,
        expires_at: now + chrono::Duration::days(30),
        revoked_at: None,
        user_agent_digest: None,
    };

    state.store.save_refresh_token(new_refresh_record).await?;

    Ok(Json(RefreshResponse {
        access_token,
        refresh_token: new_raw_refresh,
    }))
}

#[derive(Debug, Deserialize)]
pub struct StartPairingChallengeRequest {
    pub organization_id: Uuid,
    pub requested_scope: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct StartPairingChallengeResponse {
    pub pairing_code: String,
    pub expires_at: chrono::DateTime<Utc>,
}

pub async fn start_pairing_challenge(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Json(req): Json<StartPairingChallengeRequest>,
) -> Result<Json<StartPairingChallengeResponse>, ControlPlaneError> {
    let user_id = match principal {
        Principal::User { id, .. } => id,
        _ => {
            return Err(ControlPlaneError::Forbidden {
                resource: "pairing".to_string(),
                message: "only users can start pairing challenges".to_string(),
            })
        }
    };

    let pairing_code = format!("cp_pair_{}", Uuid::now_v7());
    let code_hash = hash_token(&pairing_code);
    let now = Utc::now();
    let expires_at = now + chrono::Duration::minutes(15);

    let challenge = PairingChallenge {
        code_hash,
        organization_id: req.organization_id,
        initiated_by: user_id,
        requested_scope: req.requested_scope,
        created_at: now,
        expires_at,
        consumed_at: None,
        daemon_id: None,
    };

    state.store.create_pairing_challenge(challenge).await?;

    Ok(Json(StartPairingChallengeResponse {
        pairing_code,
        expires_at,
    }))
}

#[derive(Debug, Deserialize)]
pub struct CompletePairingRequest {
    pub pairing_code: String,
    pub display_name: String,
    pub consent_manifest: String,
    pub max_publication_class: String,
    pub accepts_remote_approvals: Option<bool>,
    pub accepts_runner_dispatch: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct CompletePairingResponse {
    pub daemon_id: Uuid,
    pub organization_id: Uuid,
    pub token: String,
}

pub async fn complete_pairing(
    State(state): State<AppState>,
    Json(req): Json<CompletePairingRequest>,
) -> Result<Json<CompletePairingResponse>, ControlPlaneError> {
    let code_hash = hash_token(&req.pairing_code);
    let daemon_id = Uuid::now_v7();

    let challenge = state
        .store
        .consume_pairing_challenge(&code_hash, daemon_id)
        .await?
        .ok_or_else(|| {
            ControlPlaneError::Unauthorized("invalid or expired pairing code".to_string())
        })?;

    let now = Utc::now();
    let consent_manifest_hash = hash_token(&req.consent_manifest);

    let daemon = Daemon {
        id: daemon_id,
        organization_id: challenge.organization_id,
        paired_by: challenge.initiated_by,
        display_name: req.display_name,
        consent_manifest_hash,
        max_publication_class: req.max_publication_class.clone(),
        accepts_remote_approvals: req.accepts_remote_approvals.unwrap_or(false),
        accepts_runner_dispatch: req.accepts_runner_dispatch.unwrap_or(false),
        state: "active".to_string(),
        paired_at: Some(now),
        revoked_at: None,
        last_seen_at: Some(now),
        created_at: now,
    };

    state.store.register_daemon(daemon).await?;

    // Issue workload token
    let raw_token = format!("cp_daemon_{}", Uuid::now_v7());
    let token_hash = hash_token(&raw_token);

    let cred = WorkloadCredential {
        id: Uuid::now_v7(),
        daemon_id,
        audience: "control-plane".to_string(),
        purpose: "sync".to_string(),
        token_hash,
        rotated_from: None,
        issued_at: now,
        expires_at: now + chrono::Duration::days(365),
        revoked_at: None,
    };

    state.store.save_workload_credential(cred).await?;

    Ok(Json(CompletePairingResponse {
        daemon_id,
        organization_id: challenge.organization_id,
        token: raw_token,
    }))
}

#[derive(Debug, Deserialize)]
pub struct LinkIdentityRequest {
    pub provider: String,
    pub issuer: String,
    pub subject: String,
    pub email_at_link: Option<String>,
}

pub async fn link_identity(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Json(req): Json<LinkIdentityRequest>,
) -> Result<Json<serde_json::Value>, ControlPlaneError> {
    let user_id = match principal {
        Principal::User { id, .. } => id,
        _ => {
            return Err(ControlPlaneError::Forbidden {
                resource: "identity".to_string(),
                message: "only authenticated users can link identities".to_string(),
            })
        }
    };

    // Design §5.3: a caller supplies the (provider, issuer, subject) tuple, so a
    // distinguishable response here is an oracle for "some other user has already
    // linked this identity". Every outcome that is not the caller's own identity
    // collapses to the same refusal.
    let already_linked = state
        .store
        .find_user_identity(&req.provider, &req.issuer, &req.subject)
        .await?;

    if let Some(existing) = already_linked {
        if existing.user_id == user_id {
            // Re-linking the caller's own identity is idempotent.
            return Ok(Json(serde_json::json!({ "status": "linked" })));
        }
        // Same constructor the store uses for a lost race, so the two refusals
        // are byte-identical by construction rather than by coincidence.
        return Err(identity_link_refused());
    }

    let identity = UserIdentity {
        id: Uuid::now_v7(),
        user_id,
        provider: req.provider,
        issuer: req.issuer,
        subject: req.subject,
        email_at_link: req.email_at_link,
        linked_at: Utc::now(),
        link_audit_id: Uuid::now_v7(),
    };

    // Losing the race against a concurrent link is collapsed by the store itself:
    // both `PgStore` and `MemoryStore` return the same "identity cannot be
    // linked" refusal as the check above rather than a unique-violation conflict.
    state.store.create_user_identity(identity).await?;

    Ok(Json(serde_json::json!({ "status": "linked" })))
}
