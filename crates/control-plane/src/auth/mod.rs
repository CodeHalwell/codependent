use async_trait::async_trait;
use axum::{
    extract::{FromRef, FromRequestParts},
    http::{header, request::Parts},
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{error::ControlPlaneError, state::AppState, store::Daemon};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Principal {
    User {
        id: Uuid,
        email: Option<String>,
        display_name: String,
    },
    Daemon {
        daemon_id: Uuid,
        organization_id: Uuid,
        paired_by: Uuid,
        max_publication_class: String,
    },
}

impl Principal {
    pub fn id(&self) -> Uuid {
        match self {
            Principal::User { id, .. } => *id,
            Principal::Daemon { daemon_id, .. } => *daemon_id,
        }
    }

    pub fn kind_str(&self) -> &'static str {
        match self {
            Principal::User { .. } => "user",
            Principal::Daemon { .. } => "daemon",
        }
    }

    pub fn organization_id(&self) -> Option<Uuid> {
        match self {
            Principal::User { .. } => None,
            Principal::Daemon {
                organization_id, ..
            } => Some(*organization_id),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub iss: String,
    pub aud: String,
    pub exp: i64,
    pub iat: i64,
    pub principal_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_publication_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paired_by: Option<Uuid>,
}

pub fn create_jwt(claims: &Claims, secret: &str) -> Result<String, ControlPlaneError> {
    let header_json = serde_json::json!({
        "alg": "HS256",
        "typ": "JWT"
    });
    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header_json).map_err(|e| {
        ControlPlaneError::Internal(format!("Failed to serialize JWT header: {e}"))
    })?);

    let claims_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).map_err(|e| {
        ControlPlaneError::Internal(format!("Failed to serialize JWT claims: {e}"))
    })?);

    let message = format!("{}.{}", header_b64, claims_b64);
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|e| ControlPlaneError::Internal(format!("HMAC key error: {e}")))?;
    mac.update(message.as_bytes());
    let sig_bytes = mac.finalize().into_bytes();
    let sig_b64 = URL_SAFE_NO_PAD.encode(sig_bytes);

    Ok(format!("{}.{}", message, sig_b64))
}

pub fn verify_jwt(token: &str, secret: &str) -> Result<Claims, ControlPlaneError> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(ControlPlaneError::Unauthorized(
            "malformed JWT token".to_string(),
        ));
    }

    let message = format!("{}.{}", parts[0], parts[1]);
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|e| ControlPlaneError::Internal(format!("HMAC key error: {e}")))?;
    mac.update(message.as_bytes());

    let sig_bytes = URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|_| ControlPlaneError::Unauthorized("invalid JWT signature base64".to_string()))?;

    mac.verify_slice(&sig_bytes).map_err(|_| {
        ControlPlaneError::Unauthorized("JWT signature verification failed".to_string())
    })?;

    let claims_json = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| ControlPlaneError::Unauthorized("invalid JWT claims base64".to_string()))?;

    let claims: Claims = serde_json::from_slice(&claims_json)
        .map_err(|_| ControlPlaneError::Unauthorized("invalid JWT payload".to_string()))?;

    let now = Utc::now().timestamp();
    if claims.exp < now {
        return Err(ControlPlaneError::Unauthorized(
            "JWT token expired".to_string(),
        ));
    }

    Ok(claims)
}

pub fn hash_token(token: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hasher.finalize().to_vec()
}

pub fn create_user_token(
    user_id: Uuid,
    email: Option<String>,
    display_name: String,
    secret: &str,
    duration_secs: i64,
) -> Result<String, ControlPlaneError> {
    let now = Utc::now().timestamp();
    let claims = Claims {
        sub: user_id.to_string(),
        iss: "codypendent-control-plane".to_string(),
        aud: "control-plane".to_string(),
        exp: now + duration_secs,
        iat: now,
        principal_kind: "user".to_string(),
        org_id: None,
        email,
        display_name: Some(display_name),
        max_publication_class: None,
        paired_by: None,
    };
    create_jwt(&claims, secret)
}

