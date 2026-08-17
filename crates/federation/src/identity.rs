//! Federated repository identity.
//!
//! `stable_repository_id` (`crates/knowledge/src/codegraph.rs:174`) is derived from
//! the canonical local path and serves as a local partition key. Federation mints
//! its own global identity deterministic across checkout paths and machines:
//! `SHA-256(root_commit || '\n' || normalized_remote)`.

use chrono::{DateTime, Utc};
use codypendent_protocol::RepositoryId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Normalizes a remote Git URL for deterministic federated identification.
///
/// Strips:
/// - Transport schemes (`https://`, `http://`, `ssh://`, `git://`, `file://`)
/// - User credentials (`user:pass@`, `git@`)
/// - SCP separator `:` (`github.com:org/repo` -> `github.com/org/repo`)
/// - Suffixes (`.git`, `.git/`, trailing `/`)
/// - Lowercases host and repository path.
///
/// Returns `None` if the input is empty or contains no non-whitespace characters.
#[must_use]
pub fn normalize_remote(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut url = trimmed;

    // Strip scheme if present
    for scheme in &["https://", "http://", "ssh://", "git://", "file://"] {
        if let Some(rest) = url.strip_prefix(scheme) {
            url = rest;
            break;
        }
    }

    // Strip userinfo / credentials (e.g. `user:pass@` or `git@`)
    if let Some(idx) = url.find('@') {
        url = &url[idx + 1..];
    }

    // Handle SCP-style git URLs: `github.com:org/repo` -> `github.com/org/repo`
    let mut normalized = String::with_capacity(url.len());
    let mut seen_slash = false;

    for ch in url.chars() {
        if ch == '/' {
            seen_slash = true;
            normalized.push('/');
        } else if ch == ':' && !seen_slash {
            // Replace host:path colon with slash
            normalized.push('/');
            seen_slash = true;
        } else {
            normalized.push(ch);
        }
    }

    // Strip leading slashes
    let mut cleaned = normalized.trim_start_matches('/').to_string();

    // Strip trailing `.git/` or `.git` or `/`
    while cleaned.ends_with('/') || cleaned.ends_with(".git") {
        if let Some(stripped) = cleaned.strip_suffix(".git/") {
            cleaned = stripped.to_string();
        } else if let Some(stripped) = cleaned.strip_suffix(".git") {
            cleaned = stripped.to_string();
        } else if let Some(stripped) = cleaned.strip_suffix('/') {
            cleaned = stripped.to_string();
        }
    }

    let result = cleaned.to_ascii_lowercase();
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Derives the 64-character hex federated repository ID.
///
/// `SHA-256(root_commit || '\n' || normalized_remote)`
#[must_use]
pub fn derive_federated_id(root_commit: &str, normalized_remote: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(root_commit.as_bytes());
    hasher.update(b"\n");
    if let Some(remote) = normalized_remote {
        hasher.update(remote.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Durable federated identity of a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FederatedRepositoryIdentity {
    /// Local path-derived RepositoryId (join key to code_nodes.repository).
    pub repository_id: RepositoryId,
    /// Cross-machine deterministic SHA-256 hex string.
    pub federated_id: String,
    /// First root commit of the repository history.
    pub root_commit: String,
    /// Canonical normalized remote URL, or `None` for local-only repositories.
    pub normalized_remote: Option<String>,
    /// Operator-visible label.
    pub display_name: String,
    /// Timestamp when this identity was established.
    pub established_at: DateTime<Utc>,
    /// Kernel-derived uid of the principal establishing the identity.
    pub established_by_uid: i64,
}

impl FederatedRepositoryIdentity {
    /// Create a new [`FederatedRepositoryIdentity`].
    pub fn new(
        repository_id: RepositoryId,
        root_commit: impl Into<String>,
        raw_remote: Option<&str>,
        display_name: impl Into<String>,
        established_by_uid: i64,
    ) -> Self {
        let root = root_commit.into();
        let normalized = raw_remote.and_then(normalize_remote);
        let federated_id = derive_federated_id(&root, normalized.as_deref());
        Self {
            repository_id,
            federated_id,
            root_commit: root,
            normalized_remote: normalized,
            display_name: display_name.into(),
            established_at: Utc::now(),
            established_by_uid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_normalize_remote_variations() {
        assert_eq!(
            normalize_remote("https://github.com/CodeHalwell/codypendent.git"),
            Some("github.com/codehalwell/codypendent".to_string())
        );
        assert_eq!(
            normalize_remote("git@github.com:CodeHalwell/codypendent.git"),
            Some("github.com/codehalwell/codypendent".to_string())
        );
        assert_eq!(
            normalize_remote("ssh://git@gitlab.com/group/subgroup/project.git/"),
            Some("gitlab.com/group/subgroup/project".to_string())
        );
        assert_eq!(
            normalize_remote("https://user:password@github.com/org/repo/"),
            Some("github.com/org/repo".to_string())
        );
        assert_eq!(normalize_remote("   "), None);
        assert_eq!(normalize_remote(""), None);
    }

    #[test]
    fn federated_id_is_stable_across_checkout_paths() {
        let root_commit = "7e2a9b3d1f4c5e6a7b8c9d0e1f2a3b4c5d6e7f8a";
        let remote = "https://github.com/example/repo.git";

        let path_a = Path::new("/Users/developer/work/repo");
        let path_b = Path::new("/home/runner/workspace/repo");

        // Local repository ids differ across paths
        let local_id_a = codypendent_knowledge::codegraph::stable_repository_id(path_a);
        let local_id_b = codypendent_knowledge::codegraph::stable_repository_id(path_b);
        assert_ne!(local_id_a, local_id_b);

        // Federated identities match exactly across paths
        let identity_a =
            FederatedRepositoryIdentity::new(local_id_a, root_commit, Some(remote), "Repo A", 1000);
        let identity_b =
            FederatedRepositoryIdentity::new(local_id_b, root_commit, Some(remote), "Repo B", 1000);

        assert_eq!(identity_a.federated_id, identity_b.federated_id);
        assert_eq!(identity_a.federated_id.len(), 64);
    }

    #[test]
    fn null_remote_derives_predictable_hash() {
        let root_commit = "abcdef1234567890";
        let fed_id = derive_federated_id(root_commit, None);
        let mut hasher = Sha256::new();
        hasher.update(b"abcdef1234567890\n");
        let expected = hex::encode(hasher.finalize());
        assert_eq!(fed_id, expected);
    }
}
