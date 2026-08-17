//! Versioned, redacted session and support bundle wire contracts.
//!
//! Bundles are data interchange archives, not backups. In particular, these
//! contracts contain no credential value, secret reference, or operation that
//! could restore credentials. Importers create local identities and retain the
//! source identity only as provenance.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::artifact::{ArtifactRef, DataClassification};
use crate::ids::SessionId;

/// First bundle format understood by this protocol crate.
pub const BUNDLE_FORMAT_V1: u32 = 1;

/// Exact categories the caller permits an exporter to include.
///
/// Every switch defaults to `false`; omission therefore cannot accidentally
/// broaden an export when a newer exporter adds another category.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct BundleInclusionPolicy {
    pub transcript_events: bool,
    pub routing_metadata: bool,
    pub approvals: bool,
    pub artifact_manifests: bool,
    pub patches: bool,
    pub environment_diagnostics: bool,
}

/// Redactions an exporter must perform before hashing archive entries.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum BundleRedactionPolicy {
    /// Built-in safe policy: credentials are omitted and recognized secrets and
    /// local identifying values are replaced with stable placeholders.
    #[default]
    Standard,
    /// More restrictive support export, omitting all artifact bodies.
    SupportSafe,
    /// A policy supplied by a newer peer. Receivers must not interpret this as
    /// less restrictive than `Standard`.
    #[serde(other)]
    Unknown,
}

/// Auditable aggregate of material removed or replaced during export.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct BundleRedactionSummary {
    pub values_replaced: u64,
    pub entries_omitted: u64,
    pub credentials_omitted: u64,
    pub artifact_bodies_omitted: u64,
    pub diagnostics_fields_omitted: u64,
}

/// Semantic role of an archive entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum BundleEntryKind {
    TranscriptEvents,
    RoutingMetadata,
    Approvals,
    ArtifactManifest,
    Patch,
    EnvironmentDiagnostics,
    #[serde(other)]
    Unknown,
}

/// One regular-file entry in the archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BundleEntryManifest {
    /// Normalized relative archive path. Importers still validate this value.
    pub path: String,
    pub kind: BundleEntryKind,
    /// Lowercase hexadecimal SHA-256 of the uncompressed entry bytes.
    pub sha256: String,
    pub byte_length: u64,
    /// IANA media type.
    pub media_type: String,
    pub classification: DataClassification,
}

/// Self-describing manifest stored in every bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BundleManifest {
    pub format_version: u32,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub source_session_ids: Vec<SessionId>,
    #[serde(default)]
    pub inclusion: BundleInclusionPolicy,
    #[serde(default)]
    pub redaction_policy: BundleRedactionPolicy,
    #[serde(default)]
    pub redaction_summary: BundleRedactionSummary,
    #[serde(default)]
    pub entries: Vec<BundleEntryManifest>,
    /// Lowercase hexadecimal SHA-256 of the canonical entry manifest.
    pub manifest_sha256: String,
}

/// Request a deterministic bundle export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BundleExportRequest {
    #[serde(default)]
    pub source_session_ids: Vec<SessionId>,
    pub inclusion: BundleInclusionPolicy,
    #[serde(default)]
    pub redaction_policy: BundleRedactionPolicy,
}

/// Successful export. Archive bytes remain behind an artifact reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BundleExportReceipt {
    pub bundle: ArtifactRef,
    pub manifest: BundleManifest,
}

/// How an importer handles a source identity that already exists locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum BundleCollisionPolicy {
    /// Reject the complete import without making changes.
    Reject,
    /// Allocate a fresh local identity and preserve the source in provenance.
    #[default]
    Remap,
    /// Skip colliding records while importing independent records.
    Skip,
    #[serde(other)]
    Unknown,
}

/// Request an import from a previously uploaded bundle artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BundleImportRequest {
    pub bundle: ArtifactRef,
    #[serde(default)]
    pub collision_policy: BundleCollisionPolicy,
}

/// Kind of durable identity rewritten by an import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum BundleIdentityKind {
    Session,
    Run,
    Artifact,
    Approval,
    ChangeSet,
    #[serde(other)]
    Unknown,
}

