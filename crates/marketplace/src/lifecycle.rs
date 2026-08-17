//! Lifecycle management for marketplace packages (Milestone 5).
//!
//! State machine:
//! Install (Disabled) -> Smoke test -> Enable (at scope) -> Disable / Revoke.
//! Updates: Evaluate permissions -> (Expansion requires Human Approval Receipt) -> Apply.
//!
//! Enforces:
//! - Package installation NEVER enables executable code automatically.
//! - Sandbox `InstalledPlugin` remains the final execution authority.
//! - Revocation is checked before enabling or executing.
//! - Permission expansion requires a fresh approval receipt bound to the reviewed manifest hash.

use std::collections::BTreeSet;

use chrono::Utc;
use codypendent_sandbox::{
    CapabilitySet, InstalledPlugin, LifecycleError, LifecycleState, TrustedPublishers,
    UiCapability, UnsignedPolicy,
};
use uuid::Uuid;

use crate::compatibility::CompatibilityChecker;
use crate::distribution::ContentAddressedStore;
use crate::error::MarketplaceError;
use crate::permission::PermissionEvaluation;
use crate::store::{
    InstallLifecycleState, MarketplaceInstall, MarketplacePackage, MarketplaceStore,
    MarketplaceVersion,
};
use crate::verify::PackageVerifier;

/// The reason recorded when an operator disables an install.
///
/// Disabling is recorded as a revocation because that is what the sandbox
/// actually does — see [`MarketplaceLifecycleManager::disable`].
pub const DISABLE_REVOCATION_REASON: &str =
    "disabled by operator (the sandbox has no reversible disable; revocation is terminal)";

/// Project the sandbox's lifecycle state — the authority — onto the database's
/// `(lifecycle, enabled_scope)` columns **as an inseparable pair**.
///
/// Every write to `marketplace_installs.lifecycle` goes through here so the row
/// can only ever assert a state the sandbox has actually reached, and so
/// `enabled_scope` is only ever non-NULL alongside `lifecycle = 'enabled'` —
/// which is what the schema's
/// `CHECK (lifecycle = 'enabled' OR enabled_scope IS NULL)` requires
/// (`migrations/0046_marketplace.sql`).
///
/// `UpdateBlocked` has no database spelling: the plugin is inert while a human
/// decides, so it projects to `installed_disabled` (nothing runs) rather than to
/// a state that implies the update landed.
fn project_state(plugin: &InstalledPlugin) -> (InstallLifecycleState, Option<&str>) {
    match plugin.state() {
        LifecycleState::Enabled => (InstallLifecycleState::Enabled, plugin.enabled_scope()),
        LifecycleState::SmokeTested => (InstallLifecycleState::SmokeTested, None),
        LifecycleState::InstalledDisabled | LifecycleState::UpdateBlocked => {
            (InstallLifecycleState::InstalledDisabled, None)
        }
        LifecycleState::Revoked => (InstallLifecycleState::Revoked, None),
    }
}

/// High-level manager coordinating the full package lifecycle.
#[derive(Debug, Clone)]
pub struct MarketplaceLifecycleManager {
    store: MarketplaceStore,
    cas: ContentAddressedStore,
    verifier: PackageVerifier,
    compatibility: CompatibilityChecker,
}

impl MarketplaceLifecycleManager {
    #[must_use]
    pub fn new(
        store: MarketplaceStore,
        cas: ContentAddressedStore,
        verifier: PackageVerifier,
        compatibility: CompatibilityChecker,
    ) -> Self {
        Self {
            store,
            cas,
            verifier,
            compatibility,
        }
    }

    #[must_use]
    pub fn store(&self) -> &MarketplaceStore {
        &self.store
    }

    #[must_use]
    pub fn cas(&self) -> &ContentAddressedStore {
        &self.cas
    }

    #[must_use]
    pub fn compatibility(&self) -> &CompatibilityChecker {
        &self.compatibility
    }

    // -------------------------------------------------------------------------
    // Install (always starts Disabled)
    // -------------------------------------------------------------------------

