//! The plugin lifecycle (STEP 6.1).
//!
//! Chapter 05's lifecycle, enforced as a state machine rather than a convention:
//!
//! ```text
//! discover → inspect manifest → verify signature/checksum → evaluate permissions
//! → install disabled → sandbox smoke test → user enables at scope → monitor
//! → update with permission diff → revoke / remove
//! ```
//!
//! A plugin is **installed disabled** — it can do nothing until a human enables it
//! at a chosen scope. An **update** recomputes the permission diff against the
//! installed grant: an expansion is refused until re-approved (exit criterion 2).
//! Each installed plugin carries the trust record Chapter 05 requires (publisher,
//! content hash, signature status, requested capabilities, trust tier, installed
//! scope, revocation status), so retrieval and audit read trust *facts*, never the
//! plugin's self-description.

use std::collections::BTreeSet;

use crate::manifest::{PluginManifest, UiCapability};
use crate::permission::{
    diff_resources, include_ui_permissions, CapabilitySet, PermissionDiff, UiPermission,
};
use crate::verify::{
    checksum_of, signing_digest, verify_artifact, UnsignedPolicy, Verified, VerifyError,
};

/// The trust tier a plugin's items enter the registry at. Semantic relevance
/// never lifts this tier (Chapter 05 / the Phase 2 hard filter): an untrusted
/// plugin's tools are retrievable only where policy admits untrusted content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustTier {
    /// Signed by a trusted publisher.
    Trusted,
    /// Checksum-verified but unsigned (allowed only by explicit policy).
    Unsigned,
}

/// Where in its lifecycle a plugin sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    /// Verified + permission-evaluated, installed but inert. Nothing runs.
    InstalledDisabled,
    /// The sandbox smoke test passed (start, handshake, list tools, stop).
    SmokeTested,
    /// Enabled by a human at a chosen scope; the plugin's tools are live.
    Enabled,
    /// Update blocked: the new manifest expands permissions, awaiting re-approval.
    UpdateBlocked,
    /// Revoked / removed; the plugin is inert and its items deregistered.
    Revoked,
}

/// A lifecycle transition failure.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum LifecycleError {
    #[error("verification failed: {0}")]
    Verify(#[from] VerifyError),
    #[error("cannot {action} a plugin in state {state:?}")]
    IllegalTransition {
        action: &'static str,
        state: LifecycleState,
    },
    #[error(
        "update blocked: it expands permissions and must be re-approved with receipt {approval_receipt}:\n{diff}"
    )]
    UpdateExpandsPermissions {
        diff: String,
        approval_receipt: String,
    },
    #[error("plugin id/kind changed on update (was {old}, now {new}); reinstall required")]
    IdentityChanged { old: String, new: String },
    /// A granted capability is not one the manifest requested. A grant may only
    /// *narrow* the manifest's request, never widen it — otherwise a caller could
    /// smuggle an undeclared capability past the manifest into the sandbox profile
    /// (exit criterion 1).
    #[error("granted capability not requested by the manifest: {capability}")]
    GrantExceedsManifest { capability: String },
    #[error("granted UI capability not requested by the manifest: {capability}")]
    UiGrantExceedsManifest { capability: String },
    #[error("plugin cannot be enabled at undeclared scope `{scope}`")]
    UndeclaredScope { scope: String },
    #[error("there is no pending update to approve")]
    NoPendingUpdate,
    #[error("the update approval receipt is invalid or has already been consumed")]
    ApprovalReceiptMismatch,
    #[error("the candidate update does not exactly match the update that was reviewed")]
    ApprovedCandidateMismatch,
}

/// The immutable review record for an update which expands authority.
///
/// The receipt is deliberately one-shot. The remaining binding fields are kept
/// private so callers cannot rewrite what a user reviewed between the decision
/// surface and [`InstalledPlugin::approve_update`].
#[derive(Debug)]
pub struct PendingUpdateApproval {
    /// Opaque token which the approval surface must return.
    approval_receipt: String,
    /// Exact permission/resource/UI delta presented to the user.
    permission_diff: PermissionDiff,
    candidate: PluginManifest,
    signing_digest: [u8; 32],
    artifact_checksum: String,
    publisher_key_checksum: Option<String>,
    signed: bool,
    previous_state: LifecycleState,
}

impl PendingUpdateApproval {
    #[must_use]
    pub fn approval_receipt(&self) -> &str {
        &self.approval_receipt
    }

    #[must_use]
    pub fn permission_diff(&self) -> &PermissionDiff {
        &self.permission_diff
    }
}

