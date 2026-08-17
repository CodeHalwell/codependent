//! Registered repository models, requests, and summaries.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{FederatedRepositoryId, OrganizationId, RepositoryId};
use crate::publication::{DataClassification, PublicationClass};

/// Repository entity registered with an organization in the control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct Repository {
    pub id: RepositoryId,
    pub organization_id: OrganizationId,
    /// Cross-machine federated identity (SHA-256 hex).
    pub federated_id: FederatedRepositoryId,
    pub display_name: String,
    pub max_publication_class: PublicationClass,
    pub max_classification: DataClassification,
    pub policy_version: u64,
    pub created_at: DateTime<Utc>,
}

/// Request to register a repository in an organization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RegisterRepositoryRequest {
    pub federated_id: FederatedRepositoryId,
    pub display_name: String,
    #[serde(default)]
    pub max_publication_class: Option<PublicationClass>,
    #[serde(default)]
    pub max_classification: Option<DataClassification>,
}

/// Request to update repository settings.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct UpdateRepositoryRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_publication_class: Option<PublicationClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_classification: Option<DataClassification>,
}

/// Compact repository summary for listings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RepositorySummary {
    pub id: RepositoryId,
    pub organization_id: OrganizationId,
    pub federated_id: FederatedRepositoryId,
    pub display_name: String,
    pub max_publication_class: PublicationClass,
    pub max_classification: DataClassification,
    pub shared_session_count: u64,
    pub published_object_count: u64,
    pub created_at: DateTime<Utc>,
}
