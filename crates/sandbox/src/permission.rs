//! Capability grants and the permission diff (STEP 6.1).
//!
//! The capabilities a plugin *declares* in its manifest become a [`CapabilitySet`]
//! — the flat, comparable statement of everything it may touch. When a plugin
//! updates, the new manifest's set is compared against the *installed* set: any
//! capability present in the new set but not the old is an **expansion**, and an
//! expansion blocks the update until the user re-approves it (exit criterion 2).
//! Removed capabilities narrow the grant and never require approval.
//!
//! The diff is rendered exactly as the TUI shows it (`+ network: uploads.github.com:443`)
//! so the user decides against the real delta, not a summary.

use std::collections::BTreeSet;
use std::fmt;

use crate::manifest::{
    CapabilitiesSpec, PluginManifest, ResourcesSpec, UiCapability, UiContributionPoint, UiTarget,
};

/// A single, comparable capability a plugin holds. Each variant carries the
/// concrete grant (a path, a `host:port`, a secret name) so two sets diff at the
/// granularity the user reasons about — adding one host to a network allowlist
/// is a distinct, visible expansion.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    /// Read access to a filesystem path.
    FilesystemRead(String),
    /// Write access to a filesystem path.
    FilesystemWrite(String),
    /// Network access to a `host:port` destination.
    Network(String),
    /// Access to a named secret (brokered per call, never via env).
    Secret(String),
    /// Permission to spawn subprocesses.
    Subprocess,
}

impl Capability {
    /// The capability class, used to group a diff line (`network`, `secret`, …).
    #[must_use]
    pub fn class(&self) -> &'static str {
        match self {
            Capability::FilesystemRead(_) => "filesystem_read",
            Capability::FilesystemWrite(_) => "filesystem_write",
            Capability::Network(_) => "network",
            Capability::Secret(_) => "secret",
            Capability::Subprocess => "subprocess",
        }
    }

    /// The concrete grant value (the path / host:port / secret name).
    #[must_use]
    pub fn value(&self) -> &str {
        match self {
            Capability::FilesystemRead(v)
            | Capability::FilesystemWrite(v)
            | Capability::Network(v)
            | Capability::Secret(v) => v,
            Capability::Subprocess => "true",
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.class(), self.value())
    }
}

/// The complete set of capabilities a plugin holds. Order-independent and
/// deduplicated, so two manifests compare by *content*, not declaration order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilitySet {
    caps: BTreeSet<Capability>,
}

impl CapabilitySet {
    /// The set derived from a manifest's `[capabilities]` table.
    #[must_use]
    pub fn from_spec(spec: &CapabilitiesSpec) -> Self {
        let mut caps = BTreeSet::new();
        for p in &spec.filesystem_read {
            caps.insert(Capability::FilesystemRead(p.clone()));
        }
        for p in &spec.filesystem_write {
            caps.insert(Capability::FilesystemWrite(p.clone()));
        }
        for n in &spec.network {
            caps.insert(Capability::Network(n.clone()));
        }
        for s in &spec.secrets {
            caps.insert(Capability::Secret(s.clone()));
        }
        if spec.subprocess {
            caps.insert(Capability::Subprocess);
        }
        Self { caps }
    }

    /// Whether the set is empty — a plugin (e.g. a theme pack) that touches
    /// nothing. Used to enforce "data-only plugins carry zero capabilities".
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.caps.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.caps.len()
    }

    /// Whether the set holds a specific capability — the run-time gate: a call
    /// requesting a capability not in the set is denied (exit criterion 1).
    #[must_use]
    pub fn grants(&self, cap: &Capability) -> bool {
        self.caps.contains(cap)
    }

    /// Iterate the capabilities in stable (sorted) order.
    pub fn iter(&self) -> impl Iterator<Item = &Capability> {
        self.caps.iter()
    }

    /// Compute the diff from `self` (the installed set) to `next` (the update's
    /// requested set).
    #[must_use]
    pub fn diff_to(&self, next: &CapabilitySet) -> PermissionDiff {
        let added = next.caps.difference(&self.caps).cloned().collect();
        let removed = self.caps.difference(&next.caps).cloned().collect();
        PermissionDiff {
            added,
            removed,
            resource_changes: Vec::new(),
            ui_added: Vec::new(),
            ui_removed: Vec::new(),
        }
    }
}

