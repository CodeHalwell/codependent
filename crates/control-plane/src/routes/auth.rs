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
    authz::{authorize_organization_action, Action},
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
    let user_id = match &principal {
        Principal::User { id, .. } => *id,
        _ => {
            return Err(ControlPlaneError::Forbidden {
                resource: "pairing".to_string(),
                message: "only users can start pairing challenges".to_string(),
            })
        }
    };

    // `organization_id` is attacker-controlled request input, and nothing
    // downstream re-checks it: `complete_pairing` writes a `daemons` row into
    // `challenge.organization_id` verbatim. Authenticated was being treated as
    // authorized, so any user could mint a pairing code naming any tenant and
    // plant a daemon row in it.
    //
    // `Action::Read` is the bar because a daemon's authority is re-derived from
    // its pairing user on every request (`daemon_effective_role`) — it can never
    // exceed what this caller already holds — so membership, not a write role,
    // is what pairing requires. `authorize_organization_action` answers
    // not-found for a non-member, so an organization the caller may not touch is
    // indistinguishable from one that does not exist.
    authorize_organization_action(
        state.store.as_ref(),
        &principal,
        req.organization_id,
        Action::Read,
    )
    .await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::{
        config::ControlPlaneConfig,
        storage::MemoryStorageDriver,
        store::{memory::MemoryStore, Organization, RoleGrant, Store as _},
    };
    use axum::extract::State;

    fn state_with(store: Arc<MemoryStore>) -> AppState {
        let config = ControlPlaneConfig::from_env_with_jwt_secret(
            "ctrl-plane-unit-test-signing-key-0123456789abcdef",
        )
        .expect("test signing secret");
        AppState::new(
            config,
            store as Arc<dyn crate::store::Store + Send + Sync>,
            Arc::new(MemoryStorageDriver::new()),
        )
    }

    fn user(id: Uuid) -> AuthPrincipal {
        AuthPrincipal(Principal::User {
            id,
            email: Some("mallory@example.com".to_string()),
            display_name: "Mallory".to_string(),
        })
    }

    async fn organization(store: &MemoryStore) -> Uuid {
        let org_id = Uuid::now_v7();
        store
            .create_organization(Organization {
                id: org_id,
                slug: "acme".to_string(),
                display_name: "Acme".to_string(),
                max_publication_class: "content-shared".to_string(),
                max_classification: "internal".to_string(),
                data_residency: None,
                retention_days: None,
                policy_version: 1,
                created_at: Utc::now(),
            })
            .await
            .expect("organization");
        org_id
    }

    /// `organization_id` is request input and nothing downstream re-checks it —
    /// `complete_pairing` copies `challenge.organization_id` straight onto the
    /// `daemons` row. Any authenticated user could therefore mint a pairing code
    /// naming a tenant they have no grant in and plant a daemon there.
    #[tokio::test]
    async fn a_user_cannot_start_a_pairing_challenge_in_an_organization_they_are_not_in() {
        let store = Arc::new(MemoryStore::new());
        let org_id = organization(&store).await;
        let state = state_with(store.clone());

        let error = start_pairing_challenge(
            State(state),
            user(Uuid::now_v7()),
            Json(StartPairingChallengeRequest {
                organization_id: org_id,
                requested_scope: serde_json::json!({ "sync": true }),
            }),
        )
        .await
        .expect_err("a non-member must be refused");

        // Not-found, not forbidden: an organization this caller may not touch
        // must be indistinguishable from one that does not exist.
        assert!(
            matches!(error, ControlPlaneError::NotFound { .. }),
            "refusal must not confirm the organization exists: {error:?}"
        );
        assert!(
            store
                .consume_pairing_challenge(&hash_token("anything"), Uuid::now_v7())
                .await
                .expect("store")
                .is_none(),
            "no challenge may have been recorded"
        );
    }

    /// The refusal above must be the same answer for an organization that does
    /// not exist at all, or the endpoint is an existence oracle for tenants.
    #[tokio::test]
    async fn a_missing_organization_and_an_unauthorized_one_refuse_identically() {
        let store = Arc::new(MemoryStore::new());
        let org_id = organization(&store).await;
        let state = state_with(store.clone());
        let caller = Uuid::now_v7();

        let unauthorized = start_pairing_challenge(
            State(state.clone()),
            user(caller),
            Json(StartPairingChallengeRequest {
                organization_id: org_id,
                requested_scope: serde_json::json!({}),
            }),
        )
        .await
        .expect_err("refused");
        let absent = start_pairing_challenge(
            State(state),
            user(caller),
            Json(StartPairingChallengeRequest {
                organization_id: Uuid::now_v7(),
                requested_scope: serde_json::json!({}),
            }),
        )
        .await
        .expect_err("refused");

        assert_eq!(format!("{unauthorized:?}"), format!("{absent:?}"));
    }

    /// Membership — not a write role — is the bar, because a daemon's authority
    /// is re-derived from its pairing user on every request and so can never
    /// exceed theirs. A reader pairing a pull-only daemon is legitimate.
    #[tokio::test]
    async fn a_member_can_still_start_a_pairing_challenge() {
        let store = Arc::new(MemoryStore::new());
        let org_id = organization(&store).await;
        let user_id = Uuid::now_v7();
        store
            .create_role_grant(RoleGrant {
                id: Uuid::now_v7(),
                organization_id: org_id,
                user_id: Some(user_id),
                team_id: None,
                repository_id: None,
                // The lowest role there is: read-only.
                role: "observer".to_string(),
                action_scope: None,
                granted_by: user_id,
                granted_at: Utc::now(),
                expires_at: None,
                revoked_at: None,
            })
            .await
            .expect("grant");
        let state = state_with(store);

        let response = start_pairing_challenge(
            State(state),
            user(user_id),
            Json(StartPairingChallengeRequest {
                organization_id: org_id,
                requested_scope: serde_json::json!({ "sync": true }),
            }),
        )
        .await
        .expect("a member is allowed to pair");
        assert!(response.0.pairing_code.starts_with("cp_pair_"));
    }
}
