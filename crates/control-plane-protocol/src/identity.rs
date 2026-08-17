//! External identities and identity linking contracts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AuditRecordId, IdentityId, UserId};

/// Supported external authentication identity providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum IdentityProvider {
    Github,
    Oidc,
    /// Unrecognized or newer provider. No login or link flow may be started for it.
    #[serde(other)]
    Unknown,
}

/// External identity linked to a control-plane user account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct UserIdentity {
    pub id: IdentityId,
    pub user_id: UserId,
    pub provider: IdentityProvider,
    pub issuer: String,
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_at_link: Option<String>,
    pub linked_at: DateTime<Utc>,
    pub link_audit_id: AuditRecordId,
}

/// Request to link a new external identity to the current authenticated account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct IdentityLinkRequest {
    pub provider: IdentityProvider,
    pub issuer: String,
    pub auth_code: String,
    pub redirect_uri: String,
    pub code_verifier: String,
}

/// Result of an identity linking operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct IdentityLinkResult {
    pub identity_id: IdentityId,
    pub user_id: UserId,
    pub provider: IdentityProvider,
    pub linked_at: DateTime<Utc>,
}