/// A privilege-relevant embedded-UI declaration. UI code cannot gain OS sandbox
/// capabilities through this type; these are the distinct host surfaces users
/// must be shown when installing or updating component code.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum UiPermission {
    Capability(UiCapability),
    Compatibility {
        contract: &'static str,
        requirement: String,
    },
    Entrypoint {
        target: UiTarget,
        path: String,
    },
    Contribution {
        id: String,
        point: UiContributionPoint,
        renderer: String,
        targets: Vec<UiTarget>,
        fallback_renderer: Option<String>,
    },
}

impl fmt::Display for UiPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(capability) => {
                write!(f, "ui.capability: {}", capability.as_str())
            }
            Self::Compatibility {
                contract,
                requirement,
            } => write!(f, "ui.compatibility.{contract}: {requirement}"),
            Self::Entrypoint { target, path } => {
                write!(f, "ui.entrypoint.{}: {path}", target.as_str())
            }
            Self::Contribution {
                id,
                point,
                renderer,
                targets,
                fallback_renderer,
            } => {
                let targets = targets
                    .iter()
                    .map(|target| target.as_str())
                    .collect::<Vec<_>>()
                    .join(",");
                write!(
                    f,
                    "ui.contribution.{}: {id} (renderer={renderer}, targets={targets}",
                    point.as_str()
                )?;
                if let Some(fallback) = fallback_renderer {
                    write!(f, ", fallback={fallback}")?;
                }
                write!(f, ")")
            }
        }
    }
}

fn ui_permission_set(manifest: &PluginManifest) -> BTreeSet<UiPermission> {
    let Some(ui) = &manifest.ui else {
        return BTreeSet::new();
    };
    let mut permissions = BTreeSet::new();
    for capability in &ui.requested_capabilities {
        permissions.insert(UiPermission::Capability(*capability));
    }
    permissions.insert(UiPermission::Compatibility {
        contract: "protocol",
        requirement: ui.compatibility.protocol.clone(),
    });
    permissions.insert(UiPermission::Compatibility {
        contract: "sdk",
        requirement: ui.compatibility.sdk.clone(),
    });
    for (target, path) in [
        (UiTarget::Shared, &ui.entrypoints.shared),
        (UiTarget::Terminal, &ui.entrypoints.terminal),
        (UiTarget::Web, &ui.entrypoints.web),
    ] {
        if let Some(path) = path {
            permissions.insert(UiPermission::Entrypoint {
                target,
                path: path.clone(),
            });
        }
    }
    for contribution in &ui.contributions {
        let mut targets = contribution.targets.clone();
        targets.sort_unstable();
        targets.dedup();
        permissions.insert(UiPermission::Contribution {
            id: contribution.id.clone(),
            point: contribution.point,
            renderer: contribution.renderer.clone(),
            targets,
            fallback_renderer: contribution.fallback_renderer.clone(),
        });
    }
    permissions
}

/// Fold privilege-relevant embedded-UI changes into an existing permission
/// diff. Kept as the shared implementation for both the CLI manifest diff and
/// the live install/update lifecycle.
pub(crate) fn include_ui_permissions(
    diff: &mut PermissionDiff,
    old: &PluginManifest,
    next: &PluginManifest,
) {
    let old_ui = ui_permission_set(old);
    let next_ui = ui_permission_set(next);
    diff.ui_added = next_ui.difference(&old_ui).cloned().collect();
    diff.ui_removed = old_ui.difference(&next_ui).cloned().collect();
}

