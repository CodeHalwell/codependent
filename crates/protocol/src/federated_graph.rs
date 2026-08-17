//! Wire contracts for cross-repository architecture intelligence, outbound publication
//! management, access-safe traversal queries, and multi-repository campaigns (Milestone 6).
//!
//! # Publication Class vs Data Classification
//! - [`DataClassification`](crate::artifact::DataClassification) ranks **sensitivity**:
//!   higher is more restrictive (`Public (0) < Internal (1) < Confidential (2) < Secret (3) < Unknown (4)`).
//! - [`PublicationClass`] ranks **audience breadth**:
//!   higher is more shared (`Unknown (0) < PrivateLocal (1) < MetadataShared (2) < ContentShared (3) < OrganizationKnowledge (4) < PublicMarketplace (5)`).
//! - `Unknown` is **0** (narrowest audience), ensuring unrecognized classes from newer peers
//!   default to non-shared.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::artifact::DataClassification;
use crate::ids::RunId;
use crate::session::PageCursor;

/// Audience breadth of a published fact or repository policy ceiling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum PublicationClass {
    /// Strictly private to local daemon / checkout. Published to no one.
    #[serde(alias = "private", alias = "PrivateLocal", alias = "Private")]
    #[default]
    PrivateLocal,
    /// High-level topology and metadata only (kinds, packages, linkages). No symbol names or paths.
    #[serde(alias = "MetadataShared")]
    MetadataShared,
    /// Interface and signature facts shared within an approved scope.
    #[serde(
        alias = "interface-shared",
        alias = "ContentShared",
        alias = "InterfaceShared"
    )]
    ContentShared,
    /// Full organizational graph knowledge.
    #[serde(alias = "OrganizationKnowledge")]
    OrganizationKnowledge,
    /// Publicly distributed marketplace knowledge.
    #[serde(
        alias = "full-source",
        alias = "PublicMarketplace",
        alias = "FullSource"
    )]
    PublicMarketplace,
    /// An unrecognized class from a newer peer. Treated as the narrowest audience.
    #[serde(other)]
    Unknown,
}

impl PartialOrd for PublicationClass {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PublicationClass {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.breadth().cmp(&other.breadth())
    }
}

impl PublicationClass {
    /// Audience breadth, narrowest (0) to widest (5).
    #[must_use]
    pub fn breadth(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::PrivateLocal => 1,
            Self::MetadataShared => 2,
            Self::ContentShared => 3,
            Self::OrganizationKnowledge => 4,
            Self::PublicMarketplace => 5,
        }
    }

    /// The strictest (narrowest) of two classes. Inheritance is always a MIN.
    #[must_use]
    pub fn strictest(self, other: Self) -> Self {
        if self.breadth() <= other.breadth() {
            self
        } else {
            other
        }
    }

    /// Canonical database and protocol string representation.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::PrivateLocal => "private-local",
            Self::MetadataShared => "metadata-shared",
            Self::ContentShared => "content-shared",
            Self::OrganizationKnowledge => "organization-knowledge",
            Self::PublicMarketplace => "public-marketplace",
        }
    }

    /// Parse from a string, falling back to [`PublicationClass::Unknown`].
    #[must_use]
    pub fn from_str_lenient(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "private-local" | "private" | "privatelocal" => Self::PrivateLocal,
            "metadata-shared" | "metadatashared" => Self::MetadataShared,
            "content-shared" | "interface-shared" | "contentshared" | "interfaceshared" => {
                Self::ContentShared
            }
            "organization-knowledge" | "organizationknowledge" => Self::OrganizationKnowledge,
            "public-marketplace" | "full-source" | "publicmarketplace" | "fullsource" => {
                Self::PublicMarketplace
            }
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for PublicationClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Durable federated identity view of a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct FederatedRepositoryIdentityView {
    pub repository_id: String,
    pub federated_id: String,
    pub root_commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_remote: Option<String>,
    pub display_name: String,
    pub established_at: DateTime<Utc>,
}

/// Publication policy view for a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct GraphPublicationPolicyView {
    pub repository_id: String,
    pub max_class: PublicationClass,
    pub max_classification: DataClassification,
    pub publish_symbol_names: bool,
    pub publish_source_paths: bool,
    pub publish_signature_hashes: bool,
    pub publish_evidence_artifacts: bool,
    pub policy_version: u64,
    pub updated_at: DateTime<Utc>,
}

/// Client request to update publication policy for a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct UpdatePublicationPolicyRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_class: Option<PublicationClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_classification: Option<DataClassification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_symbol_names: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_source_paths: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_signature_hashes: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_evidence_artifacts: Option<bool>,
}

