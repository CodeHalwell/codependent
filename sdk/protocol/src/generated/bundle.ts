/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * How sensitive an artifact's contents are.
 *
 * Ordered least to most restrictive; higher classifications gate model routing, export, and display. A wire enum, so it is internally tagged and carries an [`DataClassification::Unknown`] fallback for forward compatibility.
 */
type DataClassification =
  | {
      type: "Public";
    }
  | {
      type: "Internal";
    }
  | {
      type: "Confidential";
    }
  | {
      type: "Secret";
    }
  | {
      type: "Unknown";
    };
/**
 * Semantic role of an archive entry.
 */
export type BundleEntryKind =
  | {
      type: "TranscriptEvents";
    }
  | {
      type: "RoutingMetadata";
    }
  | {
      type: "Approvals";
    }
  | {
      type: "ArtifactManifest";
    }
  | {
      type: "Patch";
    }
  | {
      type: "EnvironmentDiagnostics";
    }
  | {
      type: "Unknown";
    };
/**
 * Redactions an exporter must perform before hashing archive entries.
 */
export type BundleRedactionPolicy =
  | {
      type: "Standard";
    }
  | {
      type: "SupportSafe";
    }
  | {
      type: "Unknown";
    };
/**
 * Kind of durable identity rewritten by an import.
 */
export type BundleIdentityKind =
  | {
      type: "Session";
    }
  | {
      type: "Run";
    }
  | {
      type: "Artifact";
    }
  | {
      type: "Approval";
    }
  | {
      type: "ChangeSet";
    }
  | {
      type: "Unknown";
    };
/**
 * How an importer handles a source identity that already exists locally.
 */
export type BundleCollisionPolicy =
  | {
      type: "Reject";
    }
  | {
      type: "Remap";
    }
  | {
      type: "Skip";
    }
  | {
      type: "Unknown";
    };

interface BundleCatalog {
  export_receipt: BundleExportReceipt;
  export_request: BundleExportRequest;
  import_receipt: BundleImportReceipt;
  import_request: BundleImportRequest;
  manifest: BundleManifest;
}
/**
 * Successful export. Archive bytes remain behind an artifact reference.
 */
export interface BundleExportReceipt {
  bundle: ArtifactRef;
  manifest: BundleManifest;
}
/**
 * A pointer to a stored artifact plus the metadata needed to handle it safely.
 *
 * `id` and `sha256` are deliberately independent: identical bytes dedup to one blob (keyed by `sha256`) but every occurrence is its own `ArtifactRef` with its own id and `sensitivity` (Chapter 14 / STEP 1.4). Classification checks always read the ref in hand, never a row looked up by hash.
 */
interface ArtifactRef {
  byte_length: number;
  id: string;
  /**
   * IANA media type, e.g. `text/plain` or `application/json`.
   */
  media_type: string;
  sensitivity: DataClassification;
  /**
   * Lowercase hex SHA-256 of the blob's bytes (the content address).
   */
  sha256: string;
}
/**
 * Self-describing manifest stored in every bundle.
 */
export interface BundleManifest {
  created_at: string;
  entries?: BundleEntryManifest[];
  format_version: number;
  inclusion?: BundleInclusionPolicy;
  /**
   * Lowercase hexadecimal SHA-256 of the canonical entry manifest.
   */
  manifest_sha256: string;
  redaction_policy?: BundleRedactionPolicy;
  redaction_summary?: BundleRedactionSummary;
  source_session_ids?: string[];
}
/**
 * One regular-file entry in the archive.
 */
export interface BundleEntryManifest {
  byte_length: number;
  classification: DataClassification;
  kind: BundleEntryKind;
  /**
   * IANA media type.
   */
  media_type: string;
  /**
   * Normalized relative archive path. Importers still validate this value.
   */
  path: string;
  /**
   * Lowercase hexadecimal SHA-256 of the uncompressed entry bytes.
   */
  sha256: string;
}
/**
 * Exact categories the caller permits an exporter to include.
 *
 * Every switch defaults to `false`; omission therefore cannot accidentally broaden an export when a newer exporter adds another category.
 */
export interface BundleInclusionPolicy {
  approvals?: boolean;
  artifact_manifests?: boolean;
  environment_diagnostics?: boolean;
  patches?: boolean;
  routing_metadata?: boolean;
  transcript_events?: boolean;
}
/**
 * Auditable aggregate of material removed or replaced during export.
 */
export interface BundleRedactionSummary {
  artifact_bodies_omitted?: number;
  credentials_omitted?: number;
  diagnostics_fields_omitted?: number;
  entries_omitted?: number;
  values_replaced?: number;
}
/**
 * Request a deterministic bundle export.
 */
export interface BundleExportRequest {
  inclusion: BundleInclusionPolicy;
  redaction_policy?: BundleRedactionPolicy;
  source_session_ids?: string[];
}
/**
 * Successful import result. No approvals or credentials are restored.
 */
export interface BundleImportReceipt {
  identity_mappings?: BundleIdentityMapping[];
  imported_session_ids?: string[];
  provenance: BundleImportProvenance;
  skipped_entries?: number;
}
/**
 * Mapping from an opaque source identity to its newly allocated local one.
 */
export interface BundleIdentityMapping {
  kind: BundleIdentityKind;
  local_id: string;
  /**
   * Provenance attached to the corresponding imported durable record.
   */
  provenance: BundleImportProvenance;
  source_id: string;
}
/**
 * Provenance attached to every durable record created by an import.
 */
export interface BundleImportProvenance {
  /**
   * Lowercase hexadecimal SHA-256 of the imported archive bytes.
   */
  bundle_sha256: string;
  imported_at: string;
  /**
   * Lowercase hexadecimal SHA-256 asserted by the verified manifest.
   */
  manifest_sha256: string;
  source_session_ids?: string[];
}
/**
 * Request an import from a previously uploaded bundle artifact.
 */
export interface BundleImportRequest {
  bundle: ArtifactRef;
  collision_policy?: BundleCollisionPolicy;
}

type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