/// A change in one resource cap (memory/cpu/wall/output) between an installed
/// manifest and an update. Resource caps are scalars, not a set, so they don't
/// fit `Capability`'s added/removed shape — but a raised cap is just as much an
/// expansion of what a plugin may do as a new capability, and must gate
/// re-approval the same way (P6-A: caps used to sit outside the diff entirely,
/// so raising one was invisible and auto-applied).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceChange {
    /// The manifest field name (`memory_mb`, `cpu_seconds`, `wall_seconds`,
    /// `maximum_output_mb`), rendered as `resources.<field>`.
    pub field: &'static str,
    pub old: u64,
    pub new: u64,
}

impl ResourceChange {
    /// Whether this change *raises* the cap — the only direction that expands
    /// what the plugin may consume, and so the only direction that requires
    /// re-approval. Equal or lowered caps are a narrowing (or no-op) and stay
    /// auto-applicable.
    #[must_use]
    pub fn is_increase(&self) -> bool {
        // Zero is the executor's legacy "unlimited" sentinel for memory, CPU,
        // and wall time. Treat any transition to zero as an expansion so even a
        // programmatically-constructed manifest cannot auto-apply it.
        (self.new == 0 && self.old != 0) || self.new > self.old
    }
}

impl fmt::Display for ResourceChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "resources.{}: {} -> {}", self.field, self.old, self.new)
    }
}

/// Compute the resource-cap changes between an installed manifest's resources
/// and an update's. Only fields that actually differ are returned — an
/// unchanged field renders no line and contributes no expansion, mirroring how
/// [`CapabilitySet::diff_to`] only reports real deltas.
///
/// Both `ResourcesSpec`s are destructured **exhaustively** (every field named,
/// no `..` rest pattern): adding a 5th resource cap to the manifest schema
/// will fail to compile here until it is named below, so a future field can
/// never silently bypass the P6-A fold-in the way memory/cpu/wall/output did
/// before this diff existed.
#[must_use]
pub fn diff_resources(old: &ResourcesSpec, new: &ResourcesSpec) -> Vec<ResourceChange> {
    let ResourcesSpec {
        memory_mb: old_memory_mb,
        cpu_seconds: old_cpu_seconds,
        wall_seconds: old_wall_seconds,
        maximum_output_mb: old_maximum_output_mb,
    } = old;
    let ResourcesSpec {
        memory_mb: new_memory_mb,
        cpu_seconds: new_cpu_seconds,
        wall_seconds: new_wall_seconds,
        maximum_output_mb: new_maximum_output_mb,
    } = new;
    let fields: [(&'static str, u64, u64); 4] = [
        ("memory_mb", *old_memory_mb, *new_memory_mb),
        ("cpu_seconds", *old_cpu_seconds, *new_cpu_seconds),
        ("wall_seconds", *old_wall_seconds, *new_wall_seconds),
        (
            "maximum_output_mb",
            *old_maximum_output_mb,
            *new_maximum_output_mb,
        ),
    ];
    fields
        .into_iter()
        .filter(|(_, old_v, new_v)| old_v != new_v)
        .map(|(field, old, new)| ResourceChange { field, old, new })
        .collect()
}

/// Compute the permission diff between two plugin manifests **directly** — the
/// manifest-to-manifest comparison the CLI's `plugin diff` command needs (a
/// bare pair of manifest files, with no live [`crate::lifecycle::InstalledPlugin`]
/// / granted-capability state to compare against).
///
/// Folds in resource-cap changes (P6-A) via [`diff_resources`] exactly as
/// [`crate::lifecycle::InstalledPlugin::diff_update`] does for the live
/// lifecycle path, so both entry points detect a raised resource cap as an
/// expansion — a raised cap can no longer slip past the CLI gate that
/// `codypendent plugin diff` exists to enforce.
#[must_use]
pub fn diff_manifests(old: &PluginManifest, next: &PluginManifest) -> PermissionDiff {
    let old_set = CapabilitySet::from_spec(&old.capabilities);
    let next_set = CapabilitySet::from_spec(&next.capabilities);
    let mut diff = old_set.diff_to(&next_set);
    diff.resource_changes = diff_resources(&old.resources, &next.resources);
    include_ui_permissions(&mut diff, old, next);
    diff
}

/// The change in capabilities (and resource caps) between an installed
/// manifest and an update.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionDiff {
    /// Capabilities the update requests that the install did not hold — the
    /// **expansion** that gates re-approval.
    pub added: Vec<Capability>,
    /// Capabilities the install held that the update drops — a narrowing.
    pub removed: Vec<Capability>,
    /// Resource-cap deltas (memory/cpu/wall/output). A field the update
    /// *raises* counts as an expansion (STEP 6.1 exit criterion 2, P6-A); a
    /// field it lowers or leaves unchanged never requires re-approval.
    /// [`CapabilitySet::diff_to`] never populates this (it has no resource
    /// data to compare) — [`crate::lifecycle`] folds it in from the manifests.
    pub resource_changes: Vec<ResourceChange>,
    /// Newly requested host-facing UI privileges. Any addition gates update
    /// re-approval just like a filesystem/network capability addition.
    pub ui_added: Vec<UiPermission>,
    /// UI privileges or executable surfaces removed by the update.
    pub ui_removed: Vec<UiPermission>,
}

