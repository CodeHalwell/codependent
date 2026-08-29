use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

use codypendent_control_plane_protocol::{
    auth::{AuthTokenResponse, RefreshTokenRequest},
    daemon::{InitiatePairingRequest, InitiatePairingResponse},
    ids::FederatedRepositoryId,
};

use crate::{
    auth::{create_user_token, hash_token, random_opaque_token, AuthPrincipal, Principal},
    authz::{authorize_organization_action, parse_publication_class, Action},
    error::{ControlPlaneError, ErrorResponse},
    state::AppState,
    store::{PairingChallenge, PairingCompletion, RefreshRotation, RefreshRotationOutcome},
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

pub async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshTokenRequest>,
) -> Result<Json<AuthTokenResponse>, ControlPlaneError> {
    let token_hash = hash_token(&req.refresh_token);
    let now = Utc::now();
    let new_raw_refresh = random_opaque_token("cprt_")?;
    let new_token_hash = hash_token(&new_raw_refresh);

    // Resolve the old token, detect replay, revoke it, and insert its one
    // replacement in a single store transaction/critical section.
    let outcome = state
        .store
        .rotate_refresh_token(RefreshRotation {
            old_token_hash: token_hash,
            new_id: Uuid::now_v7(),
            new_token_hash,
            issued_at: now,
            expires_at: now + chrono::Duration::days(30),
            user_agent_digest: None,
        })
        .await?;
    let user = match outcome {
        RefreshRotationOutcome::Rotated(user) => user,
        RefreshRotationOutcome::ReuseDetected => {
            return Err(ControlPlaneError::Unauthorized(
                "refresh token reuse detected; chain revoked".to_string(),
            ));
        }
        RefreshRotationOutcome::Expired => {
            return Err(ControlPlaneError::Unauthorized(
                "refresh token expired".to_string(),
            ));
        }
        RefreshRotationOutcome::InactiveUser => {
            return Err(ControlPlaneError::Unauthorized(
                "user account is not active".to_string(),
            ));
        }
        RefreshRotationOutcome::Invalid => {
            return Err(ControlPlaneError::Unauthorized(
                "invalid refresh token".to_string(),
            ));
        }
    };

    let access_token = create_user_token(
        user.id,
        user.primary_email,
        user.display_name,
        &state.config.jwt_secret,
        3600,
    )?;

    Ok(Json(AuthTokenResponse {
        access_token,
        token_type: "Bearer".to_string(),
        expires_in: 3600,
        refresh_token: Some(new_raw_refresh),
        user: None,
    }))
}

