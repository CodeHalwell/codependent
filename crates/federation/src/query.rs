//! Access-safe cross-repository graph queries, traversal, and planning.
//!
//! # Traversal Invariant
//! Access authorization grants are applied at **seed selection** and at **every recursive step**.
//! An unauthorized intermediate node does NOT extend the frontier: paths through it do not exist,
//! preventing hidden nodes from leaking their existence through hop counts, path lengths, or counts.
//!
//! Seed selection returns the exact same [`FederationError::NodeNotFound`] for absent and
//! unauthorized nodes alike.

use std::collections::{HashSet, VecDeque};

use codypendent_protocol::RepositoryId;
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::authorization::AuthorizedGrants;
use crate::error::{FederationError, Result};
use crate::store::{PublishedEdge, PublishedNode, SharedGraphStore};

/// Result of a cross-repository blast-radius analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlastRadiusResult {
    pub seed_node: PublishedNode,
    pub reachable_nodes: Vec<PublishedNode>,
    pub reachable_edges: Vec<PublishedEdge>,
    pub impacted_repositories: Vec<RepositoryId>,
    pub max_depth_reached: usize,
}

/// Result of a cross-repository migration planning query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MigrationPlanResult {
    pub target_node: PublishedNode,
    pub referencing_nodes: Vec<PublishedNode>,
    pub referencing_edges: Vec<PublishedEdge>,
    pub impacted_repositories: Vec<RepositoryId>,
}

/// Suggested reviewer based on cross-repository ownership and publication facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerSuggestion {
    pub principal_uid: i64,
    pub repository_id: RepositoryId,
    pub rationale: String,
}

/// Keyset pagination cursor bound to a principal and query hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FederationPageCursor {
    pub principal_uid: i64,
    pub query_hash: String,
    pub last_id: String,
}

impl FederationPageCursor {
    /// Create and encode a new pagination cursor.
    #[must_use]
    pub fn encode_cursor(principal_uid: i64, query_hash: &str, last_id: &str) -> String {
        let cursor = Self {
            principal_uid,
            query_hash: query_hash.to_string(),
            last_id: last_id.to_string(),
        };
        let json = serde_json::to_string(&cursor).unwrap_or_default();
        hex::encode(json)
    }

    /// Decode and validate a cursor against the current principal and query.
    pub fn decode_and_verify(
        encoded: &str,
        expected_principal: i64,
        expected_query_hash: &str,
    ) -> Result<String> {
        let bytes = hex::decode(encoded).map_err(|_| FederationError::InvalidCursor)?;
        let cursor: FederationPageCursor =
            serde_json::from_slice(&bytes).map_err(|_| FederationError::InvalidCursor)?;

        if cursor.principal_uid != expected_principal || cursor.query_hash != expected_query_hash {
            return Err(FederationError::InvalidCursor);
        }

        Ok(cursor.last_id)
    }
}

/// Query service over the shared federated graph.
#[derive(Debug, Clone)]
pub struct SharedGraphQuery {
    store: SharedGraphStore,
}

impl SharedGraphQuery {
    /// Create a new query service over a [`SharedGraphStore`].
    #[must_use]
    pub fn new(store: SharedGraphStore) -> Self {
        Self { store }
    }

