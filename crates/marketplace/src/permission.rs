//! Permission expansion detection, diffing, and approval receipt generation (Milestone 5).

use codypendent_sandbox::{diff_manifests, signing_digest, PermissionDiff, PluginManifest};
use uuid::Uuid;

use crate::error::MarketplaceError;
use crate::store::MarketplacePermissionReceipt;

/// Result of evaluating an update's permission changes.
#[derive(Debug, Clone)]
pub struct PermissionEvaluation {
    pub expands_permissions: bool,
    pub diff: PermissionDiff,
    pub diff_rendered: String,
    pub candidate_manifest_hash: String,
}

impl PermissionEvaluation {
    /// Evaluate permission changes from `current` to `candidate`, **manifest to
    /// manifest**.
    ///
    /// This is a preview for surfaces that hold two manifests and no installed
    /// plugin (the CLI's `plugin diff`). It is deliberately NOT the authority
    /// for an update decision: a manifest-to-manifest diff cannot see the grant
    /// the user actually approved, so it reports "identical" for an update that
    /// [`codypendent_sandbox::InstalledPlugin::diff_update`] — which diffs the
    /// *granted* set against the new *request* — correctly calls an expansion.
    /// Deciding an update from this function while the sandbox decides from the
    /// other is exactly how the two representations drift apart, so
    /// [`crate::lifecycle::MarketplaceLifecycleManager::update`] evaluates with
    /// [`Self::from_installed_diff`] instead.
    #[must_use]
    pub fn evaluate(current: &PluginManifest, candidate: &PluginManifest) -> Self {
        Self::from_installed_diff(diff_manifests(current, candidate), candidate)
    }

    /// Build an evaluation from a diff the **sandbox** computed against an
    /// installed plugin ([`codypendent_sandbox::InstalledPlugin::diff_update`]),
    /// so the receipt records the same verdict, and the same rendered text, that
    /// the sandbox will act on when the update is applied.
    #[must_use]
    pub fn from_installed_diff(diff: PermissionDiff, candidate: &PluginManifest) -> Self {
        let expands_permissions = diff.expands_permissions();
        let diff_rendered = diff.render();
        let digest = signing_digest(candidate);
        let candidate_manifest_hash = format!("sha256:{}", hex::encode(digest));

        Self {
            expands_permissions,
            diff,
            diff_rendered,
            candidate_manifest_hash,
        }
    }

    /// Create a pending permission receipt for this evaluation.
    #[must_use]
    pub fn create_receipt(
        &self,
        install_id: &str,
        from_version_id: Option<&str>,
        to_version_id: &str,
    ) -> MarketplacePermissionReceipt {
        let now = chrono::Utc::now().to_rfc3339();
        MarketplacePermissionReceipt {
            id: Uuid::now_v7().to_string(),
            install_id: install_id.to_string(),
            from_version_id: from_version_id.map(String::from),
            to_version_id: to_version_id.to_string(),
            diff_rendered: self.diff_rendered.clone(),
            expands_permissions: self.expands_permissions,
            approved_manifest_hash: self.candidate_manifest_hash.clone(),
            decision: "pending".to_string(),
            decided_by: None,
            decided_at: None,
            invalidated_at: None,
            created_at: now,
        }
    }

    /// Verify that an approved receipt matches the candidate manifest exactly.
    pub fn verify_receipt_binding(
        receipt: &MarketplacePermissionReceipt,
        candidate: &PluginManifest,
    ) -> Result<(), MarketplaceError> {
        let candidate_digest = signing_digest(candidate);
        let candidate_hash = format!("sha256:{}", hex::encode(candidate_digest));

        if receipt.approved_manifest_hash != candidate_hash {
            return Err(MarketplaceError::InvalidState(
                "candidate manifest does not match the manifest reviewed in approval receipt"
                    .into(),
            ));
        }

        if receipt.decision != "approved" {
            return Err(MarketplaceError::InvalidState(format!(
                "receipt decision is `{}` (expected `approved`)",
                receipt.decision
            )));
        }

        if receipt.invalidated_at.is_some() {
            return Err(MarketplaceError::ReceiptNotFoundOrInvalid(format!(
                "receipt {} was invalidated",
                receipt.id
            )));
        }

        Ok(())
    }
}