    /// Install a package from its manifest TOML and artifact bytes.
    ///
    /// The package is ALWAYS installed in the `installed_disabled` state.
    /// It cannot run until explicitly smoke-tested and enabled.
    #[allow(clippy::too_many_arguments)] // mirrors `InstalledPlugin::install_disabled` plus the store's provenance columns
    pub async fn install(
        &self,
        manifest_toml: &str,
        artifact: &[u8],
        source_url: &str,
        owner_uid: i64,
        trust_store: &TrustedPublishers,
        granted: Option<CapabilitySet>,
        granted_ui: Option<BTreeSet<UiCapability>>,
    ) -> Result<(InstalledPlugin, MarketplaceInstall), MarketplaceError> {
        // 1. Verify artifact checksum and signature against trust store (default-deny unsigned).
        let (manifest, verified) = self.verifier.verify(manifest_toml, artifact, trust_store)?;

        // 2. Check if publisher or package is revoked.
        if let Some(pub_record) = self.store.get_publisher(&manifest.publisher).await? {
            if let Some(revoked_at) = pub_record.revoked_at {
                return Err(MarketplaceError::RevokedPublisher {
                    publisher: manifest.publisher.clone(),
                    reason: pub_record
                        .revoked_reason
                        .unwrap_or_else(|| format!("revoked at {revoked_at}")),
                });
            }
        }

        let content_hash = manifest.security.checksum.trim().to_string();

        // 3. Safely extract artifact into content-addressed store.
        self.cas.install_artifact(&content_hash, artifact)?;

        let now = Utc::now().to_rfc3339();

        // 4. Ensure package row exists in store.
        let pkg_id = manifest.id.clone();
        if self.store.get_package(&pkg_id, true).await?.is_none() {
            let pkg = MarketplacePackage {
                id: pkg_id.clone(),
                publisher_id: manifest.publisher.clone(),
                kind: manifest.kind.as_str().to_string(),
                display_name: manifest.name.clone(),
                summary: manifest.name.clone(),
                hidden: false,
                created_at: now.clone(),
                updated_at: now.clone(),
            };
            self.store.create_package(&pkg).await?;
        }

        // 5. Ensure version row exists in store.
        let version_id = Uuid::now_v7().to_string();
        if self
            .store
            .get_version(&pkg_id, &manifest.version)
            .await?
            .is_none()
        {
            let ver = MarketplaceVersion {
                id: version_id.clone(),
                package_id: pkg_id.clone(),
                version: manifest.version.clone(),
                content_hash: content_hash.clone(),
                manifest_toml: manifest_toml.to_string(),
                signature_b64: if manifest.security.is_signed() {
                    Some(manifest.security.signature.clone())
                } else {
                    None
                },
                signed: verified.signed,
                source_url: source_url.to_string(),
                artifact_bytes: artifact.len() as i64,
                min_daemon_version: None,
                max_daemon_version: None,
                published_at: now.clone(),
                yanked_at: None,
            };
            self.store.create_version(&ver).await?;
        }

        // 7. Create sandbox InstalledPlugin (in InstalledDisabled state).
        let publisher_key = trust_store
            .key_for(&manifest.publisher)
            .map(|k| k.as_slice());
        let granted_caps =
            granted.unwrap_or_else(|| CapabilitySet::from_spec(&manifest.capabilities));
        let granted_ui_caps = granted_ui.unwrap_or_default();

        let plugin = InstalledPlugin::install_disabled(
            manifest.clone(),
            artifact,
            publisher_key,
            UnsignedPolicy::Deny,
            granted_caps,
            granted_ui_caps,
        )?;

        // 8. Record installation in the database — as the state the sandbox
        //    reached, which `install_disabled` guarantees is inert.
        let install_id = Uuid::now_v7().to_string();
        let (lifecycle, enabled_scope) = project_state(&plugin);
        let install = MarketplaceInstall {
            id: install_id,
            package_id: pkg_id,
            version_id,
            owner_uid,
            lifecycle,
            pinned: false,
            pinned_version: None,
            enabled_scope: enabled_scope.map(String::from),
            revoked_at: None,
            revoked_reason: None,
            installed_at: now.clone(),
            updated_at: now,
        };

        self.store.record_install(&install).await?;

        Ok((plugin, install))
    }