pub async fn start_pairing_challenge(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Json(req): Json<InitiatePairingRequest>,
) -> Result<Json<InitiatePairingResponse>, ControlPlaneError> {
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
        req.organization_id.as_uuid(),
        Action::Read,
    )
    .await?;

    let organization = state
        .store
        .get_organization(req.organization_id.as_uuid())
        .await?
        .ok_or_else(|| ControlPlaneError::not_found("organization", "no such organization"))?;
    let organization_ceiling = parse_publication_class(&organization.max_publication_class);
    if !req
        .requested_scope
        .max_publication_class
        .permits_in_ceiling(organization_ceiling)
    {
        return Err(ControlPlaneError::BadRequest(
            "pairing scope exceeds the organization publication ceiling".to_string(),
        ));
    }
    if req.requested_scope.repositories.len() > 256 {
        return Err(ControlPlaneError::BadRequest(
            "pairing scope names more than 256 repositories".to_string(),
        ));
    }
    let mut repositories = HashSet::with_capacity(req.requested_scope.repositories.len());
    for repository in &req.requested_scope.repositories {
        FederatedRepositoryId::new(repository.as_str()).map_err(|_| {
            ControlPlaneError::BadRequest(
                "pairing scope contains an invalid federated repository id".to_string(),
            )
        })?;
        if !repositories.insert(repository.as_str()) {
            return Err(ControlPlaneError::BadRequest(
                "pairing scope contains a duplicate repository".to_string(),
            ));
        }
    }

    let pairing_code = random_opaque_token("cp_pair_")?;
    let code_hash = hash_token(&pairing_code);
    let now = Utc::now();
    let expires_at = now + chrono::Duration::minutes(15);

    let challenge = PairingChallenge {
        code_hash,
        organization_id: req.organization_id.as_uuid(),
        initiated_by: user_id,
        requested_scope: serde_json::to_value(&req.requested_scope).map_err(|e| {
            ControlPlaneError::Internal(format!("failed to serialize pairing scope: {e}"))
        })?,
        created_at: now,
        expires_at,
        consumed_at: None,
        daemon_id: None,
    };

    state.store.create_pairing_challenge(challenge).await?;

    Ok(Json(InitiatePairingResponse {
        verification_uri: format!("/pair?code={pairing_code}"),
        challenge_code: pairing_code,
        expires_at,
        poll_interval_seconds: 5,
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
    let now = Utc::now();
    let consent_manifest_hash = hash_token(&req.consent_manifest);
    let raw_token = random_opaque_token("cp_daemon_")?;
    let token_hash = hash_token(&raw_token);
    let challenge = state
        .store
        .complete_pairing(
            &code_hash,
            PairingCompletion {
                daemon_id,
                display_name: req.display_name,
                consent_manifest_hash,
                max_publication_class: req.max_publication_class,
                accepts_remote_approvals: req.accepts_remote_approvals.unwrap_or(false),
                accepts_runner_dispatch: req.accepts_runner_dispatch.unwrap_or(false),
                credential_id: Uuid::now_v7(),
                credential_audience: "control-plane".to_string(),
                credential_purpose: "sync".to_string(),
                credential_token_hash: token_hash,
                completed_at: now,
                credential_expires_at: now + chrono::Duration::days(365),
            },
        )
        .await?
        .ok_or_else(|| {
            ControlPlaneError::Unauthorized("invalid or expired pairing code".to_string())
        })?;

    Ok(Json(CompletePairingResponse {
        daemon_id,
        organization_id: challenge.organization_id,
        token: raw_token,
    }))
}

/// `POST /v1/auth/link`
///
/// Knowing an external `(provider, issuer, subject)` tuple is not proof of
/// controlling it. Until the authorization-code flow authenticates the second
/// identity (with PKCE, state and nonce) this endpoint must not persist a link:
/// doing so lets any logged-in user pre-claim another person's future login.
pub async fn link_identity(AuthPrincipal(_principal): AuthPrincipal) -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(ErrorResponse {
            r#type: "not_implemented".to_string(),
            resource: Some("identity_provider".to_string()),
            message: "identity linking requires a verified provider flow and is unavailable"
                .to_string(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::{
        config::ControlPlaneConfig,
        storage::MemoryStorageDriver,
        store::{memory::MemoryStore, Membership, Organization, RoleGrant, Store as _, User},
    };
    use axum::extract::State;
    use codypendent_control_plane_protocol::{
        daemon::PairingScope, ids::OrganizationId, publication::PublicationClass,
    };

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

    fn initiate(organization_id: Uuid) -> InitiatePairingRequest {
        InitiatePairingRequest {
            organization_id: OrganizationId::from_uuid(organization_id),
            requested_scope: PairingScope {
                max_publication_class: PublicationClass::MetadataShared,
                accepts_remote_approvals: false,
                accepts_runner_dispatch: false,
                repositories: Vec::new(),
            },
        }
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

        let error =
            start_pairing_challenge(State(state), user(Uuid::now_v7()), Json(initiate(org_id)))
                .await
                .expect_err("a non-member must be refused");

        // Not-found, not forbidden: an organization this caller may not touch
        // must be indistinguishable from one that does not exist.
        assert!(
            matches!(error, ControlPlaneError::NotFound { .. }),
            "refusal must not confirm the organization exists: {error:?}"
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

        let unauthorized =
            start_pairing_challenge(State(state.clone()), user(caller), Json(initiate(org_id)))
                .await
                .expect_err("refused");
        let absent =
            start_pairing_challenge(State(state), user(caller), Json(initiate(Uuid::now_v7())))
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
        let now = Utc::now();
        store
            .create_user(User {
                id: user_id,
                display_name: "Member".to_string(),
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
                // The lowest role there is: read-only.
                role: "observer".to_string(),
                action_scope: None,
                granted_by: user_id,
                granted_at: now,
                expires_at: None,
                revoked_at: None,
            })
            .await
            .expect("grant");
        let state = state_with(store);

        let response = start_pairing_challenge(State(state), user(user_id), Json(initiate(org_id)))
            .await
            .expect("a member is allowed to pair");
        assert!(response.0.challenge_code.starts_with("cp_pair_"));
    }
}