pub fn create_daemon_token(
    daemon_id: Uuid,
    org_id: Uuid,
    paired_by: Uuid,
    max_publication_class: String,
    secret: &str,
    duration_secs: i64,
) -> Result<String, ControlPlaneError> {
    let now = Utc::now().timestamp();
    let claims = Claims {
        sub: daemon_id.to_string(),
        iss: "codypendent-control-plane".to_string(),
        aud: "control-plane".to_string(),
        exp: now + duration_secs,
        iat: now,
        principal_kind: "daemon".to_string(),
        org_id: Some(org_id),
        email: None,
        display_name: None,
        max_publication_class: Some(max_publication_class),
        paired_by: Some(paired_by),
    };
    create_jwt(&claims, secret)
}

pub struct AuthPrincipal(pub Principal);

#[async_trait]
impl<S> FromRequestParts<S> for AuthPrincipal
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ControlPlaneError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);

        let auth_header = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .ok_or_else(|| {
                ControlPlaneError::Unauthorized("missing authorization header".to_string())
            })?;

        let token = auth_header.strip_prefix("Bearer ").ok_or_else(|| {
            ControlPlaneError::Unauthorized(
                "authorization header must use Bearer scheme".to_string(),
            )
        })?;

        // First attempt JWT verification
        if let Ok(claims) = verify_jwt(token, &app_state.config.jwt_secret) {
            let principal = match claims.principal_kind.as_str() {
                "user" => {
                    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| {
                        ControlPlaneError::Unauthorized("invalid user id in JWT".to_string())
                    })?;
                    Principal::User {
                        id: user_id,
                        email: claims.email,
                        display_name: claims.display_name.unwrap_or_else(|| "User".to_string()),
                    }
                }
                "daemon" => {
                    let daemon_id = Uuid::parse_str(&claims.sub).map_err(|_| invalid_token())?;

                    // A JWT only proves the control plane minted *something* for
                    // this daemon id at some point. Every authority-bearing field
                    // (organization, pairing user, publication ceiling) is read
                    // from the daemons row on each request so revocation,
                    // suspension and re-scoping take effect immediately instead of
                    // at token expiry. Claims are never trusted for authority.
                    let daemon = app_state
                        .store
                        .get_daemon(daemon_id)
                        .await?
                        .ok_or_else(invalid_token)?;

                    if !daemon_is_usable(&daemon) {
                        return Err(invalid_token());
                    }

                    // A token minted for one organization must not be replayed
                    // against a daemon row that now belongs to another. Absent
                    // org_id is also refused: fail closed on ambiguity.
                    if claims.org_id != Some(daemon.organization_id) {
                        return Err(invalid_token());
                    }

                    Principal::Daemon {
                        daemon_id: daemon.id,
                        organization_id: daemon.organization_id,
                        paired_by: daemon.paired_by,
                        max_publication_class: daemon.max_publication_class,
                    }
                }
                _ => {
                    return Err(ControlPlaneError::Unauthorized(
                        "unknown principal kind".to_string(),
                    ))
                }
            };
            return Ok(AuthPrincipal(principal));
        }

        // Second attempt: Workload token / API key lookup by hash in store
        let token_hash = hash_token(token);
        if let Some(workload) = app_state
            .store
            .lookup_workload_credential(&token_hash)
            .await?
        {
            // Re-check the credential here rather than relying on the lookup
            // query to have filtered it: authentication must not depend on a
            // predicate living in another layer.
            let now = Utc::now();
            if workload.revoked_at.is_some() || workload.expires_at <= now {
                return Err(invalid_token());
            }
            if let Some(daemon) = app_state.store.get_daemon(workload.daemon_id).await? {
                if daemon_is_usable(&daemon) {
                    return Ok(AuthPrincipal(Principal::Daemon {
                        daemon_id: daemon.id,
                        organization_id: daemon.organization_id,
                        paired_by: daemon.paired_by,
                        max_publication_class: daemon.max_publication_class,
                    }));
                }
            }
        }

        Err(invalid_token())
    }
}

/// A daemon may only act while its row says it is active and unrevoked.
pub(crate) fn daemon_is_usable(daemon: &Daemon) -> bool {
    daemon.state == "active" && daemon.revoked_at.is_none()
}

/// One uniform rejection for every authentication failure so that a revoked
/// daemon, an unknown daemon, a daemon in another tenant and a garbage token
/// are indistinguishable to the caller.
fn invalid_token() -> ControlPlaneError {
    ControlPlaneError::Unauthorized("invalid or expired token".to_string())
}