/// An installed plugin and its trust record. The `state` is the lifecycle
/// position; `granted` is the capability set the user approved (the profile is
/// derived from this, not from the manifest's request).
#[derive(Debug)]
pub struct InstalledPlugin {
    manifest: PluginManifest,
    state: LifecycleState,
    trust: TrustTier,
    /// The checksum the artifact verified against (content hash, for the record).
    content_hash: String,
    /// Whether a real publisher signature verified.
    signed: bool,
    /// The capabilities the user granted (may narrow the manifest's request).
    granted: CapabilitySet,
    /// Host-facing UI authority the user explicitly approved. This is distinct
    /// from requested manifest capabilities and is what worker negotiation and
    /// document-action validation must consume.
    granted_ui: BTreeSet<UiCapability>,
    /// The scope the user enabled the plugin at (`None` until enabled).
    enabled_scope: Option<String>,
    /// Sealed candidate awaiting a human decision. Only lifecycle methods may
    /// create or consume it; public fields expose the receipt and reviewed diff.
    pending_update: Option<PendingUpdateApproval>,
}

impl InstalledPlugin {
    #[must_use]
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    #[must_use]
    pub fn state(&self) -> LifecycleState {
        self.state
    }

    #[must_use]
    pub fn trust(&self) -> TrustTier {
        self.trust
    }