    // -------------------------------------------------------------------------
    // Smoke Test
    // -------------------------------------------------------------------------

    /// Record a passed sandbox smoke test for an installed package.
    pub async fn smoke_test(
        &self,
        plugin: &mut InstalledPlugin,
        install_id: &str,
    ) -> Result<(), MarketplaceError> {
        plugin.mark_smoke_tested()?;
        let (lifecycle, scope) = project_state(plugin);
        self.store
            .update_install_lifecycle(install_id, lifecycle, scope)
            .await?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Enable
    // -------------------------------------------------------------------------

    /// Explicitly enable a smoke-tested package at a given scope.
    pub async fn enable(
        &self,
        plugin: &mut InstalledPlugin,
        install_id: &str,
        scope: &str,
    ) -> Result<(), MarketplaceError> {
        let install = self
            .store
            .get_install_by_id(install_id)
            .await?
            .ok_or_else(|| MarketplaceError::InstallNotFound(install_id.to_string()))?;

        if install.revoked_at.is_some() {
            return Err(MarketplaceError::RevokedPackage {
                package: install.package_id,
                reason: install
                    .revoked_reason
                    .unwrap_or_else(|| "package is revoked".into()),
            });
        }

        // The sandbox decides whether this enable is legal (a passed smoke test,
        // a declared scope) and at which scope it lands; the row mirrors what it
        // reached, never what was asked for.
        plugin.enable(scope)?;
        let (lifecycle, enabled_scope) = project_state(plugin);
        self.store
            .update_install_lifecycle(install_id, lifecycle, enabled_scope)
            .await?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Disable
    // -------------------------------------------------------------------------

    /// Disable an enabled package. **This is a revocation and it is terminal.**
    ///
    /// The sandbox — the final execution authority — has no reversible disable:
    /// [`InstalledPlugin::revoke`] is its only transition to an inert state, and
    /// from `Revoked` a smoke test, an enable, and an update are all illegal
    /// transitions (`crates/sandbox/src/lifecycle.rs`). Writing
    /// `lifecycle = 'disabled'` here would therefore record a state the sandbox
    /// never reaches, and would advertise a re-enable that can never succeed:
    /// the row would say "disabled, turn it back on" while the sandbox had
    /// already retired the plugin for good.
    ///
    /// So the row records what actually happened — `revoked`, with `revoked_at`
    /// and a reason. That also makes the irreversibility enforced rather than
    /// merely documented: [`Self::enable`] refuses any install with `revoked_at`
    /// set, so a later re-enable fails loudly at the database *and* at the
    /// sandbox instead of appearing to succeed in one of them.
    ///
    /// `InstallLifecycleState::Disabled` is consequently never written by this
    /// crate. Restoring a genuinely reversible disable requires a reversible
    /// transition in the sandbox first (see the crate-level note in
    /// `crates/sandbox/src/lifecycle.rs`); until it exists, disable == revoke.
    pub async fn disable(
        &self,
        plugin: &mut InstalledPlugin,
        install_id: &str,
    ) -> Result<(), MarketplaceError> {
        self.revoke(plugin, install_id, DISABLE_REVOCATION_REASON)
            .await
    }

    // -------------------------------------------------------------------------
    // Revoke
    // -------------------------------------------------------------------------

    /// Revoke an installed package.
    pub async fn revoke(
        &self,
        plugin: &mut InstalledPlugin,
        install_id: &str,
        reason: &str,
    ) -> Result<(), MarketplaceError> {
        plugin.revoke();
        // Mirror the sandbox rather than hard-coding 'revoked': the lifecycle
        // and scope are written as the pair `project_state` derives from the
        // state the plugin actually reached.
        let (lifecycle, scope) = project_state(plugin);
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            r#"
            UPDATE marketplace_installs
            SET lifecycle = ?1,
                enabled_scope = ?2,
                revoked_at = ?3,
                revoked_reason = ?4,
                updated_at = ?3
            WHERE id = ?5
            "#,
        )
        .bind(lifecycle.as_str())
        .bind(scope)
        .bind(&now)
        .bind(reason)
        .bind(install_id)
        .execute(self.store.pool())
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(MarketplaceError::InstallNotFound(install_id.to_string()));
        }

        Ok(())
    }

