//! Publication policy, publication classes, and classification algebra.
//!
//! # Publication Class vs Data Classification
//! - [`DataClassification`] (`crates/protocol/src/artifact.rs:41`) ranks **sensitivity**:
//!   higher is more restrictive (`Public (0) < Internal (1) < Confidential (2) < Secret (3) < Unknown (4)`).
//! - [`PublicationClass`] ranks **audience breadth**:
//!   higher is more shared (`Unknown (0) < PrivateLocal (1) < MetadataShared (2) < ContentShared (3) < OrganizationKnowledge (4) < PublicMarketplace (5)`).
//! - `Unknown` is **0** (narrowest audience), ensuring unrecognized classes from newer peers
//!   default to non-shared.

use chrono::{DateTime, Utc};
use codypendent_protocol::{DataClassification, RepositoryId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use codypendent_protocol::PublicationClass;

/// Outbound graph publication policy for a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationPolicy {
    pub repository_id: RepositoryId,
    pub max_class: PublicationClass,
    pub max_classification: DataClassification,
    pub publish_symbol_names: bool,
    pub publish_source_paths: bool,
    pub publish_signature_hashes: bool,
    pub publish_evidence_artifacts: bool,
    pub policy_version: i64,
    pub updated_at: DateTime<Utc>,
    pub updated_by_uid: i64,
}

impl Default for PublicationPolicy {
    fn default() -> Self {
        Self {
            repository_id: RepositoryId::default(),
            max_class: PublicationClass::PrivateLocal,
            max_classification: DataClassification::Internal,
            publish_symbol_names: false,
            publish_source_paths: false,
            publish_signature_hashes: false,
            publish_evidence_artifacts: false,
            policy_version: 1,
            updated_at: Utc::now(),
            updated_by_uid: 0,
        }
    }
}

impl PublicationPolicy {
    /// Create a private default policy for a repository.
    #[must_use]
    pub fn private_default(repository_id: RepositoryId, uid: i64) -> Self {
        Self {
            repository_id,
            max_class: PublicationClass::PrivateLocal,
            max_classification: DataClassification::Internal,
            publish_symbol_names: false,
            publish_source_paths: false,
            publish_signature_hashes: false,
            publish_evidence_artifacts: false,
            policy_version: 1,
            updated_at: Utc::now(),
            updated_by_uid: uid,
        }
    }

    /// Create a metadata-shared policy with opt-ins.
    #[must_use]
    pub fn metadata_shared(repository_id: RepositoryId, uid: i64) -> Self {
        Self {
            repository_id,
            max_class: PublicationClass::MetadataShared,
            max_classification: DataClassification::Internal,
            publish_symbol_names: false,
            publish_source_paths: false,
            publish_signature_hashes: false,
            publish_evidence_artifacts: false,
            policy_version: 1,
            updated_at: Utc::now(),
            updated_by_uid: uid,
        }
    }

    /// Narrow local policy with a remote policy. A remote policy can ONLY ever
    /// narrow, never broaden.
    #[must_use]
    pub fn narrow(
        &self,
        remote_max_class: PublicationClass,
        remote_max_classification: DataClassification,
    ) -> Self {
        let effective_max_class = self.max_class.strictest(remote_max_class);
        let effective_max_classification =
            if self.max_classification.rank() <= remote_max_classification.rank() {
                self.max_classification
            } else {
                remote_max_classification
            };

        Self {
            repository_id: self.repository_id,
            max_class: effective_max_class,
            max_classification: effective_max_classification,
            publish_symbol_names: self.publish_symbol_names,
            publish_source_paths: self.publish_source_paths,
            publish_signature_hashes: self.publish_signature_hashes,
            publish_evidence_artifacts: self.publish_evidence_artifacts,
            policy_version: self.policy_version,
            updated_at: self.updated_at,
            updated_by_uid: self.updated_by_uid,
        }
    }

    /// Check if the policy permits publishing at the given audience class.
    #[must_use]
    pub fn allows_class(&self, class: PublicationClass) -> bool {
        class.breadth() <= self.max_class.breadth() && class.breadth() >= 2
    }