    #[must_use]
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }

    #[must_use]
    pub fn is_signed(&self) -> bool {
        self.signed
    }

    #[must_use]
    pub fn granted(&self) -> &CapabilitySet {
        &self.granted
    }

    #[must_use]
    pub fn granted_ui_capabilities(&self) -> &BTreeSet<UiCapability> {
        &self.granted_ui
    }

    #[must_use]
    pub fn enabled_scope(&self) -> Option<&str> {
        self.enabled_scope.as_deref()
    }

    #[must_use]
    pub fn pending_update(&self) -> Option<&PendingUpdateApproval> {
        self.pending_update.as_ref()
    }

    /// **Install (disabled).** Verify the artifact, evaluate permissions, and
    /// record trust — but do not run anything. The plugin is inert until enabled.
    ///
    /// `granted` is the capability set the user approves at install; passing the
    /// manifest's full set grants everything requested, a subset withholds
    /// capabilities. Verification (checksum, signature/unsigned policy) happens
    /// here, before the plugin exists on disk in an installed state.
    pub fn install_disabled(
        manifest: PluginManifest,
        artifact: &[u8],
        publisher_key: Option<&[u8]>,
        unsigned: UnsignedPolicy,
        granted: CapabilitySet,
        granted_ui: BTreeSet<UiCapability>,
    ) -> Result<Self, LifecycleError> {
        let Verified { signed } = verify_artifact(&manifest, artifact, publisher_key, unsigned)?;
        // A grant may only narrow the manifest — reject any granted capability the
        // manifest did not request, so an undeclared capability can never reach the
        // sandbox profile through an over-broad grant.
        assert_granted_within_manifest(&manifest, &granted)?;
        assert_ui_granted_within_manifest(&manifest, &granted_ui)?;
        let trust = if signed {
            TrustTier::Trusted
        } else {
            TrustTier::Unsigned
        };
        Ok(Self {
            content_hash: manifest.security.checksum.trim().to_string(),
            manifest,
            state: LifecycleState::InstalledDisabled,
            trust,
            signed,
            granted,
            granted_ui,
            enabled_scope: None,
            pending_update: None,
        })
    }

    /// **Sandbox smoke test.** In production this starts the plugin inside the
    /// sandbox, handshakes, lists its tools, and stops it. Here it records the
    /// transition — the executor supplies the real round-trip result. Only an
    /// installed-disabled plugin can be smoke-tested.
    pub fn mark_smoke_tested(&mut self) -> Result<(), LifecycleError> {
        match self.state {
            LifecycleState::InstalledDisabled => {
                self.state = LifecycleState::SmokeTested;
                Ok(())
            }
            state => Err(LifecycleError::IllegalTransition {
                action: "smoke-test",
                state,
            }),
        }
    }

    /// **Enable at a scope.** A human turns the plugin on; its tools go live at
    /// the chosen scope. Requires a passed smoke test.
    pub fn enable(&mut self, scope: impl Into<String>) -> Result<(), LifecycleError> {
        let scope = scope.into();
        if scope.trim().is_empty() || !self.manifest.scopes.iter().any(|allowed| allowed == &scope)
        {
            return Err(LifecycleError::UndeclaredScope { scope });
        }
        match self.state {
            LifecycleState::SmokeTested => {
                self.state = LifecycleState::Enabled;
                self.enabled_scope = Some(scope);
                Ok(())
            }
            state => Err(LifecycleError::IllegalTransition {
                action: "enable",
                state,
            }),
        }
    }

    /// **Revoke / remove.** The plugin becomes inert and its items are
    /// deregistered. Legal from any state.
    pub fn revoke(&mut self) {
        self.state = LifecycleState::Revoked;
        self.enabled_scope = None;
        self.pending_update = None;
    }

    /// Compute the permission diff a candidate update would introduce, without
    /// applying it. The TUI renders this to the user at the decision point.
    /// Folds in resource-cap changes (P6-A) alongside the capability diff — a
    /// raised memory/cpu/wall/output cap is exactly as much an expansion as an
    /// added capability, and must be visible in the same diff or it would
    /// auto-apply unreviewed.
    #[must_use]
    pub fn diff_update(&self, next: &PluginManifest) -> PermissionDiff {
        let next_set = CapabilitySet::from_spec(&next.capabilities);
        let mut diff = self.granted.diff_to(&next_set);
        diff.resource_changes = diff_resources(&self.manifest.resources, &next.resources);
        include_ui_permissions(&mut diff, &self.manifest, next);
        // Requested UI APIs are not grants. Replace the manifest-to-manifest
        // capability portion with approved-grant-to-next-request so a withheld
        // command capability cannot become active on a "safe" update.
        diff.ui_added
            .retain(|permission| !matches!(permission, UiPermission::Capability(_)));
        diff.ui_removed
            .retain(|permission| !matches!(permission, UiPermission::Capability(_)));
        let next_ui = requested_ui_capabilities(next);
        diff.ui_added.extend(
            next_ui
                .difference(&self.granted_ui)
                .copied()
                .map(UiPermission::Capability),
        );
        diff.ui_removed.extend(
            self.granted_ui
                .difference(&next_ui)
                .copied()
                .map(UiPermission::Capability),
        );
        diff.ui_added.sort();
        diff.ui_removed.sort();
        diff
    }

    /// **Update.** Verify the new artifact and apply the update **only if it does
    /// not expand permissions** (exit criterion 2). An expansion returns
    /// [`LifecycleError::UpdateExpandsPermissions`] carrying the rendered diff and
    /// leaves the plugin `UpdateBlocked` — the operator must call
    /// [`Self::approve_update`] to accept the expanded grant.
    ///
    /// A permission-identical update (or one that only *narrows*) applies
    /// automatically. The plugin id and kind must not change — that is a new
    /// plugin, not an update.
    pub fn update(
        &mut self,
        next: PluginManifest,
        artifact: &[u8],
        publisher_key: Option<&[u8]>,
        unsigned: UnsignedPolicy,
    ) -> Result<PermissionDiff, LifecycleError> {
        if matches!(
            self.state,
            LifecycleState::UpdateBlocked | LifecycleState::Revoked
        ) {
            return Err(LifecycleError::IllegalTransition {
                action: "update",
                state: self.state,
            });
        }
        if next.id != self.manifest.id {
            return Err(LifecycleError::IdentityChanged {
                old: self.manifest.id.clone(),
                new: next.id,
            });
        }
        if next.kind != self.manifest.kind {
            return Err(LifecycleError::IdentityChanged {
                old: self.manifest.kind.as_str().to_string(),
                new: next.kind.as_str().to_string(),
            });
        }
        let Verified { signed } = verify_artifact(&next, artifact, publisher_key, unsigned)?;
        let diff = self.diff_update(&next);
        if diff.expands_permissions() {
            // Seal every input to the decision. Approval must present this exact
            // manifest and artifact under the same publisher identity, plus the
            // one-shot receipt. A later same-id candidate cannot ride an earlier
            // approval.
            let approval_receipt = uuid::Uuid::now_v7().to_string();
            self.pending_update = Some(PendingUpdateApproval {
                approval_receipt: approval_receipt.clone(),
                permission_diff: diff.clone(),
                signing_digest: signing_digest(&next),
                artifact_checksum: checksum_of(artifact),
                publisher_key_checksum: publisher_key.map(checksum_of),
                candidate: next,
                signed,
                previous_state: self.state,
            });
            self.state = LifecycleState::UpdateBlocked;
            return Err(LifecycleError::UpdateExpandsPermissions {
                diff: diff.render(),
                approval_receipt,
            });
        }
        // Identical or narrowing: apply. The granted set follows the new manifest
        // intersected down to what it declares (a narrowing update drops grants).
        let next_set = CapabilitySet::from_spec(&next.capabilities);
        let next_ui = requested_ui_capabilities(&next);
        let narrowed_ui = self.granted_ui.intersection(&next_ui).copied().collect();
        self.apply_manifest(next, next_set, narrowed_ui, signed)?;
        self.pending_update = None;
        Ok(diff)
    }

    /// **Approve a blocked update.** The human accepts the expanded permissions
    /// from a prior [`Self::update`] that returned
    /// [`LifecycleError::UpdateExpandsPermissions`]. Re-verifies and applies the
    /// new manifest with the (now approved) expanded grant.
    pub fn approve_update(
        &mut self,
        approval_receipt: &str,
        next: PluginManifest,
        artifact: &[u8],
        publisher_key: Option<&[u8]>,
        unsigned: UnsignedPolicy,
    ) -> Result<(), LifecycleError> {
        if self.state != LifecycleState::UpdateBlocked {
            return Err(LifecycleError::NoPendingUpdate);
        }
        let pending = self
            .pending_update
            .as_ref()
            .ok_or(LifecycleError::NoPendingUpdate)?;
        if pending.approval_receipt != approval_receipt {
            return Err(LifecycleError::ApprovalReceiptMismatch);
        }
        if next.id != self.manifest.id || next.kind != self.manifest.kind {
            return Err(LifecycleError::IdentityChanged {
                old: self.manifest.id.clone(),
                new: next.id,
            });
        }
        let Verified { signed } = verify_artifact(&next, artifact, publisher_key, unsigned)?;
        let diff = self.diff_update(&next);
        if next != pending.candidate
            || signing_digest(&next) != pending.signing_digest
            || checksum_of(artifact) != pending.artifact_checksum
            || publisher_key.map(checksum_of) != pending.publisher_key_checksum
            || signed != pending.signed
            || diff != pending.permission_diff
        {
            return Err(LifecycleError::ApprovedCandidateMismatch);
        }
        let previous_state = pending.previous_state;
        let next_set = CapabilitySet::from_spec(&next.capabilities);
        let next_ui = requested_ui_capabilities(&next);
        // Restore the pre-review lifecycle state before applying so an enabled
        // plugin remains enabled after the user accepts the expansion.
        self.state = previous_state;
        self.apply_manifest(next, next_set, next_ui, signed)?;
        self.pending_update = None;
        Ok(())
    }

    /// Reject the currently pending update and restore the old installation's
    /// lifecycle state. The receipt is consumed and can never approve a later
    /// candidate.
    pub fn reject_pending_update(&mut self, approval_receipt: &str) -> Result<(), LifecycleError> {
        if self.state != LifecycleState::UpdateBlocked {
            return Err(LifecycleError::NoPendingUpdate);
        }
        let pending = self
            .pending_update
            .as_ref()
            .ok_or(LifecycleError::NoPendingUpdate)?;
        if pending.approval_receipt != approval_receipt {
            return Err(LifecycleError::ApprovalReceiptMismatch);
        }
        self.state = pending.previous_state;
        self.pending_update = None;
        Ok(())
    }

    /// Apply a verified manifest as the plugin's new installed state. **Every**
    /// path that installs or updates a grant funnels through here (P6-C), which
    /// re-asserts the "granted ⊆ manifest-requested" subset invariant
    /// structurally — not merely as a side effect of how `update`/
    /// `approve_update` happen to compute their `granted` argument today. That
    /// makes the guard robust to a future refactor that starts handing this
    /// function an externally-supplied (and possibly over-broad) grant.
    fn apply_manifest(
        &mut self,
        next: PluginManifest,
        granted: CapabilitySet,
        granted_ui: BTreeSet<UiCapability>,
        signed: bool,
    ) -> Result<(), LifecycleError> {
        assert_granted_within_manifest(&next, &granted)?;
        assert_ui_granted_within_manifest(&next, &granted_ui)?;
        self.content_hash = next.security.checksum.trim().to_string();
        self.manifest = next;
        self.granted = granted;
        self.granted_ui = granted_ui;
        self.signed = signed;
        self.trust = if signed {
            TrustTier::Trusted
        } else {
            TrustTier::Unsigned
        };
        // An update leaves the plugin enabled if it was enabled (a permission-safe
        // update is transparent); otherwise it returns to installed-disabled.
        if self.state != LifecycleState::Enabled {
            self.state = LifecycleState::InstalledDisabled;
        }
        Ok(())
    }

    /// Whether the plugin's tools are currently live (enabled and not revoked).
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.state == LifecycleState::Enabled
    }
}

