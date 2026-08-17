//! Shared graph persistence and projection storage over migrations/0047_graph_publication.sql.

use chrono::{DateTime, Utc};
use codypendent_protocol::{CodeNodeId, DataClassification, RepositoryId};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::error::{FederationError, Result};
use crate::identity::FederatedRepositoryIdentity;
use crate::publication::{
    calculate_edge_class, calculate_edge_classification, compute_batch_merkle_root,
    compute_class_inputs_digest, compute_edge_content_hash, compute_node_content_hash,
    derive_shared_edge_id, derive_shared_node_id, BatchState, PublicationClass,
    PublicationDecision, PublicationPolicy, SubjectKind, TombstoneReason,
};

/// A published node projection record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedNode {
    pub shared_node_id: String,
    pub repository_id: RepositoryId,
    pub code_node_id: Option<CodeNodeId>,
    pub kind: String,
    pub language: String,
    pub package: Option<String>,
    pub qualified_name: Option<String>,
    pub source_path: Option<String>,
    pub signature_hash: Option<String>,
    pub class: PublicationClass,
    pub classification: DataClassification,
    pub revision: String,
    pub content_hash: String,
    pub computed_at: DateTime<Utc>,
}

/// A published edge projection record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishedEdge {
    pub shared_edge_id: String,
    pub from_shared_node_id: String,
    pub to_shared_node_id: String,
    pub from_repository_id: RepositoryId,
    pub to_repository_id: RepositoryId,
    pub relation: String,
    pub confidence: f64,
    pub evidence_kind: String,
    pub evidence_artifact: Option<String>,
    pub class: PublicationClass,
    pub classification: DataClassification,
    pub class_inputs_digest: String,
    pub revision: String,
    pub content_hash: String,
    pub computed_at: DateTime<Utc>,
}

/// A sealed or in-progress graph publication batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationBatch {
    pub id: String,
    pub repository_id: RepositoryId,
    pub owner_uid: i64,
    pub idempotency_key: String,
    pub policy_version: i64,
    pub state: BatchState,
    pub fact_count: i64,
    pub batch_hash: Option<String>,
    pub sealed_at: Option<DateTime<Utc>>,
    pub remote_receipt: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Provenance record of an individual fact publication decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationRecord {
    pub id: String,
    pub batch_id: String,
    pub subject_kind: SubjectKind,
    pub subject_id: String,
    pub repository_id: RepositoryId,
    pub class: PublicationClass,
    pub classification: DataClassification,
    pub decision: PublicationDecision,
    pub policy_version: i64,
    pub content_hash: String,
    pub encryption: String,
    pub retention_class: String,
    pub actor_uid: i64,
    pub published_at: DateTime<Utc>,
}

/// A retraction or deletion tombstone record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TombstoneRecord {
    pub id: String,
    pub repository_id: RepositoryId,
    pub subject_kind: SubjectKind,
    pub subject_id: String,
    pub reason: TombstoneReason,
    pub published_class: PublicationClass,
    pub created_at: DateTime<Utc>,
    pub created_by_uid: i64,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub remote_receipt: Option<String>,
}

/// Database store for shared graph projection, policies, batches, and tombstones.
#[derive(Debug, Clone)]
pub struct SharedGraphStore {
    pool: SqlitePool,
}

impl SharedGraphStore {
    /// Create a new [`SharedGraphStore`] over an existing SQLite connection pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Access the underlying connection pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    // -----------------------------------------------------------------------
    // Federated Repository Identity
    // -----------------------------------------------------------------------