    // -------------------------------------------------------------------------
    // Update & Permission Approvals
    // -------------------------------------------------------------------------

    /// Check and apply an update or block for permission approval.
    pub async fn update(
        &self,
        plugin: &mut InstalledPlugin,
        install_id: &str,
        candidate_manifest_toml: &str,
        candidate_artifact: &[u8],
        trust_store: &TrustedPublishers,
    ) -> Result<Option<String>, MarketplaceError> {
        let install = self
            .store
            .get_install_by_id(install_id)
            .await?
            .ok_or_else(|| MarketplaceError::InstallNotFound(install_id.to_string()))?;

        let (candidate_manifest, candidate_verified) =
            self.verifier
                .verify(candidate_manifest_toml, candidate_artifact, trust_store)?;

        let now = Utc::now().to_rfc3339();
        let content_hash = candidate_manifest.security.checksum.trim().to_string();

        // Safely extract candidate artifact
        self.cas
            .install_artifact(&content_hash, candidate_artifact)?;

        // Ensure candidate version row exists
        let candidate_version_id = match self
            .store
            .get_version(&candidate_manifest.id, &candidate_manifest.version)
            .await?
        {
            Some(v) => v.id,
            None => {
                let v_id = Uuid::now_v7().to_string();
                let ver = MarketplaceVersion {
                    id: v_id.clone(),
                    package_id: candidate_manifest.id.clone(),
                    version: candidate_manifest.version.clone(),
                    content_hash: content_hash.clone(),
                    manifest_toml: candidate_manifest_toml.to_string(),
                    signature_b64: if candidate_manifest.security.is_signed() {
                        Some(candidate_manifest.security.signature.clone())
                    } else {
                        None
                    },
                    signed: candidate_verified.signed,
                    source_url: String::new(),
                    artifact_bytes: candidate_artifact.len() as i64,
                    min_daemon_version: None,
                    max_daemon_version: None,
                    published_at: now.clone(),
                    yanked_at: None,
                };
                self.store.create_version(&ver).await?;
                v_id
            }
        };

        // Ask the SANDBOX whether this update expands authority — do not decide
        // it here. A manifest-to-manifest diff cannot see the grant the user
        // approved, so it calls an update "identical" that `plugin.update()`
        // will refuse as an expansion (a capability withheld at install is
        // exactly that case). Deciding it independently meant the "safe" path
        // could hand the sandbox an update it then blocked: the plugin was left
        // `UpdateBlocked` — inert, and unable to update or be approved ever
        // again — while the database still read `enabled` at the old version
        // with no pending receipt for anyone to decide.
        let eval = PermissionEvaluation::from_installed_diff(
            plugin.diff_update(&candidate_manifest),
            &candidate_manifest,
        );

        if eval.expands_permissions {
            // Permission expansion detected — create receipt and block update.
            let receipt =
                eval.create_receipt(install_id, Some(&install.version_id), &candidate_version_id);
            let receipt_id = receipt.id.clone();
            self.store.create_permission_receipt(&receipt).await?;

            return Err(MarketplaceError::UpdateExpandsPermissions {
                diff: eval.diff_rendered,
                receipt: receipt_id,
            });
        }

        // Safe update (no expansion) — apply directly
        let publisher_key = trust_store
            .key_for(&candidate_manifest.publisher)
            .map(|k| k.as_slice());

        let _diff = plugin.update(
            candidate_manifest,
            candidate_artifact,
            publisher_key,
            UnsignedPolicy::Deny,
        )?;

        // Update install row to the new version, and re-project the lifecycle:
        // applying an update is not lifecycle-neutral in the sandbox. An enabled
        // plugin stays enabled, but any other state is reset to
        // `InstalledDisabled` (`apply_manifest`), so a smoke-tested install that
        // takes a safe update must stop claiming `smoke_tested` here — otherwise
        // the row would offer an enable the sandbox would refuse.
        let (lifecycle, scope) = project_state(plugin);
        let result = sqlx::query(
            r#"
            UPDATE marketplace_installs
            SET version_id = ?1, lifecycle = ?2, enabled_scope = ?3, updated_at = ?4
            WHERE id = ?5
            "#,
        )
        .bind(&candidate_version_id)
        .bind(lifecycle.as_str())
        .bind(scope)
        .bind(&now)
        .bind(install_id)
        .execute(self.store.pool())
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(MarketplaceError::InstallNotFound(install_id.to_string()));
        }