/// The shared guard (P6-C): assert that every capability in `granted` is one
/// `manifest` actually requests — a grant may only *narrow* the manifest,
/// never widen it, on **every** path that installs or updates a grant. Before
/// this existed, the invariant held only because `update`/`approve_update`
/// happened to derive their `granted` argument directly from the manifest
/// itself (so it was trivially a subset); calling this from every such path —
/// [`InstalledPlugin::install_disabled`] and [`InstalledPlugin::apply_manifest`]
/// — makes the guarantee structural instead of an accident of today's call
/// sites, so a future refactor that starts accepting a caller-supplied grant on
/// update cannot silently reopen the hole (exit criterion 1: an undeclared
/// capability can never reach the sandbox profile through an over-broad grant).
fn assert_granted_within_manifest(
    manifest: &PluginManifest,
    granted: &CapabilitySet,
) -> Result<(), LifecycleError> {
    let requested = CapabilitySet::from_spec(&manifest.capabilities);
    for cap in granted.iter() {
        if !requested.grants(cap) {
            return Err(LifecycleError::GrantExceedsManifest {
                capability: cap.to_string(),
            });
        }
    }
    Ok(())
}

fn requested_ui_capabilities(manifest: &PluginManifest) -> BTreeSet<UiCapability> {
    manifest
        .ui
        .as_ref()
        .map(|ui| ui.requested_capabilities.iter().copied().collect())
        .unwrap_or_default()
}

