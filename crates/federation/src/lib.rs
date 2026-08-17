//! Cross-repository architecture intelligence, publication policy, and federated graph queries.
//!
//! Milestone 6 implementation for Codypendent.

pub mod authorization;
pub mod campaign;
pub mod error;
pub mod identity;
pub mod publication;
pub mod query;
pub mod store;
pub mod tombstone;

pub use authorization::{AuthorizedGrants, RepositoryGrant};
pub use campaign::{
    Campaign, CampaignApproval, CampaignApprovalDecision, CampaignApprovalMode, CampaignEffect,
    CampaignEngine, CampaignError, CampaignKind, CampaignRepository, CampaignRepositoryState,
    CampaignRun, CampaignState, TargetRepositorySpec,
};
pub use error::{FederationError, Result};
pub use identity::{derive_federated_id, normalize_remote, FederatedRepositoryIdentity};
pub use publication::{
    calculate_edge_class, calculate_edge_classification, compute_batch_merkle_root,
    compute_class_inputs_digest, compute_edge_content_hash, compute_node_content_hash,
    derive_shared_edge_id, derive_shared_node_id, BatchState, PublicationClass,
    PublicationDecision, PublicationPolicy, SubjectKind, TombstoneReason,
};
pub use query::{
    BlastRadiusResult, FederationPageCursor, MigrationPlanResult, ReviewerSuggestion,
    SharedGraphQuery,
};
pub use store::{
    PublicationBatch, PublicationRecord, PublishedEdge, PublishedNode, SharedGraphStore,
    TombstoneRecord,
};
pub use tombstone::{GraphTombstone, TombstoneManager};
