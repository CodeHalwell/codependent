//! Access authorization predicates for federated graph queries.
//!
//! Enforces:
//! - Repository grants per principal.
//! - Classification & audience ceilings per repository.
//! - Access-safe traversal: unauthorized and absent nodes must be indistinguishable.

use std::collections::HashMap;

use codypendent_protocol::{DataClassification, RepositoryId};

use crate::publication::PublicationClass;

/// An access grant for a principal over a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryGrant {
    pub repository_id: RepositoryId,
    pub federated_id: String,
    pub max_class: PublicationClass,
    pub max_classification: DataClassification,
}

/// Set of authorized grants held by a principal for a query execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedGrants {
    pub principal_uid: i64,
    grants_by_repo_id: HashMap<String, RepositoryGrant>,
}

impl AuthorizedGrants {
    /// Create a new [`AuthorizedGrants`] from a list of repository grants.
    #[must_use]
    pub fn new(principal_uid: i64, grants: Vec<RepositoryGrant>) -> Self {
        let mut grants_by_repo_id = HashMap::with_capacity(grants.len());
        for grant in grants {
            grants_by_repo_id.insert(grant.repository_id.to_string(), grant);
        }
        Self {
            principal_uid,
            grants_by_repo_id,
        }
    }

    /// Grant full access to a set of repositories for a principal.
    #[must_use]
    pub fn allow_repositories(
        principal_uid: i64,
        repos: &[(RepositoryId, String, PublicationClass, DataClassification)],
    ) -> Self {
        let grants = repos
            .iter()
            .map(
                |(repo_id, fed_id, max_class, max_classification)| RepositoryGrant {
                    repository_id: *repo_id,
                    federated_id: fed_id.clone(),
                    max_class: *max_class,
                    max_classification: *max_classification,
                },
            )
            .collect();
        Self::new(principal_uid, grants)
    }

    /// Whether the repository is authorized for this principal.
    #[must_use]
    pub fn is_repo_authorized(&self, repository_id: &str) -> bool {
        self.grants_by_repo_id.contains_key(repository_id)
    }

    /// Retrieve the grant for a given repository.
    #[must_use]
    pub fn grant_for(&self, repository_id: &str) -> Option<&RepositoryGrant> {
        self.grants_by_repo_id.get(repository_id)
    }

    /// Check if a node is accessible under this principal's grants.
    #[must_use]
    pub fn is_node_authorized(
        &self,
        repository_id: &str,
        class: PublicationClass,
        classification: DataClassification,
    ) -> bool {
        if let Some(grant) = self.grants_by_repo_id.get(repository_id) {
            class.breadth() <= grant.max_class.breadth()
                && classification.allowed_off_device(grant.max_classification)
        } else {
            false
        }
    }

    /// Check if an edge is accessible under this principal's grants.
    ///
    /// Both endpoints must belong to authorized repositories, and the edge's
    /// inherited class/classification must pass both ceilings.
    #[must_use]
    pub fn is_edge_authorized(
        &self,
        from_repo_id: &str,
        to_repo_id: &str,
        class: PublicationClass,
        classification: DataClassification,
    ) -> bool {
        let from_grant = self.grants_by_repo_id.get(from_repo_id);
        let to_grant = self.grants_by_repo_id.get(to_repo_id);

        match (from_grant, to_grant) {
            (Some(fg), Some(tg)) => {
                let max_class = fg.max_class.strictest(tg.max_class);
                let allowed_class = class.breadth() <= max_class.breadth();
                let allowed_sens = classification.allowed_off_device(fg.max_classification)
                    && classification.allowed_off_device(tg.max_classification);
                allowed_class && allowed_sens
            }
            _ => false,
        }
    }
}