        Ok(None)
    }

    /// Approve a pending update receipt.
    #[allow(clippy::too_many_arguments)] // the approval is bound to every input it was reviewed against
    pub async fn approve_update(
        &self,
        plugin: &mut InstalledPlugin,
        install_id: &str,
        receipt_id: &str,
        candidate_manifest_toml: &str,
        candidate_artifact: &[u8],
        decided_by: &str,
        trust_store: &TrustedPublishers,
    ) -> Result<(), MarketplaceError> {
        let receipt =
            self.store.get_receipt(receipt_id).await?.ok_or_else(|| {
                MarketplaceError::ReceiptNotFoundOrInvalid(receipt_id.to_string())
            })?;

        if receipt.invalidated_at.is_some() {
            return Err(MarketplaceError::ReceiptNotFoundOrInvalid(format!(
                "receipt {receipt_id} is invalidated"
            )));
        }

        let (candidate_manifest, _) =
            self.verifier
                .verify(candidate_manifest_toml, candidate_artifact, trust_store)?;

        // Verify receipt bindings
        let candidate_hash = format!(
            "sha256:{}",
            hex::encode(codypendent_sandbox::signing_digest(&candidate_manifest))
        );
        if receipt.approved_manifest_hash != candidate_hash {
            return Err(MarketplaceError::InvalidState(
                "candidate manifest does not match reviewed receipt".into(),
            ));
        }

        let publisher_key = trust_store
            .key_for(&candidate_manifest.publisher)
            .map(|k| k.as_slice());

        // Re-verify and apply through the sandbox, which stays the final execution
        // authority. An expanding update parks the plugin in `UpdateBlocked` and
        // mints its own one-shot approval receipt; spend that receipt immediately,
        // now that the human decision has been recorded and bound to this exact
        // manifest hash. Any other lifecycle refusal aborts the approval — the
        // receipt must not be marked approved, and the install row must not move,
        // if the sandbox refused the candidate.
        match plugin.update(
            candidate_manifest.clone(),
            candidate_artifact,
            publisher_key,
            UnsignedPolicy::Deny,
        ) {
            Ok(_) => {}
            Err(LifecycleError::UpdateExpandsPermissions {
                approval_receipt, ..
            }) => {
                plugin.approve_update(
                    &approval_receipt,
                    candidate_manifest,
                    candidate_artifact,
                    publisher_key,
                    UnsignedPolicy::Deny,
                )?;
            }
            Err(err) => return Err(err.into()),
        }

        // Mark receipt decided in database
        self.store
            .decide_receipt(receipt_id, "approved", Some(decided_by))
            .await?;

        let now = Utc::now().to_rfc3339();

        // Mirror the sandbox's post-approval state. `enabled_scope` is only ever
        // written alongside `lifecycle = 'enabled'`, which is what the schema's
        // CHECK constraint requires.
        let (lifecycle, scope) = project_state(plugin);

        // Update install row
        let result = sqlx::query(
            r#"
            UPDATE marketplace_installs
            SET version_id = ?1, lifecycle = ?2, enabled_scope = ?3, updated_at = ?4
            WHERE id = ?5
            "#,
        )
        .bind(&receipt.to_version_id)
        .bind(lifecycle.as_str())
        .bind(scope)
        .bind(&now)
        .bind(install_id)
        .execute(self.store.pool())
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(MarketplaceError::InstallNotFound(install_id.to_string()));
        }

        Ok(())
    }
}