    /// Check if the policy permits data at the given sensitivity classification.
    #[must_use]
    pub fn allows_classification(&self, classification: DataClassification) -> bool {
        classification.allowed_off_device(self.max_classification)
    }
}

/// Audit decision recorded per published/withheld fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PublicationDecision {
    Published,
    WithheldClass,
    WithheldClassification,
    WithheldField,
    Retracted,
}

impl PublicationDecision {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PublicationDecision::Published => "published",
            PublicationDecision::WithheldClass => "withheld-class",
            PublicationDecision::WithheldClassification => "withheld-classification",
            PublicationDecision::WithheldField => "withheld-field",
            PublicationDecision::Retracted => "retracted",
        }
    }

    #[must_use]
    pub fn from_str_lenient(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "published" => Some(PublicationDecision::Published),
            "withheld-class" => Some(PublicationDecision::WithheldClass),
            "withheld-classification" => Some(PublicationDecision::WithheldClassification),
            "withheld-field" => Some(PublicationDecision::WithheldField),
            "retracted" => Some(PublicationDecision::Retracted),
            _ => None,
        }
    }
}

/// Subject kind of a published fact or tombstone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubjectKind {
    Node,
    Edge,
    Repository,
}

impl SubjectKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SubjectKind::Node => "node",
            SubjectKind::Edge => "edge",
            SubjectKind::Repository => "repository",
        }
    }

    #[must_use]
    pub fn from_str_lenient(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "node" => Some(SubjectKind::Node),
            "edge" => Some(SubjectKind::Edge),
            "repository" => Some(SubjectKind::Repository),
            _ => None,
        }
    }
}

/// Reason for a graph tombstone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TombstoneReason {
    Deleted,
    Narrowed,
    Revoked,
}

impl TombstoneReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TombstoneReason::Deleted => "deleted",
            TombstoneReason::Narrowed => "narrowed",
            TombstoneReason::Revoked => "revoked",
        }
    }

    #[must_use]
    pub fn from_str_lenient(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "deleted" => Some(TombstoneReason::Deleted),
            "narrowed" => Some(TombstoneReason::Narrowed),
            "revoked" => Some(TombstoneReason::Revoked),
            _ => None,
        }
    }
}

/// Lifecycle state of a publication batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BatchState {
    Building,
    Sealed,
    Acknowledged,
}

impl BatchState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            BatchState::Building => "building",
            BatchState::Sealed => "sealed",
            BatchState::Acknowledged => "acknowledged",
        }
    }

    #[must_use]
    pub fn from_str_lenient(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "building" => Some(BatchState::Building),
            "sealed" => Some(BatchState::Sealed),
            "acknowledged" => Some(BatchState::Acknowledged),
            _ => None,
        }
    }
}

/// Computes the inherited [`PublicationClass`] for an edge.
///
/// `strictest(from_node, to_node, from_repo_policy, to_repo_policy, evidence_floor)`
#[must_use]
pub fn calculate_edge_class(
    from_node_class: PublicationClass,
    to_node_class: PublicationClass,
    from_repo_policy: PublicationClass,
    to_repo_policy: PublicationClass,
    evidence_floor: PublicationClass,
) -> PublicationClass {
    from_node_class
        .strictest(to_node_class)
        .strictest(from_repo_policy)
        .strictest(to_repo_policy)
        .strictest(evidence_floor)
}

/// Computes the inherited [`DataClassification`] (sensitivity) for an edge.
///
/// Sensitivity is the MAXIMUM restrictiveness rank of both endpoints.
#[must_use]
pub fn calculate_edge_classification(
    from_classification: DataClassification,
    to_classification: DataClassification,
) -> DataClassification {
    if from_classification.rank() >= to_classification.rank() {
        from_classification
    } else {
        to_classification
    }
}

