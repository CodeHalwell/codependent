//! Publication classes, data classification, and policy snapshots.
//!
//! Enforces policy boundaries between local daemons and the control plane.
//! Note: Unknown tags rank strictest to prevent unauthorized data dissemination.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::Sha256Digest;

/// Publication class hierarchy. Defines how far data may be synchronized or shared.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum PublicationClass {
    /// Stays on the local machine only; never sent off-device.
    #[default]
    PrivateLocal,
    /// Bounded operational metadata only (e.g. session counts, run states). No titles/content.
    MetadataShared,
    /// Shared session and artifact titles and content summaries within the organization.
    ContentShared,
    /// Indexed in the shared organization knowledge graph.
    OrganizationKnowledge,
    /// Signed and publishable to the public marketplace.
    PublicMarketplace,
    /// Unknown or newer publication class. Treated as strictly local (strictest rank).
    #[serde(other)]
    Unknown,
}

impl PublicationClass {
    /// Restrictiveness rank: lower number is more restrictive / narrower scope.
    /// `Unknown` returns `u8::MAX` (255) to ensure it is treated with maximum restrictiveness.
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::PrivateLocal => 0,
            Self::MetadataShared => 1,
            Self::ContentShared => 2,
            Self::OrganizationKnowledge => 3,
            Self::PublicMarketplace => 4,
            Self::Unknown => u8::MAX,
        }
    }

    /// Whether this class permits sharing off-device.
    #[must_use]
    pub fn allows_off_device(self) -> bool {
        matches!(
            self,
            Self::MetadataShared
                | Self::ContentShared
                | Self::OrganizationKnowledge
                | Self::PublicMarketplace
        )
    }

    /// Check if this class is permitted under the specified ceiling policy.
    #[must_use]
    pub fn permits_in_ceiling(self, ceiling: PublicationClass) -> bool {
        if self == Self::Unknown || ceiling == Self::Unknown {
            return false;
        }
        self.rank() <= ceiling.rank()
    }

    /// Compute the intersection (most restrictive / narrowest) of two publication classes.
    #[must_use]
    pub fn intersect(self, other: PublicationClass) -> PublicationClass {
        if self == Self::Unknown || other == Self::Unknown {
            return Self::PrivateLocal;
        }
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PrivateLocal => "private-local",
            Self::MetadataShared => "metadata-shared",
            Self::ContentShared => "content-shared",
            Self::OrganizationKnowledge => "organization-knowledge",
            Self::PublicMarketplace => "public-marketplace",
            Self::Unknown => "unknown",
        }
    }
}

/// Data sensitivity classification hierarchy.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DataClassification {
    /// Publicly shareable data.
    Public,
    /// Internal organization data.
    #[default]
    Internal,
    /// Confidential data requiring elevated access.
    Confidential,
    /// Highly sensitive secrets and credentials.
    Secret,
    /// Unrecognized classification. Treated as at least as restrictive as Secret.
    #[serde(other)]
    Unknown,
}

impl DataClassification {
    /// Sensitivity rank: 0 (least sensitive) to 3 (most sensitive), with Unknown as 255.
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::Public => 0,
            Self::Internal => 1,
            Self::Confidential => 2,
            Self::Secret => 3,
            Self::Unknown => u8::MAX,
        }
    }

    /// Check if this classification is permitted under a given maximum sensitivity ceiling.
    #[must_use]
    pub fn permits(self, ceiling: DataClassification) -> bool {
        if self == Self::Unknown || ceiling == Self::Unknown {
            return false;
        }
        self.rank() <= ceiling.rank()
    }

    /// Compute the intersection of two sensitivity ceilings (returns the strictest / lowest ceiling).
    #[must_use]
    pub fn intersect(self, other: DataClassification) -> DataClassification {
        if self == Self::Unknown || other == Self::Unknown {
            return Self::Secret;
        }
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Internal => "internal",
            Self::Confidential => "confidential",
            Self::Secret => "secret",
            Self::Unknown => "unknown",
        }
    }
}

/// Narrowed policy snapshot delivered from control plane to daemon.
/// The local daemon uses this strictly as narrowing inputs (`local.strictest(remote)`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PolicySnapshot {
    pub policy_version: u64,
    pub max_publication_class: PublicationClass,
    pub max_classification: DataClassification,
    pub restrictions: PolicyRestrictions,
    pub received_at: DateTime<Utc>,
    pub payload_hash: Sha256Digest,
}

/// Provider, model, and regional restrictions configured by the organization.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PolicyRestrictions {
    /// Optional allow-list of LLM provider names. If None, all non-denied providers are allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_providers: Option<Vec<String>>,
    /// Explicit deny-list of LLM provider names.
    #[serde(default)]
    pub denied_providers: Vec<String>,
    /// Optional allow-list of model IDs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    /// Explicit deny-list of model IDs.
    #[serde(default)]
    pub denied_models: Vec<String>,
    /// Optional allow-list of geographic regions for cloud processing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_regions: Option<Vec<String>>,
    /// Explicit deny-list of geographic regions.
    #[serde(default)]
    pub denied_regions: Vec<String>,
    /// Denied third-party integrations or tool names.
    #[serde(default)]
    pub denied_integrations: Vec<String>,
}

impl PolicyRestrictions {
    /// Check whether a given provider is permitted by this policy.
    #[must_use]
    pub fn is_provider_allowed(&self, provider: &str) -> bool {
        if self
            .denied_providers
            .iter()
            .any(|d| d.eq_ignore_ascii_case(provider))
        {
            return false;
        }
        if let Some(ref allowed) = self.allowed_providers {
            return allowed.iter().any(|a| a.eq_ignore_ascii_case(provider));
        }
        true
    }

    /// Check whether a given model ID is permitted by this policy.
    #[must_use]
    pub fn is_model_allowed(&self, model: &str) -> bool {
        if self
            .denied_models
            .iter()
            .any(|d| d.eq_ignore_ascii_case(model))
        {
            return false;
        }
        if let Some(ref allowed) = self.allowed_models {
            return allowed.iter().any(|a| a.eq_ignore_ascii_case(model));
        }
        true
    }

    /// Check whether a region is permitted.
    #[must_use]
    pub fn is_region_allowed(&self, region: &str) -> bool {
        if self
            .denied_regions
            .iter()
            .any(|d| d.eq_ignore_ascii_case(region))
        {
            return false;
        }
        if let Some(ref allowed) = self.allowed_regions {
            return allowed.iter().any(|a| a.eq_ignore_ascii_case(region));
        }
        true
    }

    /// Check whether an integration is permitted.
    #[must_use]
    pub fn is_integration_allowed(&self, integration: &str) -> bool {
        !self
            .denied_integrations
            .iter()
            .any(|d| d.eq_ignore_ascii_case(integration))
    }
}