/// Summary report of a publication batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PublicationBatchSummary {
    pub batch_id: String,
    pub repository_id: String,
    pub policy_version: u64,
    pub state: String,
    pub fact_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<DateTime<Utc>>,
}

/// Published node fact projection view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SharedNodeView {
    pub shared_node_id: String,
    pub repository_id: String,
    pub kind: String,
    pub language: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualified_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_hash: Option<String>,
    pub class: PublicationClass,
    pub classification: DataClassification,
    pub revision: String,
}

/// Published edge fact projection view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SharedEdgeView {
    pub shared_edge_id: String,
    pub from_shared_node_id: String,
    pub to_shared_node_id: String,
    pub from_repository_id: String,
    pub to_repository_id: String,
    pub relation: String,
    pub confidence: f64,
    pub evidence_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_artifact: Option<String>,
    pub class: PublicationClass,
    pub classification: DataClassification,
    pub revision: String,
}

/// Filtered query for shared nodes and edges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct FederatedGraphQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_ceiling: Option<PublicationClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Paginated result of a federated graph query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct FederatedGraphPage {
    pub nodes: Vec<SharedNodeView>,
    pub edges: Vec<SharedEdgeView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
    pub has_more: bool,
}

/// Query for cross-repository blast radius analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BlastRadiusQuery {
    pub repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// A node in a blast radius result graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BlastRadiusNode {
    pub shared_node_id: String,
    pub repository_id: String,
    pub display_name: String,
    pub kind: String,
    pub depth: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relation_path: Vec<String>,
    pub class: PublicationClass,
}

/// Result report of a blast radius query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BlastRadiusReport {
    pub seed_node_id: String,
    pub affected_repositories: Vec<String>,
    pub affected_nodes: Vec<BlastRadiusNode>,
    pub edge_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
    pub has_more: bool,
}

/// Kind of multi-repository campaign or migration plan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum CampaignKind {
    ApiMigration,
    SchemaMigration,
    DependencyUpgrade,
    OwnershipReview,
    #[default]
    Custom,
    #[serde(other)]
    Unknown,
}

impl CampaignKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiMigration => "api-migration",
            Self::SchemaMigration => "schema-migration",
            Self::DependencyUpgrade => "dependency-upgrade",
            Self::OwnershipReview => "ownership-review",
            Self::Custom => "custom",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub fn from_str_lenient(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "api-migration" | "apimigration" => Self::ApiMigration,
            "schema-migration" | "schemamigration" => Self::SchemaMigration,
            "dependency-upgrade" | "dependencyupgrade" => Self::DependencyUpgrade,
            "ownership-review" | "ownershipreview" => Self::OwnershipReview,
            "custom" => Self::Custom,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for CampaignKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Lifecycle state of a coordinated campaign.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum CampaignState {
    #[default]
    Planning,
    Running,
    PartiallyFailed,
    Completed,
    Cancelled,
    #[serde(other)]
    Unknown,
}

impl CampaignState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Running => "running",
            Self::PartiallyFailed => "partially-failed",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub fn from_str_lenient(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "planning" => Self::Planning,
            "running" => Self::Running,
            "partially-failed" | "partiallyfailed" => Self::PartiallyFailed,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for CampaignState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Approval mode for a campaign repository enrollment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum CampaignApprovalMode {
    #[default]
    PerEffect,
    PerRun,
    #[serde(other)]
    Unknown,
}

impl CampaignApprovalMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PerEffect => "per-effect",
            Self::PerRun => "per-run",
            Self::Unknown => "unknown",
        }
    }
}

/// State of a repository within a campaign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum CampaignRepoState {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
    Denied,
    Skipped,
    #[serde(other)]
    Unknown,
}

impl CampaignRepoState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Denied => "denied",
            Self::Skipped => "skipped",
            Self::Unknown => "unknown",
        }
    }
}

/// Query to plan a cross-repository API or schema migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct MigrationPlanQuery {
    pub source_repository: String,
    pub source_symbol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_repositories: Vec<String>,
    pub kind: CampaignKind,
}

/// A step in an architectural migration plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct MigrationPlanStep {
    pub step_number: u32,
    pub repository_id: String,
    pub action: String,
    pub target_symbols: Vec<String>,
    pub estimated_risk: String,
}

/// Report of a cross-repository migration plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct MigrationPlanReport {
    pub title: String,
    pub kind: CampaignKind,
    pub source_repository: String,
    pub steps: Vec<MigrationPlanStep>,
    pub total_affected_repositories: u32,
}

/// Query to suggest reviewers based on graph topology and changed symbols/paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ReviewerSuggestionQuery {
    pub repository: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_symbols: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// A suggested reviewer with confidence and reasoning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ReviewerSuggestion {
    pub reviewer_id: String,
    pub confidence: f64,
    pub reason: String,
    pub relevant_symbols: Vec<String>,
    pub relevant_repositories: Vec<String>,
}