    pub async fn upsert_identity(&self, identity: &FederatedRepositoryIdentity) -> Result<()> {
        sqlx::query(
            "INSERT INTO federated_repository_identity \
             (repository_id, federated_id, root_commit, normalized_remote, display_name, established_at, established_by_uid) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(repository_id) DO UPDATE SET \
                 federated_id = excluded.federated_id, \
                 root_commit = excluded.root_commit, \
                 normalized_remote = excluded.normalized_remote, \
                 display_name = excluded.display_name, \
                 established_at = excluded.established_at, \
                 established_by_uid = excluded.established_by_uid",
        )
        .bind(identity.repository_id.to_string())
        .bind(&identity.federated_id)
        .bind(&identity.root_commit)
        .bind(identity.normalized_remote.as_deref())
        .bind(&identity.display_name)
        .bind(identity.established_at.to_rfc3339())
        .bind(identity.established_by_uid)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_identity(
        &self,
        repository_id: &RepositoryId,
    ) -> Result<Option<FederatedRepositoryIdentity>> {
        let row = sqlx::query(
            "SELECT repository_id, federated_id, root_commit, normalized_remote, display_name, established_at, established_by_uid \
             FROM federated_repository_identity WHERE repository_id = ?",
        )
        .bind(repository_id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        if let Some(r) = row {
            let rep_str: String = r.get("repository_id");
            let rep_id = rep_str
                .parse()
                .map_err(|e| FederationError::Other(format!("invalid repo id: {e}")))?;
            let established_str: String = r.get("established_at");
            let established_at = DateTime::parse_from_rfc3339(&established_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            Ok(Some(FederatedRepositoryIdentity {
                repository_id: rep_id,
                federated_id: r.get("federated_id"),
                root_commit: r.get("root_commit"),
                normalized_remote: r.get("normalized_remote"),
                display_name: r.get("display_name"),
                established_at,
                established_by_uid: r.get("established_by_uid"),
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn get_identity_by_federated_id(
        &self,
        federated_id: &str,
    ) -> Result<Option<FederatedRepositoryIdentity>> {
        let row = sqlx::query(
            "SELECT repository_id, federated_id, root_commit, normalized_remote, display_name, established_at, established_by_uid \
             FROM federated_repository_identity WHERE federated_id = ?",
        )
        .bind(federated_id)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(r) = row {
            let rep_str: String = r.get("repository_id");
            let rep_id = rep_str
                .parse()
                .map_err(|e| FederationError::Other(format!("invalid repo id: {e}")))?;
            let established_str: String = r.get("established_at");
            let established_at = DateTime::parse_from_rfc3339(&established_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            Ok(Some(FederatedRepositoryIdentity {
                repository_id: rep_id,
                federated_id: r.get("federated_id"),
                root_commit: r.get("root_commit"),
                normalized_remote: r.get("normalized_remote"),
                display_name: r.get("display_name"),
                established_at,
                established_by_uid: r.get("established_by_uid"),
            }))
        } else {
            Ok(None)
        }
    }

    // -----------------------------------------------------------------------
    // Publication Policy
    // -----------------------------------------------------------------------

    pub async fn upsert_policy(&self, policy: &PublicationPolicy) -> Result<()> {
        sqlx::query(
            "INSERT INTO graph_publication_policy \
             (repository_id, max_class, max_classification, publish_symbol_names, publish_source_paths, \
              publish_signature_hashes, publish_evidence_artifacts, policy_version, updated_at, updated_by_uid) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(repository_id) DO UPDATE SET \
                 max_class = excluded.max_class, \
                 max_classification = excluded.max_classification, \
                 publish_symbol_names = excluded.publish_symbol_names, \
                 publish_source_paths = excluded.publish_source_paths, \
                 publish_signature_hashes = excluded.publish_signature_hashes, \
                 publish_evidence_artifacts = excluded.publish_evidence_artifacts, \
                 policy_version = excluded.policy_version, \
                 updated_at = excluded.updated_at, \
                 updated_by_uid = excluded.updated_by_uid",
        )
        .bind(policy.repository_id.to_string())
        .bind(policy.max_class.as_str())
        .bind(match policy.max_classification {
            DataClassification::Public => "public",
            DataClassification::Internal => "internal",
            DataClassification::Confidential => "confidential",
            DataClassification::Secret | DataClassification::Unknown => "secret",
            // `DataClassification` is `#[non_exhaustive]`: an unrecognized future
            // variant is treated as at least as restrictive as `Secret`.
            _ => "secret",
        })
        .bind(if policy.publish_symbol_names { 1 } else { 0 })
        .bind(if policy.publish_source_paths { 1 } else { 0 })
        .bind(if policy.publish_signature_hashes { 1 } else { 0 })
        .bind(if policy.publish_evidence_artifacts { 1 } else { 0 })
        .bind(policy.policy_version)
        .bind(policy.updated_at.to_rfc3339())
        .bind(policy.updated_by_uid)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_policy(
        &self,
        repository_id: &RepositoryId,
    ) -> Result<Option<PublicationPolicy>> {
        let row = sqlx::query(
            "SELECT repository_id, max_class, max_classification, publish_symbol_names, publish_source_paths, \
                    publish_signature_hashes, publish_evidence_artifacts, policy_version, updated_at, updated_by_uid \
             FROM graph_publication_policy WHERE repository_id = ?",
        )
        .bind(repository_id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        if let Some(r) = row {
            let rep_str: String = r.get("repository_id");
            let rep_id = rep_str
                .parse()
                .map_err(|e| FederationError::Other(format!("invalid repo id: {e}")))?;
            let max_class_str: String = r.get("max_class");
            let max_class = PublicationClass::from_str_lenient(&max_class_str);

            let max_classification_str: String = r.get("max_classification");
            let max_classification = match max_classification_str.as_str() {
                "public" => DataClassification::Public,
                "internal" => DataClassification::Internal,
                "confidential" => DataClassification::Confidential,
                "secret" => DataClassification::Secret,
                _ => DataClassification::Unknown,
            };

            let updated_str: String = r.get("updated_at");
            let updated_at = DateTime::parse_from_rfc3339(&updated_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            Ok(Some(PublicationPolicy {
                repository_id: rep_id,
                max_class,
                max_classification,
                publish_symbol_names: r.get::<i64, _>("publish_symbol_names") == 1,
                publish_source_paths: r.get::<i64, _>("publish_source_paths") == 1,
                publish_signature_hashes: r.get::<i64, _>("publish_signature_hashes") == 1,
                publish_evidence_artifacts: r.get::<i64, _>("publish_evidence_artifacts") == 1,
                policy_version: r.get("policy_version"),
                updated_at,
                updated_by_uid: r.get("updated_by_uid"),
            }))
        } else {
            Ok(None)
        }
    }

    // -----------------------------------------------------------------------
    // Node Projections
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn project_node(
        &self,
        identity: &FederatedRepositoryIdentity,
        policy: Option<&PublicationPolicy>,
        code_node_id: &CodeNodeId,
        symbol_key: &str,
        kind: &str,
        language: &str,
        package: Option<&str>,
        qualified_name: Option<&str>,
        source_path: Option<&str>,
        signature_hash: Option<&str>,
        candidate_class: PublicationClass,
        candidate_classification: DataClassification,
        revision: &str,
    ) -> Result<(PublishedNode, PublicationDecision)> {
        let shared_node_id = derive_shared_node_id(&identity.federated_id, symbol_key);

        // Effective policy check
        let default_policy = PublicationPolicy::private_default(identity.repository_id, 0);
        let pol = policy.unwrap_or(&default_policy);

        // Three independent reasons to withhold on class, all yielding the same
        // decision: no normalized remote (nothing off-device may name this repo
        // above metadata-shared), above the operator ceiling, or below the
        // metadata-shared floor.
        let withheld_on_class = (identity.normalized_remote.is_none()
            && candidate_class.breadth() > PublicationClass::MetadataShared.breadth())
            || candidate_class.breadth() > pol.max_class.breadth()
            || candidate_class.breadth() < 2;
        let decision = if withheld_on_class {
            PublicationDecision::WithheldClass
        } else if !candidate_classification.allowed_off_device(pol.max_classification) {
            PublicationDecision::WithheldClassification
        } else {
            PublicationDecision::Published
        };

        // Redact fields according to policy flags
        let redacted_qualified_name = if pol.publish_symbol_names {
            qualified_name.map(ToOwned::to_owned)
        } else {
            None
        };
        let redacted_source_path = if pol.publish_source_paths {
            source_path.map(ToOwned::to_owned)
        } else {
            None
        };
        let redacted_signature_hash = if pol.publish_signature_hashes {
            signature_hash.map(ToOwned::to_owned)
        } else {
            None
        };

        let effective_class = candidate_class.strictest(pol.max_class);
        let now = Utc::now();
        let content_hash = compute_node_content_hash(
            &shared_node_id,
            &identity.repository_id.to_string(),
            kind,
            language,
            package,
            redacted_qualified_name.as_deref(),
            redacted_source_path.as_deref(),
            redacted_signature_hash.as_deref(),
            effective_class,
            candidate_classification,
            revision,
        );

        let node = PublishedNode {
            shared_node_id: shared_node_id.clone(),
            repository_id: identity.repository_id,
            code_node_id: Some(*code_node_id),
            kind: kind.to_string(),
            language: language.to_string(),
            package: package.map(ToOwned::to_owned),
            qualified_name: redacted_qualified_name,
            source_path: redacted_source_path,
            signature_hash: redacted_signature_hash,
            class: effective_class,
            classification: candidate_classification,
            revision: revision.to_string(),
            content_hash: content_hash.clone(),
            computed_at: now,
        };

        sqlx::query(
            "INSERT INTO shared_graph_node \
             (shared_node_id, repository_id, code_node_id, kind, language, package, qualified_name, \
              source_path, signature_hash, class, classification, revision, content_hash, computed_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(shared_node_id) DO UPDATE SET \
                 repository_id = excluded.repository_id, \
                 code_node_id = excluded.code_node_id, \
                 kind = excluded.kind, \
                 language = excluded.language, \
                 package = excluded.package, \
                 qualified_name = excluded.qualified_name, \
                 source_path = excluded.source_path, \
                 signature_hash = excluded.signature_hash, \
                 class = excluded.class, \
                 classification = excluded.classification, \
                 revision = excluded.revision, \
                 content_hash = excluded.content_hash, \
                 computed_at = excluded.computed_at",
        )
        .bind(&node.shared_node_id)
        .bind(node.repository_id.to_string())
        .bind(node.code_node_id.map(|id| id.to_string()))
        .bind(&node.kind)
        .bind(&node.language)
        .bind(node.package.as_deref())
        .bind(node.qualified_name.as_deref())
        .bind(node.source_path.as_deref())
        .bind(node.signature_hash.as_deref())
        .bind(node.class.as_str())
        .bind(match node.classification {
            DataClassification::Public => "public",
            DataClassification::Internal => "internal",
            DataClassification::Confidential => "confidential",
            DataClassification::Secret | DataClassification::Unknown => "secret",
            // `DataClassification` is `#[non_exhaustive]`: an unrecognized future
            // variant is treated as at least as restrictive as `Secret`.
            _ => "secret",
        })
        .bind(&node.revision)
        .bind(&node.content_hash)
        .bind(node.computed_at.to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok((node, decision))
    }

    pub async fn get_shared_node(&self, shared_node_id: &str) -> Result<Option<PublishedNode>> {
        let row = sqlx::query(
            "SELECT shared_node_id, repository_id, code_node_id, kind, language, package, \
                    qualified_name, source_path, signature_hash, class, classification, revision, \
                    content_hash, computed_at \
             FROM shared_graph_node WHERE shared_node_id = ?",
        )
        .bind(shared_node_id)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(r) = row {
            let rep_str: String = r.get("repository_id");
            let rep_id = rep_str
                .parse()
                .map_err(|e| FederationError::Other(format!("invalid repo id: {e}")))?;
            let code_node_id: Option<String> = r.get("code_node_id");
            let code_node_id = code_node_id.and_then(|id| id.parse().ok());
            let class_str: String = r.get("class");
            let class = PublicationClass::from_str_lenient(&class_str);

            let classification_str: String = r.get("classification");
            let classification = match classification_str.as_str() {
                "public" => DataClassification::Public,
                "internal" => DataClassification::Internal,
                "confidential" => DataClassification::Confidential,
                "secret" => DataClassification::Secret,
                _ => DataClassification::Unknown,
            };

            let computed_str: String = r.get("computed_at");
            let computed_at = DateTime::parse_from_rfc3339(&computed_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            Ok(Some(PublishedNode {
                shared_node_id: r.get("shared_node_id"),
                repository_id: rep_id,
                code_node_id,
                kind: r.get("kind"),
                language: r.get("language"),
                package: r.get("package"),
                qualified_name: r.get("qualified_name"),
                source_path: r.get("source_path"),
                signature_hash: r.get("signature_hash"),
                class,
                classification,
                revision: r.get("revision"),
                content_hash: r.get("content_hash"),
                computed_at,
            }))
        } else {
            Ok(None)
        }
    }

    // -----------------------------------------------------------------------
    // Edge Projections
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn project_edge(
        &self,
        from_node: &PublishedNode,
        to_node: &PublishedNode,
        from_policy: Option<&PublicationPolicy>,
        to_policy: Option<&PublicationPolicy>,
        relation: &str,
        confidence: f64,
        evidence_kind: &str,
        evidence_artifact: Option<&str>,
        evidence_floor: PublicationClass,
        revision: &str,
    ) -> Result<(PublishedEdge, PublicationDecision)> {
        let shared_edge_id =
            derive_shared_edge_id(&from_node.shared_node_id, &to_node.shared_node_id, relation);

        let default_from = PublicationPolicy::private_default(from_node.repository_id, 0);
        let default_to = PublicationPolicy::private_default(to_node.repository_id, 0);
        let pol_from = from_policy.unwrap_or(&default_from);
        let pol_to = to_policy.unwrap_or(&default_to);

        let inherited_class = calculate_edge_class(
            from_node.class,
            to_node.class,
            pol_from.max_class,
            pol_to.max_class,
            evidence_floor,
        );
        let inherited_classification =
            calculate_edge_classification(from_node.classification, to_node.classification);

        let class_inputs_digest = compute_class_inputs_digest(
            from_node.class,
            to_node.class,
            pol_from.policy_version,
            pol_to.policy_version,
            evidence_floor,
        );

        let decision = if inherited_class.breadth() < 2 {
            PublicationDecision::WithheldClass
        } else if !pol_from.allows_classification(inherited_classification)
            || !pol_to.allows_classification(inherited_classification)
        {
            PublicationDecision::WithheldClassification
        } else {
            PublicationDecision::Published
        };

        // Redact evidence artifact if policy forbids it
        let redacted_artifact =
            if pol_from.publish_evidence_artifacts && pol_to.publish_evidence_artifacts {
                evidence_artifact.map(ToOwned::to_owned)
            } else {
                None
            };

        let confidence_bucket =
            if inherited_class.breadth() <= PublicationClass::MetadataShared.breadth() {
                if confidence >= 0.8 {
                    "high"
                } else if confidence >= 0.5 {
                    "medium"
                } else {
                    "low"
                }
            } else {
                "exact"
            };

        let now = Utc::now();
        let content_hash = compute_edge_content_hash(
            &shared_edge_id,
            &from_node.shared_node_id,
            &to_node.shared_node_id,
            &from_node.repository_id.to_string(),
            &to_node.repository_id.to_string(),
            relation,
            confidence_bucket,
            evidence_kind,
            redacted_artifact.as_deref(),
            inherited_class,
            inherited_classification,
            revision,
        );

        let edge = PublishedEdge {
            shared_edge_id: shared_edge_id.clone(),
            from_shared_node_id: from_node.shared_node_id.clone(),
            to_shared_node_id: to_node.shared_node_id.clone(),
            from_repository_id: from_node.repository_id,
            to_repository_id: to_node.repository_id,
            relation: relation.to_string(),
            confidence,
            evidence_kind: evidence_kind.to_string(),
            evidence_artifact: redacted_artifact,
            class: inherited_class,
            classification: inherited_classification,
            class_inputs_digest: class_inputs_digest.clone(),
            revision: revision.to_string(),
            content_hash: content_hash.clone(),
            computed_at: now,
        };

        sqlx::query(
            "INSERT INTO shared_graph_edge \
             (shared_edge_id, from_shared_node_id, to_shared_node_id, from_repository_id, to_repository_id, \
              relation, confidence, evidence_kind, evidence_artifact, class, classification, \
              class_inputs_digest, revision, content_hash, computed_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(shared_edge_id) DO UPDATE SET \
                 from_shared_node_id = excluded.from_shared_node_id, \
                 to_shared_node_id = excluded.to_shared_node_id, \
                 from_repository_id = excluded.from_repository_id, \
                 to_repository_id = excluded.to_repository_id, \
                 relation = excluded.relation, \
                 confidence = excluded.confidence, \
                 evidence_kind = excluded.evidence_kind, \
                 evidence_artifact = excluded.evidence_artifact, \
                 class = excluded.class, \
                 classification = excluded.classification, \
                 class_inputs_digest = excluded.class_inputs_digest, \
                 revision = excluded.revision, \
                 content_hash = excluded.content_hash, \
                 computed_at = excluded.computed_at",
        )
        .bind(&edge.shared_edge_id)
        .bind(&edge.from_shared_node_id)
        .bind(&edge.to_shared_node_id)
        .bind(edge.from_repository_id.to_string())
        .bind(edge.to_repository_id.to_string())
        .bind(&edge.relation)
        .bind(edge.confidence)
        .bind(&edge.evidence_kind)
        .bind(edge.evidence_artifact.as_deref())
        .bind(edge.class.as_str())
        .bind(match edge.classification {
            DataClassification::Public => "public",
            DataClassification::Internal => "internal",
            DataClassification::Confidential => "confidential",
            DataClassification::Secret | DataClassification::Unknown => "secret",
            // `DataClassification` is `#[non_exhaustive]`: an unrecognized future
            // variant is treated as at least as restrictive as `Secret`.
            _ => "secret",
        })
        .bind(&edge.class_inputs_digest)
        .bind(&edge.revision)
        .bind(&edge.content_hash)
        .bind(edge.computed_at.to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok((edge, decision))
    }

    pub async fn get_shared_edge(&self, shared_edge_id: &str) -> Result<Option<PublishedEdge>> {
        let row = sqlx::query(
            "SELECT shared_edge_id, from_shared_node_id, to_shared_node_id, from_repository_id, \
                    to_repository_id, relation, confidence, evidence_kind, evidence_artifact, \
                    class, classification, class_inputs_digest, revision, content_hash, computed_at \
             FROM shared_graph_edge WHERE shared_edge_id = ?",
        )
        .bind(shared_edge_id)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(r) = row {
            let from_rep_str: String = r.get("from_repository_id");
            let to_rep_str: String = r.get("to_repository_id");
            let from_repository_id = from_rep_str
                .parse()
                .map_err(|e| FederationError::Other(format!("invalid repo id: {e}")))?;
            let to_repository_id = to_rep_str
                .parse()
                .map_err(|e| FederationError::Other(format!("invalid repo id: {e}")))?;
            let class_str: String = r.get("class");
            let class = PublicationClass::from_str_lenient(&class_str);

            let classification_str: String = r.get("classification");
            let classification = match classification_str.as_str() {
                "public" => DataClassification::Public,
                "internal" => DataClassification::Internal,
                "confidential" => DataClassification::Confidential,
                "secret" => DataClassification::Secret,
                _ => DataClassification::Unknown,
            };

            let computed_str: String = r.get("computed_at");
            let computed_at = DateTime::parse_from_rfc3339(&computed_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            Ok(Some(PublishedEdge {
                shared_edge_id: r.get("shared_edge_id"),
                from_shared_node_id: r.get("from_shared_node_id"),
                to_shared_node_id: r.get("to_shared_node_id"),
                from_repository_id,
                to_repository_id,
                relation: r.get("relation"),
                confidence: r.get("confidence"),
                evidence_kind: r.get("evidence_kind"),
                evidence_artifact: r.get("evidence_artifact"),
                class,
                classification,
                class_inputs_digest: r.get("class_inputs_digest"),
                revision: r.get("revision"),
                content_hash: r.get("content_hash"),
                computed_at,
            }))
        } else {
            Ok(None)
        }
    }

    // -----------------------------------------------------------------------
    // Batches & Publications
    // -----------------------------------------------------------------------

    pub async fn create_batch_idempotent(
        &self,
        repository_id: &RepositoryId,
        owner_uid: i64,
        idempotency_key: &str,
        policy_version: i64,
    ) -> Result<(PublicationBatch, bool)> {
        let existing = sqlx::query(
            "SELECT id, repository_id, owner_uid, idempotency_key, policy_version, state, fact_count, \
                    batch_hash, sealed_at, remote_receipt, acknowledged_at, created_at \
             FROM graph_publication_batch WHERE owner_uid = ? AND idempotency_key = ?",
        )
        .bind(owner_uid)
        .bind(idempotency_key)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(r) = existing {
            let rep_str: String = r.get("repository_id");
            let rep_id = rep_str
                .parse()
                .map_err(|e| FederationError::Other(format!("invalid repo id: {e}")))?;
            let state_str: String = r.get("state");
            let state = BatchState::from_str_lenient(&state_str).unwrap_or(BatchState::Building);
            let created_str: String = r.get("created_at");
            let created_at = DateTime::parse_from_rfc3339(&created_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let sealed_at: Option<String> = r.get("sealed_at");
            let sealed_at = sealed_at.and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            });
            let acknowledged_at: Option<String> = r.get("acknowledged_at");
            let acknowledged_at = acknowledged_at.and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            });

            return Ok((
                PublicationBatch {
                    id: r.get("id"),
                    repository_id: rep_id,
                    owner_uid: r.get("owner_uid"),
                    idempotency_key: r.get("idempotency_key"),
                    policy_version: r.get("policy_version"),
                    state,
                    fact_count: r.get("fact_count"),
                    batch_hash: r.get("batch_hash"),
                    sealed_at,
                    remote_receipt: r.get("remote_receipt"),
                    acknowledged_at,
                    created_at,
                },
                false,
            ));
        }

        let id = Uuid::now_v7().to_string();
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO graph_publication_batch \
             (id, repository_id, owner_uid, idempotency_key, policy_version, state, fact_count, created_at) \
             VALUES (?, ?, ?, ?, ?, 'building', 0, ?)",
        )
        .bind(&id)
        .bind(repository_id.to_string())
        .bind(owner_uid)
        .bind(idempotency_key)
        .bind(policy_version)
        .bind(now.to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok((
            PublicationBatch {
                id,
                repository_id: *repository_id,
                owner_uid,
                idempotency_key: idempotency_key.to_string(),
                policy_version,
                state: BatchState::Building,
                fact_count: 0,
                batch_hash: None,
                sealed_at: None,
                remote_receipt: None,
                acknowledged_at: None,
                created_at: now,
            },
            true,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_publication(
        &self,
        batch_id: &str,
        subject_kind: SubjectKind,
        subject_id: &str,
        repository_id: &RepositoryId,
        class: PublicationClass,
        classification: DataClassification,
        decision: PublicationDecision,
        policy_version: i64,
        content_hash: &str,
        encryption: &str,
        retention_class: &str,
        actor_uid: i64,
    ) -> Result<PublicationRecord> {
        let id = Uuid::now_v7().to_string();
        let now = Utc::now();

        sqlx::query(
            "INSERT INTO graph_publication \
             (id, batch_id, subject_kind, subject_id, repository_id, class, classification, decision, \
              policy_version, content_hash, encryption, retention_class, actor_uid, published_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(batch_id, subject_kind, subject_id) DO NOTHING",
        )
        .bind(&id)
        .bind(batch_id)
        .bind(subject_kind.as_str())
        .bind(subject_id)
        .bind(repository_id.to_string())
        .bind(class.as_str())
        .bind(match classification {
            DataClassification::Public => "public",
            DataClassification::Internal => "internal",
            DataClassification::Confidential => "confidential",
            DataClassification::Secret | DataClassification::Unknown => "secret",
            // `DataClassification` is `#[non_exhaustive]`: an unrecognized future
            // variant is treated as at least as restrictive as `Secret`.
            _ => "secret",
        })
        .bind(decision.as_str())
        .bind(policy_version)
        .bind(content_hash)
        .bind(encryption)
        .bind(retention_class)
        .bind(actor_uid)
        .bind(now.to_rfc3339())
        .execute(&self.pool)
        .await?;

        // Increment fact count if published
        if decision == PublicationDecision::Published {
            sqlx::query(
                "UPDATE graph_publication_batch SET fact_count = fact_count + 1 WHERE id = ?",
            )
            .bind(batch_id)
            .execute(&self.pool)
            .await?;
        }

        Ok(PublicationRecord {
            id,
            batch_id: batch_id.to_string(),
            subject_kind,
            subject_id: subject_id.to_string(),
            repository_id: *repository_id,
            class,
            classification,
            decision,
            policy_version,
            content_hash: content_hash.to_string(),
            encryption: encryption.to_string(),
            retention_class: retention_class.to_string(),
            actor_uid,
            published_at: now,
        })
    }

    /// Seal a publication batch.
    ///
    /// # Tombstone Precedence Assertion
    /// Before sealing, this method checks whether there are any unacknowledged tombstones
    /// for this repository. If so, sealing is refused (`UnacknowledgedTombstonesPending`)
    /// to guarantee tombstones are drained first.
    pub async fn seal_batch(&self, batch_id: &str) -> Result<PublicationBatch> {
        let batch_row = sqlx::query(
            "SELECT id, repository_id, owner_uid, idempotency_key, policy_version, state, fact_count, \
                    batch_hash, sealed_at, remote_receipt, acknowledged_at, created_at \
             FROM graph_publication_batch WHERE id = ?",
        )
        .bind(batch_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| FederationError::BatchNotFound(batch_id.to_string()))?;

        let rep_str: String = batch_row.get("repository_id");
        let rep_id: RepositoryId = rep_str
            .parse()
            .map_err(|e| FederationError::Other(format!("invalid repo id: {e}")))?;
        let state_str: String = batch_row.get("state");
        if state_str == "sealed" || state_str == "acknowledged" {
            return Err(FederationError::BatchAlreadySealed(batch_id.to_string()));
        }

        // Check for pending unacknowledged tombstones for this repository
        let pending_tombstones: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM graph_tombstone WHERE repository_id = ? AND acknowledged_at IS NULL",
        )
        .bind(rep_id.to_string())
        .fetch_one(&self.pool)
        .await?;

        if pending_tombstones.0 > 0 {
            return Err(FederationError::UnacknowledgedTombstonesPending);
        }

        // Compute Merkle root over published facts
        let hashes: Vec<(String,)> = sqlx::query_as(
            "SELECT content_hash FROM graph_publication WHERE batch_id = ? AND decision = 'published' ORDER BY id ASC",
        )
        .bind(batch_id)
        .fetch_all(&self.pool)
        .await?;

        let hash_strs: Vec<String> = hashes.into_iter().map(|(h,)| h).collect();
        let batch_hash = compute_batch_merkle_root(&hash_strs).unwrap_or_else(|| {
            // For empty batches (e.g. absent policy), hash of empty byte sequence
            let mut hasher = sha2::Sha256::new();
            hasher.update(b"empty_batch");
            hex::encode(hasher.finalize())
        });

        let now = Utc::now();
        sqlx::query(
            "UPDATE graph_publication_batch \
             SET state = 'sealed', batch_hash = ?, sealed_at = ? \
             WHERE id = ?",
        )
        .bind(&batch_hash)
        .bind(now.to_rfc3339())
        .bind(batch_id)
        .execute(&self.pool)
        .await?;

        let fact_count = hash_strs.len() as i64;
        let created_str: String = batch_row.get("created_at");
        let created_at = DateTime::parse_from_rfc3339(&created_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        Ok(PublicationBatch {
            id: batch_id.to_string(),
            repository_id: rep_id,
            owner_uid: batch_row.get("owner_uid"),
            idempotency_key: batch_row.get("idempotency_key"),
            policy_version: batch_row.get("policy_version"),
            state: BatchState::Sealed,
            fact_count,
            batch_hash: Some(batch_hash),
            sealed_at: Some(now),
            remote_receipt: None,
            acknowledged_at: None,
            created_at,
        })
    }

    pub async fn acknowledge_batch(&self, batch_id: &str, remote_receipt: &str) -> Result<()> {
        let now = Utc::now();
        sqlx::query(
            "UPDATE graph_publication_batch \
             SET state = 'acknowledged', remote_receipt = ?, acknowledged_at = ? \
             WHERE id = ?",
        )
        .bind(remote_receipt)
        .bind(now.to_rfc3339())
        .bind(batch_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Tombstones
    // -----------------------------------------------------------------------

    pub async fn record_tombstone(&self, tombstone: &TombstoneRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO graph_tombstone \
             (id, repository_id, subject_kind, subject_id, reason, published_class, created_at, created_by_uid, acknowledged_at, remote_receipt) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(repository_id, subject_kind, subject_id, created_at) DO NOTHING",
        )
        .bind(&tombstone.id)
        .bind(tombstone.repository_id.to_string())
        .bind(tombstone.subject_kind.as_str())
        .bind(&tombstone.subject_id)
        .bind(tombstone.reason.as_str())
        .bind(tombstone.published_class.as_str())
        .bind(tombstone.created_at.to_rfc3339())
        .bind(tombstone.created_by_uid)
        .bind(tombstone.acknowledged_at.map(|dt| dt.to_rfc3339()))
        .bind(tombstone.remote_receipt.as_deref())
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_unacknowledged_tombstones(
        &self,
        repository_id: &RepositoryId,
    ) -> Result<Vec<TombstoneRecord>> {
        let rows = sqlx::query(
            "SELECT id, repository_id, subject_kind, subject_id, reason, published_class, \
                    created_at, created_by_uid \
             FROM graph_tombstone \
             WHERE repository_id = ? AND acknowledged_at IS NULL \
             ORDER BY created_at ASC",
        )
        .bind(repository_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        let mut results = Vec::with_capacity(rows.len());
        for r in rows {
            let rep_str: String = r.get("repository_id");
            let rep_id = rep_str
                .parse()
                .map_err(|e| FederationError::Other(format!("invalid repo id: {e}")))?;
            let kind_str: String = r.get("subject_kind");
            let kind = SubjectKind::from_str_lenient(&kind_str).unwrap_or(SubjectKind::Node);
            let reason_str: String = r.get("reason");
            let reason =
                TombstoneReason::from_str_lenient(&reason_str).unwrap_or(TombstoneReason::Deleted);
            let class_str: String = r.get("published_class");
            let published_class = PublicationClass::from_str_lenient(&class_str);
            let created_str: String = r.get("created_at");
            let created_at = DateTime::parse_from_rfc3339(&created_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            results.push(TombstoneRecord {
                id: r.get("id"),
                repository_id: rep_id,
                subject_kind: kind,
                subject_id: r.get("subject_id"),
                reason,
                published_class,
                created_at,
                created_by_uid: r.get("created_by_uid"),
                acknowledged_at: None,
                remote_receipt: None,
            });
        }

        Ok(results)
    }

    pub async fn acknowledge_tombstones(
        &self,
        tombstone_ids: &[String],
        remote_receipt: &str,
    ) -> Result<()> {
        if tombstone_ids.is_empty() {
            return Ok(());
        }

        let now = Utc::now();
        for id in tombstone_ids {
            sqlx::query(
                "UPDATE graph_tombstone SET acknowledged_at = ?, remote_receipt = ? WHERE id = ?",
            )
            .bind(now.to_rfc3339())
            .bind(remote_receipt)
            .bind(id)
            .execute(&self.pool)
            .await?;
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Reclassification and Retraction
    // -----------------------------------------------------------------------

    /// Reclassifies all edges incident on a repository after a policy change,
    /// generating tombstones for any edge whose class has narrowed.
    pub async fn reclassify_edges_for_repository(
        &self,
        repository_id: &RepositoryId,
        uid: i64,
    ) -> Result<Vec<TombstoneRecord>> {
        let edges = sqlx::query(
            "SELECT e.shared_edge_id, e.from_shared_node_id, e.to_shared_node_id, e.from_repository_id, \
                    e.to_repository_id, e.relation, e.confidence, e.evidence_kind, e.evidence_artifact, \
                    e.class, e.classification, e.class_inputs_digest, e.revision, \
                    fn.class as from_class, tn.class as to_class, \
                    fp.policy_version as from_pol_ver, tp.policy_version as to_pol_ver, \
                    fp.max_class as from_max_class, tp.max_class as to_max_class \
             FROM shared_graph_edge e \
             JOIN shared_graph_node fn ON fn.shared_node_id = e.from_shared_node_id \
             JOIN shared_graph_node tn ON tn.shared_node_id = e.to_shared_node_id \
             LEFT JOIN graph_publication_policy fp ON fp.repository_id = e.from_repository_id \
             LEFT JOIN graph_publication_policy tp ON tp.repository_id = e.to_repository_id \
             WHERE e.from_repository_id = ? OR e.to_repository_id = ?",
        )
        .bind(repository_id.to_string())
        .bind(repository_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        let mut tombstones = Vec::new();
        let now = Utc::now();

        for r in edges {
            let edge_id: String = r.get("shared_edge_id");
            let old_class_str: String = r.get("class");
            let old_class = PublicationClass::from_str_lenient(&old_class_str);
            let old_digest: String = r.get("class_inputs_digest");

            let from_class = PublicationClass::from_str_lenient(&r.get::<String, _>("from_class"));
            let to_class = PublicationClass::from_str_lenient(&r.get::<String, _>("to_class"));
            let from_pol_ver: i64 = r.get("from_pol_ver");
            let to_pol_ver: i64 = r.get("to_pol_ver");
            let from_max_class = PublicationClass::from_str_lenient(
                &r.get::<Option<String>, _>("from_max_class")
                    .unwrap_or_default(),
            );
            let to_max_class = PublicationClass::from_str_lenient(
                &r.get::<Option<String>, _>("to_max_class")
                    .unwrap_or_default(),
            );

            let evidence_floor = PublicationClass::MetadataShared;
            let new_digest = compute_class_inputs_digest(
                from_class,
                to_class,
                from_pol_ver,
                to_pol_ver,
                evidence_floor,
            );

            if new_digest != old_digest {
                let new_class = calculate_edge_class(
                    from_class,
                    to_class,
                    from_max_class,
                    to_max_class,
                    evidence_floor,
                );

                // If class narrowed, generate tombstone
                if new_class.breadth() < old_class.breadth() {
                    let tombstone = TombstoneRecord {
                        id: Uuid::now_v7().to_string(),
                        repository_id: *repository_id,
                        subject_kind: SubjectKind::Edge,
                        subject_id: edge_id.clone(),
                        reason: TombstoneReason::Narrowed,
                        published_class: old_class,
                        created_at: now,
                        created_by_uid: uid,
                        acknowledged_at: None,
                        remote_receipt: None,
                    };
                    self.record_tombstone(&tombstone).await?;
                    tombstones.push(tombstone);
                }

                // Update edge with new class and digest
                sqlx::query(
                    "UPDATE shared_graph_edge \
                     SET class = ?, class_inputs_digest = ? \
                     WHERE shared_edge_id = ?",
                )
                .bind(new_class.as_str())
                .bind(&new_digest)
                .bind(&edge_id)
                .execute(&self.pool)
                .await?;
            }
        }

        Ok(tombstones)
    }
}