    /// Compute the blast radius from a seed node across repository boundaries.
    ///
    /// Unauthorized intermediate nodes terminate traversal at their boundary.
    pub async fn blast_radius(
        &self,
        seed_shared_node_id: &str,
        grants: &AuthorizedGrants,
        max_depth: usize,
    ) -> Result<BlastRadiusResult> {
        let seed_node = self
            .store
            .get_shared_node(seed_shared_node_id)
            .await?
            .ok_or_else(|| FederationError::NodeNotFound(seed_shared_node_id.to_string()))?;

        // Authorize the seed node
        if !grants.is_node_authorized(
            &seed_node.repository_id.to_string(),
            seed_node.class,
            seed_node.classification,
        ) {
            // Must return identical error to absent node
            return Err(FederationError::NodeNotFound(
                seed_shared_node_id.to_string(),
            ));
        }

        let mut visited_nodes = HashSet::new();
        let mut visited_edges = HashSet::new();
        let mut queue = VecDeque::new();

        visited_nodes.insert(seed_node.shared_node_id.clone());
        queue.push_back((seed_node.shared_node_id.clone(), 0usize));

        let mut reachable_nodes = Vec::new();
        let mut reachable_edges = Vec::new();
        let mut impacted_repositories = HashSet::new();
        impacted_repositories.insert(seed_node.repository_id);

        let mut max_depth_reached = 0;

        while let Some((current_node_id, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }

            // Fetch outgoing edges from current node
            let edge_rows = sqlx::query(
                "SELECT e.shared_edge_id, e.from_shared_node_id, e.to_shared_node_id, \
                        e.from_repository_id, e.to_repository_id, e.relation, e.confidence, \
                        e.evidence_kind, e.evidence_artifact, e.class, e.classification, \
                        e.class_inputs_digest, e.revision, e.content_hash, e.computed_at \
                 FROM shared_graph_edge e \
                 WHERE e.from_shared_node_id = ?",
            )
            .bind(&current_node_id)
            .fetch_all(self.store.pool())
            .await?;

            for r in edge_rows {
                let edge_id: String = r.get("shared_edge_id");
                let to_node_id: String = r.get("to_shared_node_id");
                let from_rep_str: String = r.get("from_repository_id");
                let to_rep_str: String = r.get("to_repository_id");
                let from_repo_id: RepositoryId = from_rep_str
                    .parse()
                    .map_err(|e| FederationError::Other(format!("invalid repo id: {e}")))?;
                let to_repo_id: RepositoryId = to_rep_str
                    .parse()
                    .map_err(|e| FederationError::Other(format!("invalid repo id: {e}")))?;

                let edge_class = crate::publication::PublicationClass::from_str_lenient(
                    &r.get::<String, _>("class"),
                );
                let edge_sens_str: String = r.get("classification");
                let edge_sens = match edge_sens_str.as_str() {
                    "public" => codypendent_protocol::DataClassification::Public,
                    "internal" => codypendent_protocol::DataClassification::Internal,
                    "confidential" => codypendent_protocol::DataClassification::Confidential,
                    "secret" => codypendent_protocol::DataClassification::Secret,
                    _ => codypendent_protocol::DataClassification::Unknown,
                };

                // Check edge authorization
                if !grants.is_edge_authorized(&from_rep_str, &to_rep_str, edge_class, edge_sens) {
                    continue;
                }

                // Check target node authorization before expanding frontier
                if let Some(target_node) = self.store.get_shared_node(&to_node_id).await? {
                    if !grants.is_node_authorized(
                        &target_node.repository_id.to_string(),
                        target_node.class,
                        target_node.classification,
                    ) {
                        // Hidden intermediate node — DO NOT transit through it!
                        continue;
                    }

                    if visited_edges.insert(edge_id.clone()) {
                        let computed_str: String = r.get("computed_at");
                        let computed_at = chrono::DateTime::parse_from_rfc3339(&computed_str)
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                            .unwrap_or_else(|_| chrono::Utc::now());

                        reachable_edges.push(PublishedEdge {
                            shared_edge_id: edge_id,
                            from_shared_node_id: current_node_id.clone(),
                            to_shared_node_id: to_node_id.clone(),
                            from_repository_id: from_repo_id,
                            to_repository_id: to_repo_id,
                            relation: r.get("relation"),
                            confidence: r.get("confidence"),
                            evidence_kind: r.get("evidence_kind"),
                            evidence_artifact: r.get("evidence_artifact"),
                            class: edge_class,
                            classification: edge_sens,
                            class_inputs_digest: r.get("class_inputs_digest"),
                            revision: r.get("revision"),
                            content_hash: r.get("content_hash"),
                            computed_at,
                        });
                    }

                    if visited_nodes.insert(to_node_id.clone()) {
                        impacted_repositories.insert(target_node.repository_id);
                        reachable_nodes.push(target_node);
                        let next_depth = depth + 1;
                        if next_depth > max_depth_reached {
                            max_depth_reached = next_depth;
                        }
                        queue.push_back((to_node_id, next_depth));
                    }
                }
            }
        }

        Ok(BlastRadiusResult {
            seed_node,
            reachable_nodes,
            reachable_edges,
            impacted_repositories: impacted_repositories.into_iter().collect(),
            max_depth_reached,
        })
    }