/// Container for reviewer suggestions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ReviewerSuggestions {
    pub suggestions: Vec<ReviewerSuggestion>,
}

/// Summary view of a coordinated campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CampaignView {
    pub id: String,
    pub title: String,
    pub kind: CampaignKind,
    pub workflow_id: String,
    pub state: CampaignState,
    pub repository_count: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_at: Option<DateTime<Utc>>,
}

/// Enrollment specification for a repository in a campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CampaignRepoEnrollment {
    pub repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_minor_units: Option<u64>,
    #[serde(default)]
    pub approval_mode: CampaignApprovalMode,
}

/// View of an enrolled repository in a campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CampaignRepositoryView {
    pub campaign_id: String,
    pub repository_id: String,
    pub federated_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_minor_units: Option<u64>,
    pub approval_mode: CampaignApprovalMode,
    pub state: CampaignRepoState,
    pub enrolled_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_at: Option<DateTime<Utc>>,
}

/// View of a child workflow run under a campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CampaignRunView {
    pub campaign_id: String,
    pub repository_id: String,
    pub run_id: RunId,
    pub attempt: u32,
    pub state: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_at: Option<DateTime<Utc>>,
}

/// View of an approval decision within a campaign repository slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CampaignApprovalView {
    pub campaign_id: String,
    pub repository_id: String,
    pub approval_id: String,
    pub action_digest: String,
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
}

/// View of an effect recorded in the campaign effect ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CampaignEffectView {
    pub id: String,
    pub campaign_id: String,
    pub repository_id: String,
    pub run_id: String,
    pub effect_kind: String,
    pub effect_digest: String,
    pub applied_at: DateTime<Utc>,
}

/// Full detail view of a campaign and all child projections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CampaignDetailView {
    pub campaign: CampaignView,
    pub repositories: Vec<CampaignRepositoryView>,
    pub runs: Vec<CampaignRunView>,
    pub approvals: Vec<CampaignApprovalView>,
    pub effects: Vec<CampaignEffectView>,
}

/// Request to create a new coordinated campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CreateCampaignRequest {
    pub title: String,
    pub kind: CampaignKind,
    pub workflow_id: String,
    pub repositories: Vec<CampaignRepoEnrollment>,
    pub idempotency_key: String,
}

/// Request to execute or re-drive a campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ExecuteCampaignRequest {
    pub campaign_id: String,
    #[serde(default)]
    pub retry_failed_only: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_class_breadth_and_strictest_ordering() {
        assert_eq!(PublicationClass::Unknown.breadth(), 0);
        assert_eq!(PublicationClass::PrivateLocal.breadth(), 1);
        assert_eq!(PublicationClass::MetadataShared.breadth(), 2);
        assert_eq!(PublicationClass::ContentShared.breadth(), 3);
        assert_eq!(PublicationClass::OrganizationKnowledge.breadth(), 4);
        assert_eq!(PublicationClass::PublicMarketplace.breadth(), 5);

        assert_eq!(
            PublicationClass::OrganizationKnowledge.strictest(PublicationClass::MetadataShared),
            PublicationClass::MetadataShared
        );
        assert_eq!(
            PublicationClass::PrivateLocal.strictest(PublicationClass::PublicMarketplace),
            PublicationClass::PrivateLocal
        );
        assert_eq!(
            PublicationClass::Unknown.strictest(PublicationClass::ContentShared),
            PublicationClass::Unknown
        );
    }

    #[test]
    fn publication_class_serialization_round_trips() {
        let class = PublicationClass::MetadataShared;
        let serialized = serde_json::to_string(&class).unwrap();
        assert_eq!(serialized, "\"metadata-shared\"");
        let deserialized: PublicationClass = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, PublicationClass::MetadataShared);

        // Unknown fallback
        let unknown: PublicationClass = serde_json::from_str("\"future-custom-class\"").unwrap();
        assert_eq!(unknown, PublicationClass::Unknown);
    }

    #[test]
    fn campaign_kind_and_state_round_trip() {
        let kind = CampaignKind::ApiMigration;
        let s = serde_json::to_string(&kind).unwrap();
        assert_eq!(s, "\"api-migration\"");
        let parsed: CampaignKind = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, CampaignKind::ApiMigration);

        let state = CampaignState::PartiallyFailed;
        let s2 = serde_json::to_string(&state).unwrap();
        assert_eq!(s2, "\"partially-failed\"");
        let parsed2: CampaignState = serde_json::from_str(&s2).unwrap();
        assert_eq!(parsed2, CampaignState::PartiallyFailed);
    }
}
