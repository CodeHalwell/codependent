//! User models, states, and profile projections.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::UserId;

/// Lifecycle state of a user account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum UserState {
    #[default]
    Active,
    Suspended,
    Deleted,
    /// Unrecognized or newer state. Never treated as active.
    #[serde(other)]
    Unknown,
}

impl UserState {
    /// Whether the account may act. Only the explicit `Active` state qualifies.
    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// User entity in the control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct User {
    pub id: UserId,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_email: Option<String>,
    pub state: UserState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request to update a user's details.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct UpdateUserRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_email: Option<String>,
}

/// Compact user summary for team listings and mentions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct UserSummary {
    pub id: UserId,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_email: Option<String>,
    pub state: UserState,
}
