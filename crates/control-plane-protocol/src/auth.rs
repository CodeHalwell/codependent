//! Authentication flows, token exchange, and refresh contracts.

use serde::{Deserialize, Serialize};

use crate::identity::IdentityProvider;
use crate::user::User;

/// Request to initiate an OAuth 2.0 / OIDC login or link flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct OAuthInitRequest {
    pub provider: IdentityProvider,
    pub redirect_uri: String,
    pub state: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
}

/// Response returned with the external authorization URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct OAuthInitResponse {
    pub authorization_url: String,
    pub state: String,
}

/// Request to complete OAuth callback and exchange authorization code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct OAuthCallbackRequest {
    pub code: String,
    pub state: String,
    pub code_verifier: String,
}

/// Token response returned upon successful authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AuthTokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
}

/// Request to rotate a refresh token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RefreshTokenRequest {
    pub refresh_token: String,
}

/// Request to revoke an active token or session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RevokeTokenRequest {
    pub token: String,
}