    /// Compute an API or schema migration plan for a target symbol.
    pub async fn migration_plan(
        &self,
        target_shared_node_id: &str,
        grants: &AuthorizedGrants,
    ) -> Result<MigrationPlanResult> {
        let target_node = self
            .store
            .get_shared_node(target_shared_node_id)
            .await?
            .ok_or_else(|| FederationError::NodeNotFound(target_shared_node_id.to_string()))?;

        if !grants.is_node_authorized(
            &target_node.repository_id.to_string(),
            target_node.class,
            target_node.classification,
        ) {
            return Err(FederationError::NodeNotFound(
                target_shared_node_id.to_string(),
            ));
        }

        // Find incoming edges pointing to target_node
        let edge_rows = sqlx::query(
            "SELECT e.shared_edge_id, e.from_shared_node_id, e.to_shared_node_id, \
                    e.from_repository_id, e.to_repository_id, e.relation, e.confidence, \
                    e.evidence_kind, e.evidence_artifact, e.class, e.classification, \
                    e.class_inputs_digest, e.revision, e.content_hash, e.computed_at \
             FROM shared_graph_edge e \
             WHERE e.to_shared_node_id = ?",
        )
        .bind(target_shared_node_id)
        .fetch_all(self.store.pool())
        .await?;

        let mut referencing_nodes = Vec::new();
        let mut referencing_edges = Vec::new();
        let mut impacted_repositories = HashSet::new();

        for r in edge_rows {
            let from_rep_str: String = r.get("from_repository_id");
            let to_rep_str: String = r.get("to_repository_id");
            let from_repo_id: RepositoryId = from_rep_str
                .parse()
                .map_err(|e| FederationError::Other(format!("invalid repo id: {e}")))?;
            let to_repo_id: RepositoryId = to_rep_str
                .parse()
                .map_err(|e| FederationError::Other(format!("invalid repo id: {e}")))?;

            let edge_class = crate::publication::PublicationClass::from_str_lenient(
                &r.get::<String, _>("class"),
            );
            let edge_sens_str: String = r.get("classification");
            let edge_sens = match edge_sens_str.as_str() {
                "public" => codypendent_protocol::DataClassification::Public,
                "internal" => codypendent_protocol::DataClassification::Internal,
                "confidential" => codypendent_protocol::DataClassification::Confidential,
                "secret" => codypendent_protocol::DataClassification::Secret,
                _ => codypendent_protocol::DataClassification::Unknown,
            };

            if !grants.is_edge_authorized(&from_rep_str, &to_rep_str, edge_class, edge_sens) {
                continue;
            }

            let from_node_id: String = r.get("from_shared_node_id");
            if let Some(caller_node) = self.store.get_shared_node(&from_node_id).await? {
                if !grants.is_node_authorized(
                    &caller_node.repository_id.to_string(),
                    caller_node.class,
                    caller_node.classification,
                ) {
                    continue;
                }

                impacted_repositories.insert(caller_node.repository_id);
                referencing_nodes.push(caller_node);

                let computed_str: String = r.get("computed_at");
                let computed_at = chrono::DateTime::parse_from_rfc3339(&computed_str)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|_| chrono::Utc::now());

                referencing_edges.push(PublishedEdge {
                    shared_edge_id: r.get("shared_edge_id"),
                    from_shared_node_id: from_node_id,
                    to_shared_node_id: target_shared_node_id.to_string(),
                    from_repository_id: from_repo_id,
                    to_repository_id: to_repo_id,
                    relation: r.get("relation"),
                    confidence: r.get("confidence"),
                    evidence_kind: r.get("evidence_kind"),
                    evidence_artifact: r.get("evidence_artifact"),
                    class: edge_class,
                    classification: edge_sens,
                    class_inputs_digest: r.get("class_inputs_digest"),
                    revision: r.get("revision"),
                    content_hash: r.get("content_hash"),
                    computed_at,
                });
            }
        }

        Ok(MigrationPlanResult {
            target_node,
            referencing_nodes,
            referencing_edges,
            impacted_repositories: impacted_repositories.into_iter().collect(),
        })
    }

    /// Suggest reviewers based on authorized cross-repository facts.
    pub async fn reviewer_suggestions(
        &self,
        repository_id: &RepositoryId,
        grants: &AuthorizedGrants,
    ) -> Result<Vec<ReviewerSuggestion>> {
        if !grants.is_repo_authorized(&repository_id.to_string()) {
            return Ok(Vec::new());
        }

        let rows = sqlx::query(
            "SELECT established_by_uid, display_name FROM federated_repository_identity WHERE repository_id = ?",
        )
        .bind(repository_id.to_string())
        .fetch_all(self.store.pool())
        .await?;

        let mut suggestions = Vec::new();
        for r in rows {
            let uid: i64 = r.get("established_by_uid");
            let name: String = r.get("display_name");
            suggestions.push(ReviewerSuggestion {
                principal_uid: uid,
                repository_id: *repository_id,
                rationale: format!("Repository maintainer / identity establisher for {name}"),
            });
        }

        Ok(suggestions)
    }
}