impl PermissionDiff {
    /// Whether the update expands permissions. `true` ⇒ the update is blocked
    /// until the user re-approves (STEP 6.1 exit criterion 2). A capability
    /// addition OR a raised resource cap both count — this must never
    /// *under*-report an expansion.
    #[must_use]
    pub fn expands_permissions(&self) -> bool {
        !self.added.is_empty()
            || !self.ui_added.is_empty()
            || self
                .resource_changes
                .iter()
                .any(ResourceChange::is_increase)
    }

    /// Whether the two manifests declare identical capabilities and resource
    /// caps.
    #[must_use]
    pub fn is_identical(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.resource_changes.is_empty()
            && self.ui_added.is_empty()
            && self.ui_removed.is_empty()
    }

    /// Render the diff the way the TUI displays it — one `+`/`-` line per
    /// capability or resource-cap change, additions/increases first:
    ///
    /// ```text
    /// + network: uploads.github.com:443
    /// + resources.memory_mb: 256 -> 1024
    /// - secret: legacy-token
    /// - resources.cpu_seconds: 60 -> 30
    /// ```
    #[must_use]
    pub fn render(&self) -> String {
        let mut plus = Vec::with_capacity(
            self.added.len() + self.resource_changes.len() + self.ui_added.len(),
        );
        let mut minus = Vec::with_capacity(
            self.removed.len() + self.resource_changes.len() + self.ui_removed.len(),
        );
        for cap in &self.added {
            plus.push(format!("+ {cap}"));
        }
        for permission in &self.ui_added {
            plus.push(format!("+ {permission}"));
        }
        for change in &self.resource_changes {
            if change.is_increase() {
                plus.push(format!("+ {change}"));
            } else {
                minus.push(format!("- {change}"));
            }
        }
        for cap in &self.removed {
            minus.push(format!("- {cap}"));
        }
        for permission in &self.ui_removed {
            minus.push(format!("- {permission}"));
        }
        plus.into_iter().chain(minus).collect::<Vec<_>>().join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(network: &[&str], secrets: &[&str], subprocess: bool) -> CapabilitiesSpec {
        CapabilitiesSpec {
            filesystem_read: vec![],
            filesystem_write: vec![],
            network: network.iter().map(|s| s.to_string()).collect(),
            secrets: secrets.iter().map(|s| s.to_string()).collect(),
            subprocess,
        }
    }

    #[test]
    fn set_is_order_independent() {
        let a = CapabilitySet::from_spec(&spec(&["a:1", "b:2"], &[], false));
        let b = CapabilitySet::from_spec(&spec(&["b:2", "a:1"], &[], false));
        assert_eq!(a, b);
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn identical_manifests_need_no_reapproval() {
        let installed =
            CapabilitySet::from_spec(&spec(&["api.github.com:443"], &["github-token"], false));
        let update =
            CapabilitySet::from_spec(&spec(&["api.github.com:443"], &["github-token"], false));
        let diff = installed.diff_to(&update);
        assert!(diff.is_identical());
        assert!(!diff.expands_permissions());
    }

    #[test]
    fn added_network_host_is_an_expansion() {
        let installed = CapabilitySet::from_spec(&spec(&["api.github.com:443"], &[], false));
        let update = CapabilitySet::from_spec(&spec(
            &["api.github.com:443", "uploads.github.com:443"],
            &[],
            false,
        ));
        let diff = installed.diff_to(&update);
        assert!(diff.expands_permissions());
        assert_eq!(
            diff.added,
            vec![Capability::Network("uploads.github.com:443".into())]
        );
        assert_eq!(diff.render(), "+ network: uploads.github.com:443");
    }

    #[test]
    fn adding_subprocess_is_an_expansion() {
        let installed = CapabilitySet::from_spec(&spec(&[], &[], false));
        let update = CapabilitySet::from_spec(&spec(&[], &[], true));
        let diff = installed.diff_to(&update);
        assert!(diff.expands_permissions());
        assert_eq!(diff.added, vec![Capability::Subprocess]);
    }

    #[test]
    fn removing_a_capability_narrows_and_needs_no_approval() {
        let installed = CapabilitySet::from_spec(&spec(&["a:1", "b:2"], &["s"], false));
        let update = CapabilitySet::from_spec(&spec(&["a:1"], &[], false));
        let diff = installed.diff_to(&update);
        assert!(!diff.expands_permissions());
        assert_eq!(diff.removed.len(), 2);
        // Removals render with a leading '-'.
        assert!(diff.render().lines().all(|l| l.starts_with('-')));
    }

    #[test]
    fn grants_gates_a_runtime_request() {
        let set = CapabilitySet::from_spec(&spec(&["api.github.com:443"], &[], false));
        assert!(set.grants(&Capability::Network("api.github.com:443".into())));
        // An undeclared host is not granted (exit criterion 1 at the decision layer).
        assert!(!set.grants(&Capability::Network("evil.example.com:443".into())));
        assert!(!set.grants(&Capability::FilesystemRead("/home/user/.ssh/id_rsa".into())));
    }

    // --- P6-A: resource caps fold into the permission diff ---

    #[test]
    fn a_resource_cap_increase_is_an_expansion_and_renders() {
        let old = ResourcesSpec {
            memory_mb: 256,
            cpu_seconds: 30,
            wall_seconds: 60,
            maximum_output_mb: 8,
        };
        let new = ResourcesSpec {
            memory_mb: 1024,
            ..old.clone()
        };
        let changes = diff_resources(&old, &new);
        assert_eq!(
            changes,
            vec![ResourceChange {
                field: "memory_mb",
                old: 256,
                new: 1024
            }]
        );
        assert!(changes[0].is_increase());

        let diff = PermissionDiff {
            resource_changes: changes,
            ..Default::default()
        };
        assert!(
            diff.expands_permissions(),
            "a raised cap must require re-approval"
        );
        assert!(!diff.is_identical());
        assert_eq!(diff.render(), "+ resources.memory_mb: 256 -> 1024");
    }

    #[test]
    fn a_resource_cap_decrease_is_a_narrowing_and_auto_applies() {
        let old = ResourcesSpec {
            memory_mb: 1024,
            cpu_seconds: 30,
            wall_seconds: 60,
            maximum_output_mb: 8,
        };
        let new = ResourcesSpec {
            memory_mb: 256,
            ..old.clone()
        };
        let changes = diff_resources(&old, &new);
        assert!(!changes[0].is_increase());

        let diff = PermissionDiff {
            resource_changes: changes,
            ..Default::default()
        };
        assert!(
            !diff.expands_permissions(),
            "a lowered cap needs no re-approval"
        );
        assert!(
            !diff.is_identical(),
            "the cap DID change, just not an expansion"
        );
        assert_eq!(diff.render(), "- resources.memory_mb: 1024 -> 256");
    }

    #[test]
    fn identical_resource_caps_produce_no_changes_and_stay_identical() {
        let caps = ResourcesSpec::default();
        assert!(diff_resources(&caps, &caps.clone()).is_empty());
        let diff = PermissionDiff::default();
        assert!(diff.is_identical());
        assert!(!diff.expands_permissions());
    }

    #[test]
    fn multiple_resource_fields_can_change_in_different_directions() {
        // A raised memory cap alongside a lowered wall-clock cap: the net
        // result must still classify as an expansion (fails toward
        // re-approval) — a decrease elsewhere can never mask an increase.
        let old = ResourcesSpec {
            memory_mb: 256,
            cpu_seconds: 30,
            wall_seconds: 120,
            maximum_output_mb: 8,
        };
        let new = ResourcesSpec {
            memory_mb: 512,
            wall_seconds: 60,
            ..old.clone()
        };
        let changes = diff_resources(&old, &new);
        assert_eq!(changes.len(), 2);
        let diff = PermissionDiff {
            resource_changes: changes,
            ..Default::default()
        };
        assert!(diff.expands_permissions());
        let rendered = diff.render();
        assert!(rendered.contains("+ resources.memory_mb: 256 -> 512"));
        assert!(rendered.contains("- resources.wall_seconds: 120 -> 60"));
    }

    // --- P6-A fix pass: diff_manifests() is the CLI's manifest-to-manifest gate ---

    fn manifest_with_memory(memory_mb: u64) -> PluginManifest {
        let toml = format!(
            r#"
schema_version = 1
id = "github"
name = "GitHub"
version = "0.1.0"
kind = "native-process"
publisher = "codypendent-project"
[runtime]
command = "codypendent-plugin-github"
[capabilities]
network = ["api.github.com:443"]
[resources]
memory_mb = {memory_mb}
cpu_seconds = 30
wall_seconds = 60
maximum_output_mb = 8
[security]
checksum = "sha256:set-during-packaging"
signature = "set-during-packaging"
"#
        );
        crate::manifest::parse_manifest(&toml).expect("manifest parses")
    }

    #[test]
    fn diff_manifests_reports_a_raised_resource_cap_as_an_expansion() {
        // The exact P6-A defect at the CLI entry point: identical
        // capabilities, only the memory cap raised. Before diff_manifests()
        // folded in diff_resources(), a bare CapabilitySet::diff_to() here
        // would report `is_identical()` and the CLI would exit 0.
        let installed = manifest_with_memory(256);
        let update = manifest_with_memory(4096);
        let diff = diff_manifests(&installed, &update);
        assert!(
            diff.expands_permissions(),
            "a raised resource cap must be an expansion"
        );
        assert_eq!(diff.render(), "+ resources.memory_mb: 256 -> 4096");
    }

    #[test]
    fn diff_manifests_treats_a_lowered_resource_cap_as_a_narrowing() {
        let installed = manifest_with_memory(4096);
        let update = manifest_with_memory(256);
        let diff = diff_manifests(&installed, &update);
        assert!(!diff.expands_permissions());
        assert!(!diff.is_identical());
    }

    #[test]
    fn diff_manifests_matches_a_capability_expansion_like_capabilityset_diff_to() {
        // diff_manifests() must still catch a plain capability expansion, not
        // just resource caps — it composes CapabilitySet::diff_to(), it
        // doesn't replace it.
        let installed = manifest_with_memory(256);
        let update = crate::manifest::parse_manifest(
            r#"
schema_version = 1
id = "github"
name = "GitHub"
version = "0.2.0"
kind = "native-process"
publisher = "codypendent-project"
[runtime]
command = "codypendent-plugin-github"
[capabilities]
network = ["api.github.com:443", "uploads.github.com:443"]
[resources]
memory_mb = 256
cpu_seconds = 30
wall_seconds = 60
maximum_output_mb = 8
[security]
checksum = "sha256:set-during-packaging"
signature = "set-during-packaging"
"#,
        )
        .expect("manifest parses");
        let diff = diff_manifests(&installed, &update);
        assert!(diff.expands_permissions());
        assert_eq!(diff.render(), "+ network: uploads.github.com:443");
    }

    fn manifest_with_ui(ui_capabilities: &str, targets: &str) -> PluginManifest {
        let toml = format!(
            r#"
schema_version = 1
id = "ui-test"
name = "UI Test"
version = "0.1.0"
kind = "ui-component"
publisher = "me"
[ui]
schema_version = 1
requested_capabilities = [{ui_capabilities}]
[ui.compatibility]
protocol = ">=1.0,<2.0"
sdk = "^1.0"
[ui.entrypoints]
shared = "dist/shared.js"
[[ui.contributions]]
id = "test.report"
point = "artifact-renderer"
renderer = "test.Report"
targets = [{targets}]
"#
        );
        crate::manifest::parse_manifest(&toml).expect("UI manifest parses")
    }

    #[test]
    fn ui_capability_addition_is_a_visible_permission_expansion() {
        let installed = manifest_with_ui("\"artifact-read\"", "\"shared\"");
        let update = manifest_with_ui(
            "\"artifact-read\", \"run-read\", \"command-invoke\"",
            "\"shared\"",
        );
        let diff = diff_manifests(&installed, &update);
        assert!(diff.expands_permissions());
        assert_eq!(diff.ui_added.len(), 2);
        let rendered = diff.render();
        assert!(rendered.contains("+ ui.capability: run-read"));
        assert!(rendered.contains("+ ui.capability: command-invoke"));
    }

    #[test]
    fn adding_or_changing_an_executable_ui_surface_requires_reapproval() {
        let installed = manifest_with_ui("", "\"shared\"");
        let mut update = installed.clone();
        let ui = update.ui.as_mut().expect("UI exists");
        ui.entrypoints.terminal = Some("dist/terminal.js".into());
        ui.contributions[0].renderer = "test.NewReport".into();
        let diff = diff_manifests(&installed, &update);
        assert!(diff.expands_permissions());
        assert!(diff
            .render()
            .contains("+ ui.entrypoint.terminal: dist/terminal.js"));
        assert!(diff.render().contains("renderer=test.NewReport"));
        assert!(diff
            .render()
            .contains("- ui.contribution.artifact-renderer"));
    }

    #[test]
    fn removing_ui_authority_is_a_narrowing() {
        let installed = manifest_with_ui("\"artifact-read\", \"run-read\"", "\"shared\"");
        let update = manifest_with_ui("\"artifact-read\"", "\"shared\"");
        let diff = diff_manifests(&installed, &update);
        assert!(!diff.expands_permissions());
        assert_eq!(
            diff.render(),
            "- ui.capability: run-read",
            "removed UI host authority is visible but auto-applicable"
        );
    }

    #[test]
    fn target_order_and_duplicate_capabilities_do_not_create_phantom_ui_diffs() {
        let installed = manifest_with_ui(
            "\"artifact-read\", \"artifact-read\"",
            "\"terminal\", \"web\"",
        );
        let update = manifest_with_ui("\"artifact-read\"", "\"web\", \"terminal\"");
        let diff = diff_manifests(&installed, &update);
        assert!(diff.is_identical());
    }

    #[test]
    fn compatibility_range_changes_are_reviewed_before_new_hosts_execute_code() {
        let installed = manifest_with_ui("", "\"shared\"");
        let mut update = installed.clone();
        update.ui.as_mut().unwrap().compatibility.sdk = ">=1.0,<2.0".into();
        let diff = diff_manifests(&installed, &update);
        assert!(diff.expands_permissions());
        assert!(diff.render().contains("+ ui.compatibility.sdk: >=1.0,<2.0"));
        assert!(diff.render().contains("- ui.compatibility.sdk: ^1.0"));
    }
}