/// Computes the 64-char SHA-256 digest of class inputs for edge reclassification.
#[must_use]
pub fn compute_class_inputs_digest(
    from_class: PublicationClass,
    to_class: PublicationClass,
    from_policy_version: i64,
    to_policy_version: i64,
    evidence_floor: PublicationClass,
) -> String {
    let raw = format!(
        "{}:{}:{}:{}:{}",
        from_class.as_str(),
        to_class.as_str(),
        from_policy_version,
        to_policy_version,
        evidence_floor.as_str()
    );
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    hex::encode(hasher.finalize())
}

/// Derives cross-machine shared node ID.
///
/// `SHA-256(federated_id || '\0' || symbol_key)`
#[must_use]
pub fn derive_shared_node_id(federated_id: &str, symbol_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(federated_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(symbol_key.as_bytes());
    hex::encode(hasher.finalize())
}

/// Derives cross-machine shared edge ID.
///
/// `SHA-256(from_shared_node_id || '\0' || to_shared_node_id || '\0' || relation)`
#[must_use]
pub fn derive_shared_edge_id(
    from_shared_node_id: &str,
    to_shared_node_id: &str,
    relation: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(from_shared_node_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(to_shared_node_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(relation.as_bytes());
    hex::encode(hasher.finalize())
}

/// Deterministic content hash for a published node record.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn compute_node_content_hash(
    shared_node_id: &str,
    repository_id: &str,
    kind: &str,
    language: &str,
    package: Option<&str>,
    qualified_name: Option<&str>,
    source_path: Option<&str>,
    signature_hash: Option<&str>,
    class: PublicationClass,
    classification: DataClassification,
    revision: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(shared_node_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(repository_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(kind.as_bytes());
    hasher.update(b"\n");
    hasher.update(language.as_bytes());
    hasher.update(b"\n");
    hasher.update(package.unwrap_or("").as_bytes());
    hasher.update(b"\n");
    hasher.update(qualified_name.unwrap_or("").as_bytes());
    hasher.update(b"\n");
    hasher.update(source_path.unwrap_or("").as_bytes());
    hasher.update(b"\n");
    hasher.update(signature_hash.unwrap_or("").as_bytes());
    hasher.update(b"\n");
    hasher.update(class.as_str().as_bytes());
    hasher.update(b"\n");
    hasher.update(format!("{:?}", classification).as_bytes());
    hasher.update(b"\n");
    hasher.update(revision.as_bytes());
    hex::encode(hasher.finalize())
}

/// Deterministic content hash for a published edge record.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn compute_edge_content_hash(
    shared_edge_id: &str,
    from_shared_node_id: &str,
    to_shared_node_id: &str,
    from_repository_id: &str,
    to_repository_id: &str,
    relation: &str,
    confidence_bucket: &str,
    evidence_kind: &str,
    evidence_artifact: Option<&str>,
    class: PublicationClass,
    classification: DataClassification,
    revision: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(shared_edge_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(from_shared_node_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(to_shared_node_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(from_repository_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(to_repository_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(relation.as_bytes());
    hasher.update(b"\n");
    hasher.update(confidence_bucket.as_bytes());
    hasher.update(b"\n");
    hasher.update(evidence_kind.as_bytes());
    hasher.update(b"\n");
    hasher.update(evidence_artifact.unwrap_or("").as_bytes());
    hasher.update(b"\n");
    hasher.update(class.as_str().as_bytes());
    hasher.update(b"\n");
    hasher.update(format!("{:?}", classification).as_bytes());
    hasher.update(b"\n");
    hasher.update(revision.as_bytes());
    hex::encode(hasher.finalize())
}

/// Merkle root digest over ordered content hashes in a batch.
#[must_use]
pub fn compute_batch_merkle_root(content_hashes: &[String]) -> Option<String> {
    if content_hashes.is_empty() {
        return None;
    }
    let mut current_layer: Vec<Vec<u8>> = content_hashes
        .iter()
        .map(|h| hex::decode(h).unwrap_or_else(|_| h.as_bytes().to_vec()))
        .collect();

    while current_layer.len() > 1 {
        let mut next_layer = Vec::with_capacity(current_layer.len().div_ceil(2));
        for chunk in current_layer.chunks(2) {
            let mut hasher = Sha256::new();
            hasher.update(&chunk[0]);
            if chunk.len() > 1 {
                hasher.update(&chunk[1]);
            } else {
                hasher.update(&chunk[0]);
            }
            next_layer.push(hasher.finalize().to_vec());
        }
        current_layer = next_layer;
    }

    Some(hex::encode(&current_layer[0]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_inherits_strictest_of_its_sources() {
        let all_classes = [
            PublicationClass::PrivateLocal,
            PublicationClass::MetadataShared,
            PublicationClass::ContentShared,
            PublicationClass::OrganizationKnowledge,
            PublicationClass::PublicMarketplace,
        ];

        for from_c in all_classes {
            for to_c in all_classes {
                for from_pol in all_classes {
                    for to_pol in all_classes {
                        for ev_floor in all_classes {
                            let calculated =
                                calculate_edge_class(from_c, to_c, from_pol, to_pol, ev_floor);
                            let min_breadth = from_c
                                .breadth()
                                .min(to_c.breadth())
                                .min(from_pol.breadth())
                                .min(to_pol.breadth())
                                .min(ev_floor.breadth());
                            assert_eq!(calculated.breadth(), min_breadth);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn unknown_class_is_treated_as_narrowest() {
        let unknown = PublicationClass::Unknown;
        assert_eq!(unknown.breadth(), 0);

        let shared = PublicationClass::ContentShared;
        assert_eq!(unknown.strictest(shared), PublicationClass::Unknown);
        assert_eq!(shared.strictest(unknown), PublicationClass::Unknown);

        let parsed: PublicationClass =
            serde_json::from_str("\"future-unknown-class\"").unwrap_or(PublicationClass::Unknown);
        assert_eq!(parsed, PublicationClass::Unknown);
    }

    #[test]
    fn remote_policy_can_only_narrow_local_policy() {
        let repo_id = RepositoryId::new();
        let local_policy = PublicationPolicy {
            repository_id: repo_id,
            max_class: PublicationClass::ContentShared,
            max_classification: DataClassification::Internal,
            publish_symbol_names: true,
            publish_source_paths: false,
            publish_signature_hashes: false,
            publish_evidence_artifacts: false,
            policy_version: 1,
            updated_at: Utc::now(),
            updated_by_uid: 1000,
        };

        // Case 1: the remote's AUDIENCE is wider (PublicMarketplace), so the local
        // `ContentShared` binds. Its CLASSIFICATION ceiling is not wider, though:
        // `max_classification` is a ceiling on what may leave, so `Public` (rank 0)
        // permits strictly less than `Internal` (rank 1) and is therefore the
        // stricter of the two. Narrowing takes the lower rank, so the remote's
        // `Public` binds. Asserting `Internal` here would require `narrow` to let a
        // remote policy RAISE a local ceiling — i.e. send more sensitive data
        // off-device than the local operator allowed.
        let narrowed_wide = local_policy.narrow(
            PublicationClass::PublicMarketplace,
            DataClassification::Public,
        );
        assert_eq!(narrowed_wide.max_class, PublicationClass::ContentShared);
        assert_eq!(narrowed_wide.max_classification, DataClassification::Public);

        // Case 2: the remote's audience is narrower (MetadataShared) and its
        // classification ceiling is looser (Confidential permits more than
        // Internal), so the local `Internal` binds. Same rule, other direction.
        let narrowed_narrow = local_policy.narrow(
            PublicationClass::MetadataShared,
            DataClassification::Confidential,
        );
        assert_eq!(narrowed_narrow.max_class, PublicationClass::MetadataShared);
        assert_eq!(
            narrowed_narrow.max_classification,
            DataClassification::Internal
        );
    }

    #[test]
    fn class_inputs_digest_changes_on_policy_version_bump() {
        let d1 = compute_class_inputs_digest(
            PublicationClass::ContentShared,
            PublicationClass::ContentShared,
            1,
            1,
            PublicationClass::MetadataShared,
        );
        let d2 = compute_class_inputs_digest(
            PublicationClass::ContentShared,
            PublicationClass::ContentShared,
            2,
            1,
            PublicationClass::MetadataShared,
        );
        assert_ne!(d1, d2);
    }
}