fn assert_ui_granted_within_manifest(
    manifest: &PluginManifest,
    granted: &BTreeSet<UiCapability>,
) -> Result<(), LifecycleError> {
    let requested = requested_ui_capabilities(manifest);
    for capability in granted {
        if !requested.contains(capability) {
            return Err(LifecycleError::UiGrantExceedsManifest {
                capability: capability.as_str().to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::parse_manifest;
    use crate::verify::checksum_of;

    fn manifest(version: &str, network: &[&str]) -> (PluginManifest, Vec<u8>) {
        let artifact = format!("plugin bytes {version}").into_bytes();
        let net = network
            .iter()
            .map(|h| format!("\"{h}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let toml = format!(
            r#"
schema_version = 1
id = "github"
name = "GitHub"
version = "{version}"
kind = "native-process"
publisher = "codypendent-project"
scopes = ["repository"]
[runtime]
command = "codypendent-plugin-github"
[capabilities]
network = [{net}]
[security]
checksum = "{}"
signature = "set-during-packaging"
"#,
            checksum_of(&artifact)
        );
        (parse_manifest(&toml).unwrap(), artifact)
    }

    fn ui_manifest(version: &str, requested_capabilities: &[&str]) -> (PluginManifest, Vec<u8>) {
        let artifact = format!("UI plugin bytes {version}").into_bytes();
        let capabilities = requested_capabilities
            .iter()
            .map(|capability| format!("\"{capability}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let toml = format!(
            r#"
schema_version = 1
id = "ui-test"
name = "UI Test"
version = "{version}"
kind = "ui-component"
publisher = "codypendent-project"
scopes = ["repository"]
[ui]
schema_version = 1
requested_capabilities = [{capabilities}]
[ui.compatibility]
protocol = ">=1.0,<2.0"
sdk = "^1.0"
[ui.entrypoints]
shared = "dist/shared.js"
[[ui.contributions]]
id = "test.report"
point = "artifact-renderer"
renderer = "test.Report"
targets = ["shared"]
[security]
checksum = "{}"
signature = "set-during-packaging"
"#,
            checksum_of(&artifact)
        );
        (parse_manifest(&toml).expect("UI manifest parses"), artifact)
    }

    fn install(version: &str, network: &[&str]) -> InstalledPlugin {
        let (m, artifact) = manifest(version, network);
        let granted = CapabilitySet::from_spec(&m.capabilities);
        InstalledPlugin::install_disabled(
            m,
            &artifact,
            None,
            UnsignedPolicy::Allow,
            granted,
            BTreeSet::new(),
        )
        .expect("installs")
    }

    #[test]
    fn installs_disabled_and_inert() {
        let p = install("0.1.0", &["api.github.com:443"]);
        assert_eq!(p.state, LifecycleState::InstalledDisabled);
        assert!(!p.is_active(), "a freshly installed plugin does nothing");
        assert_eq!(p.trust, TrustTier::Unsigned);
    }

    #[test]
    fn a_narrowing_grant_is_accepted() {
        // The manifest requests api.github.com; the user withholds it (grants
        // nothing). A narrowing grant is fine.
        let (m, artifact) = manifest("0.1.0", &["api.github.com:443"]);
        let narrowed = CapabilitySet::default();
        let p = InstalledPlugin::install_disabled(
            m,
            &artifact,
            None,
            UnsignedPolicy::Allow,
            narrowed,
            BTreeSet::new(),
        )
        .expect("a narrowing grant installs");
        assert!(p.granted.is_empty());
    }

    #[test]
    fn a_grant_the_manifest_did_not_request_is_rejected() {
        // The manifest requests only api.github.com; a caller tries to grant a
        // filesystem read the manifest never declared. It must be refused so an
        // undeclared capability can't reach the sandbox profile (exit criterion 1).
        let (m, artifact) = manifest("0.1.0", &["api.github.com:443"]);
        let smuggled = CapabilitySet::from_spec(&crate::manifest::CapabilitiesSpec {
            filesystem_read: vec!["/home/user/.ssh".into()],
            network: vec!["api.github.com:443".into()],
            ..Default::default()
        });
        let err = InstalledPlugin::install_disabled(
            m,
            &artifact,
            None,
            UnsignedPolicy::Allow,
            smuggled,
            BTreeSet::new(),
        )
        .unwrap_err();
        assert!(
            matches!(err, LifecycleError::GrantExceedsManifest { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn full_lifecycle_to_enabled() {
        let mut p = install("0.1.0", &["api.github.com:443"]);
        p.mark_smoke_tested().unwrap();
        assert_eq!(p.state, LifecycleState::SmokeTested);
        p.enable("repository").unwrap();
        assert!(p.is_active());
        assert_eq!(p.enabled_scope.as_deref(), Some("repository"));
    }

    #[test]
    fn cannot_enable_before_smoke_test() {
        let mut p = install("0.1.0", &["api.github.com:443"]);
        assert!(matches!(
            p.enable("repository"),
            Err(LifecycleError::IllegalTransition {
                action: "enable",
                ..
            })
        ));
    }

    #[test]
    fn permission_identical_update_auto_applies() {
        let mut p = install("0.1.0", &["api.github.com:443"]);
        p.mark_smoke_tested().unwrap();
        p.enable("repository").unwrap();
        let (next, artifact) = manifest("0.2.0", &["api.github.com:443"]);
        let diff = p
            .update(next, &artifact, None, UnsignedPolicy::Allow)
            .unwrap();
        assert!(diff.is_identical());
        assert_eq!(p.manifest.version, "0.2.0");
        assert!(p.is_active(), "a safe update stays enabled");
    }

    #[test]
    fn permission_expanding_update_is_blocked() {
        let mut p = install("0.1.0", &["api.github.com:443"]);
        p.mark_smoke_tested().unwrap();
        p.enable("repository").unwrap();
        let (next, artifact) = manifest("0.2.0", &["api.github.com:443", "uploads.github.com:443"]);
        let err = p
            .update(next, &artifact, None, UnsignedPolicy::Allow)
            .unwrap_err();
        match err {
            LifecycleError::UpdateExpandsPermissions { diff, .. } => {
                assert_eq!(diff, "+ network: uploads.github.com:443");
            }
            other => panic!("expected an expansion block, got {other:?}"),
        }
        assert_eq!(p.state, LifecycleState::UpdateBlocked);
        // The grant did NOT change — still the old, narrower manifest.
        assert_eq!(p.manifest.version, "0.1.0");
        assert!(p.granted.grants(&crate::permission::Capability::Network(
            "api.github.com:443".into()
        )));
        assert!(!p.granted.grants(&crate::permission::Capability::Network(
            "uploads.github.com:443".into()
        )));
    }

    #[test]
    fn ui_permission_expanding_update_is_blocked() {
        let (installed, artifact) = ui_manifest("0.1.0", &["artifact-read"]);
        let mut plugin = InstalledPlugin::install_disabled(
            installed.clone(),
            &artifact,
            None,
            UnsignedPolicy::Allow,
            CapabilitySet::default(),
            requested_ui_capabilities(&installed),
        )
        .expect("UI component installs disabled");
        let (next, next_artifact) = ui_manifest("0.2.0", &["artifact-read", "run-read"]);
        let err = plugin
            .update(next, &next_artifact, None, UnsignedPolicy::Allow)
            .unwrap_err();
        match err {
            LifecycleError::UpdateExpandsPermissions { diff, .. } => {
                assert_eq!(diff, "+ ui.capability: run-read");
            }
            other => panic!("expected a UI permission expansion block, got {other:?}"),
        }
        assert_eq!(plugin.state, LifecycleState::UpdateBlocked);
        assert_eq!(plugin.manifest.version, "0.1.0");
    }

    #[test]
    fn ui_grants_must_be_explicit_and_within_the_manifest() {
        let (manifest, artifact) = ui_manifest("0.1.0", &["artifact-read"]);
        let over_broad = BTreeSet::from([UiCapability::CommandInvoke]);
        let error = InstalledPlugin::install_disabled(
            manifest,
            &artifact,
            None,
            UnsignedPolicy::Allow,
            CapabilitySet::default(),
            over_broad,
        )
        .unwrap_err();
        assert_eq!(
            error,
            LifecycleError::UiGrantExceedsManifest {
                capability: "command-invoke".into()
            }
        );
    }

    #[test]
    fn a_previously_withheld_ui_capability_requires_approval() {
        let (installed, artifact) = ui_manifest("0.1.0", &["artifact-read"]);
        let mut plugin = InstalledPlugin::install_disabled(
            installed,
            &artifact,
            None,
            UnsignedPolicy::Allow,
            CapabilitySet::default(),
            BTreeSet::new(),
        )
        .unwrap();
        let (next, next_artifact) = ui_manifest("0.2.0", &["artifact-read"]);
        let error = plugin
            .update(next, &next_artifact, None, UnsignedPolicy::Allow)
            .unwrap_err();
        assert!(matches!(
            error,
            LifecycleError::UpdateExpandsPermissions { ref diff, .. }
                if diff == "+ ui.capability: artifact-read"
        ));
        assert!(plugin.granted_ui_capabilities().is_empty());
    }

    #[test]
    fn approving_a_blocked_update_applies_the_expanded_grant() {
        let mut p = install("0.1.0", &["api.github.com:443"]);
        p.mark_smoke_tested().unwrap();
        p.enable("repository").unwrap();
        let (next, artifact) = manifest("0.2.0", &["api.github.com:443", "uploads.github.com:443"]);
        // First the update is blocked...
        let err = p
            .update(next.clone(), &artifact, None, UnsignedPolicy::Allow)
            .unwrap_err();
        assert_eq!(p.state, LifecycleState::UpdateBlocked);
        let receipt = match err {
            LifecycleError::UpdateExpandsPermissions {
                approval_receipt, ..
            } => approval_receipt,
            other => panic!("expected an expansion block, got {other:?}"),
        };
        // ...then the human approves it.
        p.approve_update(&receipt, next, &artifact, None, UnsignedPolicy::Allow)
            .unwrap();
        assert_eq!(p.manifest.version, "0.2.0");
        assert!(p.is_active(), "approval restores the prior enabled state");
        assert!(p.granted.grants(&crate::permission::Capability::Network(
            "uploads.github.com:443".into()
        )));
    }

    #[test]
    fn approval_is_bound_to_the_exact_reviewed_candidate_and_artifact() {
        let mut p = install("0.1.0", &["api.github.com:443"]);
        let (reviewed, reviewed_artifact) =
            manifest("0.2.0", &["api.github.com:443", "uploads.github.com:443"]);
        let err = p
            .update(
                reviewed.clone(),
                &reviewed_artifact,
                None,
                UnsignedPolicy::Allow,
            )
            .unwrap_err();
        let receipt = match err {
            LifecycleError::UpdateExpandsPermissions {
                approval_receipt, ..
            } => approval_receipt,
            other => panic!("expected an expansion block, got {other:?}"),
        };
        assert_eq!(
            p.pending_update
                .as_ref()
                .expect("sealed pending review")
                .approval_receipt,
            receipt
        );

        // A different, independently valid same-id/kind update cannot ride the
        // receipt for the candidate the user actually reviewed.
        let (substituted, substituted_artifact) = manifest(
            "0.3.0",
            &[
                "api.github.com:443",
                "uploads.github.com:443",
                "evil.example:443",
            ],
        );
        let err = p
            .approve_update(
                &receipt,
                substituted,
                &substituted_artifact,
                None,
                UnsignedPolicy::Allow,
            )
            .unwrap_err();
        assert_eq!(err, LifecycleError::ApprovedCandidateMismatch);
        assert_eq!(p.manifest.version, "0.1.0");

        // The sealed candidate remains approvable after a rejected substitution.
        p.approve_update(
            &receipt,
            reviewed,
            &reviewed_artifact,
            None,
            UnsignedPolicy::Allow,
        )
        .unwrap();
        assert_eq!(p.manifest.version, "0.2.0");
    }

    #[test]
    fn approval_receipts_are_required_and_single_use() {
        let mut p = install("0.1.0", &["api.github.com:443"]);
        let (next, artifact) = manifest("0.2.0", &["api.github.com:443", "uploads.github.com:443"]);
        let err = p
            .update(next.clone(), &artifact, None, UnsignedPolicy::Allow)
            .unwrap_err();
        let receipt = match err {
            LifecycleError::UpdateExpandsPermissions {
                approval_receipt, ..
            } => approval_receipt,
            other => panic!("expected an expansion block, got {other:?}"),
        };

        assert_eq!(
            p.approve_update(
                "stale-or-forged-receipt",
                next.clone(),
                &artifact,
                None,
                UnsignedPolicy::Allow,
            )
            .unwrap_err(),
            LifecycleError::ApprovalReceiptMismatch
        );
        p.approve_update(
            &receipt,
            next.clone(),
            &artifact,
            None,
            UnsignedPolicy::Allow,
        )
        .unwrap();
        assert_eq!(
            p.approve_update(&receipt, next, &artifact, None, UnsignedPolicy::Allow,)
                .unwrap_err(),
            LifecycleError::NoPendingUpdate
        );
    }

    #[test]
    fn rejecting_a_pending_update_consumes_its_receipt() {
        let mut p = install("0.1.0", &["api.github.com:443"]);
        p.mark_smoke_tested().unwrap();
        p.enable("repository").unwrap();
        let (next, artifact) = manifest("0.2.0", &["api.github.com:443", "uploads.github.com:443"]);
        let err = p
            .update(next.clone(), &artifact, None, UnsignedPolicy::Allow)
            .unwrap_err();
        let receipt = match err {
            LifecycleError::UpdateExpandsPermissions {
                approval_receipt, ..
            } => approval_receipt,
            other => panic!("expected an expansion block, got {other:?}"),
        };

        p.reject_pending_update(&receipt).unwrap();
        assert!(p.is_active(), "rejection restores the old enabled plugin");
        assert!(p.pending_update.is_none());
        assert_eq!(
            p.approve_update(&receipt, next, &artifact, None, UnsignedPolicy::Allow)
                .unwrap_err(),
            LifecycleError::NoPendingUpdate
        );
    }

    #[test]
    fn narrowing_update_applies_without_approval() {
        let mut p = install("0.1.0", &["api.github.com:443", "uploads.github.com:443"]);
        let (next, artifact) = manifest("0.2.0", &["api.github.com:443"]);
        let diff = p
            .update(next, &artifact, None, UnsignedPolicy::Allow)
            .unwrap();
        assert!(!diff.expands_permissions());
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(p.manifest.version, "0.2.0");
    }

    #[test]
    fn update_with_a_bad_checksum_is_rejected_before_diffing() {
        let mut p = install("0.1.0", &["api.github.com:443"]);
        let (next, _artifact) = manifest("0.2.0", &["api.github.com:443"]);
        let err = p
            .update(next, b"tampered", None, UnsignedPolicy::Allow)
            .unwrap_err();
        assert!(matches!(
            err,
            LifecycleError::Verify(VerifyError::ChecksumMismatch { .. })
        ));
        assert_eq!(p.manifest.version, "0.1.0");
    }

    #[test]
    fn update_that_changes_identity_is_rejected() {
        let mut p = install("0.1.0", &["api.github.com:443"]);
        let (mut next, artifact) = manifest("0.2.0", &["api.github.com:443"]);
        next.id = "totally-different".into();
        assert!(matches!(
            p.update(next, &artifact, None, UnsignedPolicy::Allow),
            Err(LifecycleError::IdentityChanged { .. })
        ));
    }

    #[test]
    fn revoke_makes_the_plugin_inert() {
        let mut p = install("0.1.0", &["api.github.com:443"]);
        p.mark_smoke_tested().unwrap();
        p.enable("repository").unwrap();
        p.revoke();
        assert_eq!(p.state, LifecycleState::Revoked);
        assert!(!p.is_active());
        assert!(p.enabled_scope.is_none());
    }

    // --- P6-A: resource caps fold into the permission diff ---

    #[test]
    fn an_update_that_only_raises_a_resource_cap_is_blocked_as_an_expansion() {
        // Same capabilities on both sides — but the update quietly asks for 8x
        // the memory. Without folding resource caps into the diff, this would
        // compute as `is_identical()` (capabilities unchanged) and auto-apply
        // with no re-approval surface, even though the plugin can now consume
        // far more memory than a human ever reviewed.
        let mut p = install("0.1.0", &["api.github.com:443"]);
        p.mark_smoke_tested().unwrap();
        p.enable("repository").unwrap();
        let (mut next, artifact) = manifest("0.2.0", &["api.github.com:443"]);
        assert_eq!(
            next.resources.memory_mb, 128,
            "the manifest-omitted default"
        );
        next.resources.memory_mb = 1024;
        let err = p
            .update(next, &artifact, None, UnsignedPolicy::Allow)
            .unwrap_err();
        match err {
            LifecycleError::UpdateExpandsPermissions { diff, .. } => {
                assert_eq!(diff, "+ resources.memory_mb: 128 -> 1024");
            }
            other => panic!("expected a resource-cap expansion block, got {other:?}"),
        }
        assert_eq!(p.state, LifecycleState::UpdateBlocked);
        // The grant/manifest did NOT change — still the old, lower cap.
        assert_eq!(p.manifest.resources.memory_mb, 128);
    }

    #[test]
    fn an_update_that_lowers_a_resource_cap_auto_applies() {
        let mut p = install("0.1.0", &["api.github.com:443"]);
        let (mut next, artifact) = manifest("0.2.0", &["api.github.com:443"]);
        next.resources.cpu_seconds = 10; // was 30 (the manifest-omitted default) — a narrower cap
        let diff = p
            .update(next, &artifact, None, UnsignedPolicy::Allow)
            .expect("a lowered cap must not require re-approval");
        assert!(!diff.expands_permissions());
        assert_eq!(diff.resource_changes.len(), 1);
        assert_eq!(
            p.manifest.resources.cpu_seconds, 10,
            "the lower cap applied"
        );
    }

    // --- P6-C: the "granted ⊆ manifest-requested" invariant holds structurally ---

    #[test]
    fn apply_manifest_refuses_an_over_broad_grant_even_when_the_diff_would_auto_apply() {
        // The new manifest requests the SAME capabilities as the old one, so the
        // capability diff alone computes as auto-applicable (no expansion). But
        // the grant handed to the shared apply path is over-broad relative to
        // what the new manifest requests. The subset invariant must be
        // re-asserted here regardless of what the diff says — today's public
        // `update()`/`approve_update()` always derive their grant directly from
        // the manifest (so this can't happen through them), but the guard must
        // hold structurally for any future path into `apply_manifest`, not just
        // by accident of how the current call sites are wired.
        let mut p = install("0.1.0", &["api.github.com:443"]);
        let (next, artifact) = manifest("0.2.0", &["api.github.com:443"]);
        let verified =
            crate::verify::verify_artifact(&next, &artifact, None, UnsignedPolicy::Allow).unwrap();
        let over_broad = CapabilitySet::from_spec(&crate::manifest::CapabilitiesSpec {
            filesystem_read: vec!["/etc/shadow".into()],
            network: vec!["api.github.com:443".into()],
            ..Default::default()
        });
        let err = p
            .apply_manifest(next, over_broad, BTreeSet::new(), verified.signed)
            .unwrap_err();
        assert!(
            matches!(err, LifecycleError::GrantExceedsManifest { .. }),
            "got {err:?}"
        );
        // Nothing was applied — the plugin still carries its original manifest.
        assert_eq!(p.manifest.version, "0.1.0");
    }

    #[test]
    fn the_shared_guard_rejects_an_over_broad_grant_directly() {
        let (m, _artifact) = manifest("0.1.0", &["api.github.com:443"]);
        let over_broad = CapabilitySet::from_spec(&crate::manifest::CapabilitiesSpec {
            secrets: vec!["undeclared-secret".into()],
            network: vec!["api.github.com:443".into()],
            ..Default::default()
        });
        let err = assert_granted_within_manifest(&m, &over_broad).unwrap_err();
        assert!(matches!(err, LifecycleError::GrantExceedsManifest { .. }));
    }
}