/// Mapping from an opaque source identity to its newly allocated local one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BundleIdentityMapping {
    pub kind: BundleIdentityKind,
    pub source_id: String,
    pub local_id: String,
    /// Provenance attached to the corresponding imported durable record.
    pub provenance: BundleImportProvenance,
}

/// Provenance attached to every durable record created by an import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BundleImportProvenance {
    /// Lowercase hexadecimal SHA-256 of the imported archive bytes.
    pub bundle_sha256: String,
    /// Lowercase hexadecimal SHA-256 asserted by the verified manifest.
    pub manifest_sha256: String,
    pub imported_at: DateTime<Utc>,
    #[serde(default)]
    pub source_session_ids: Vec<SessionId>,
}

/// Successful import result. No approvals or credentials are restored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BundleImportReceipt {
    pub provenance: BundleImportProvenance,
    #[serde(default)]
    pub identity_mappings: Vec<BundleIdentityMapping>,
    #[serde(default)]
    pub imported_session_ids: Vec<SessionId>,
    #[serde(default)]
    pub skipped_entries: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ArtifactId;

    fn artifact() -> ArtifactRef {
        ArtifactRef {
            id: ArtifactId::new(),
            media_type: "application/vnd.codypendent.bundle".into(),
            byte_length: 42,
            sha256: "ab".repeat(32),
            sensitivity: DataClassification::Confidential,
        }
    }

    #[test]
    fn omitted_inclusion_fields_are_fail_closed() {
        let policy: BundleInclusionPolicy = serde_json::from_str("{}").expect("policy");
        assert_eq!(policy, BundleInclusionPolicy::default());
        assert!(!policy.transcript_events && !policy.environment_diagnostics);
    }

    #[test]
    fn export_and_import_requests_round_trip() {
        let session = SessionId::new();
        let export = BundleExportRequest {
            source_session_ids: vec![session],
            inclusion: BundleInclusionPolicy {
                transcript_events: true,
                ..Default::default()
            },
            redaction_policy: BundleRedactionPolicy::SupportSafe,
        };
        let encoded = serde_json::to_string(&export).expect("serialize export");
        assert_eq!(
            serde_json::from_str::<BundleExportRequest>(&encoded).unwrap(),
            export
        );

        let import = BundleImportRequest {
            bundle: artifact(),
            collision_policy: BundleCollisionPolicy::Remap,
        };
        let encoded = serde_json::to_string(&import).expect("serialize import");
        assert_eq!(
            serde_json::from_str::<BundleImportRequest>(&encoded).unwrap(),
            import
        );
    }

    #[test]
    fn unknown_variants_and_additive_fields_are_tolerated() {
        let kind: BundleEntryKind = serde_json::from_value(serde_json::json!({
            "type": "FutureEntry", "future": true
        }))
        .expect("unknown entry kind");
        assert_eq!(kind, BundleEntryKind::Unknown);

        let policy: BundleCollisionPolicy = serde_json::from_value(serde_json::json!({
            "type": "FutureCollisionPolicy"
        }))
        .expect("unknown collision policy");
        assert_eq!(policy, BundleCollisionPolicy::Unknown);

        let request: BundleImportRequest = serde_json::from_value(serde_json::json!({
            "bundle": artifact(),
            "future_field": 7
        }))
        .expect("additive field and default collision policy");
        assert_eq!(request.collision_policy, BundleCollisionPolicy::Remap);
    }

    #[test]
    fn contract_has_no_credential_restoration_field() {
        let receipt = BundleImportReceipt {
            provenance: BundleImportProvenance {
                bundle_sha256: "cd".repeat(32),
                manifest_sha256: "ef".repeat(32),
                imported_at: Utc::now(),
                source_session_ids: vec![],
            },
            identity_mappings: vec![],
            imported_session_ids: vec![],
            skipped_entries: 0,
        };
        let value = serde_json::to_value(receipt).expect("serialize receipt");
        let text = value.to_string();
        assert!(!text.contains("credential") && !text.contains("secret"));
    }
}
