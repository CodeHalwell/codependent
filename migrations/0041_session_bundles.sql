-- Milestone 2 Task 2.7: durable receipts for versioned, redacted session and
-- support bundles. The archive BYTES live in `artifacts` (content-addressed);
-- these tables are the auditable record of what was included, what was
-- redacted, and — on import — which local identities were minted.
--
-- Nothing here can restore a credential or an approval: see the module
-- contract in `crates/protocol/src/bundle.rs:1-6` and its
-- `contract_has_no_credential_restoration_field` test.

-- One row per completed export.
CREATE TABLE session_bundles (
    -- Daemon-minted. Not on the wire: `BundleExportReceipt` identifies a bundle
    -- by its artifact, so this id is internal join-key only.
    id TEXT PRIMARY KEY,
    -- The exporting principal's kernel-derived uid. NOT NULL, unlike 0031/0039:
    -- there are no pre-existing rows to adopt, so a NULL here would be a bug
    -- rather than history and must fail at write time.
    owner_uid INTEGER NOT NULL,
    -- `BundleExportReceipt.bundle` — the archive blob. The artifact row carries
    -- its own `owner_uid` (0039); set it to the same principal or the exporter
    -- cannot `ReadArtifact` its own bundle back.
    artifact_id TEXT NOT NULL REFERENCES artifacts(id),
    -- `BUNDLE_FORMAT_V1` at write time. Stored (not assumed) so a future
    -- importer can refuse a version it does not understand without guessing.
    format_version INTEGER NOT NULL,
    -- `BundleManifest.manifest_sha256`, lowercase hex SHA-256 of the canonical
    -- entry manifest. 64 chars; the CHECK is cheap and catches a truncated
    -- hash at insert instead of at verification time.
    manifest_sha256 TEXT NOT NULL CHECK (length(manifest_sha256) = 64),
    -- Serialized `BundleInclusionPolicy` verbatim. Stored as the policy that
    -- was APPLIED, not re-derived later: every switch defaults to false, so a
    -- newer exporter adding a category must not retroactively widen this row.
    inclusion_json TEXT NOT NULL,
    -- `BundleRedactionPolicy`. `Unknown` is deliberately absent from the CHECK:
    -- the contract says receivers must not treat it as less restrictive than
    -- `Standard`, so persisting it would let a later re-export claim a policy
    -- this daemon never applied. Reject it at the command boundary instead.
    redaction_policy TEXT NOT NULL
        CHECK (redaction_policy IN ('Standard', 'SupportSafe')),
    -- `BundleRedactionSummary` counters. The audit answer to "what was removed";
    -- kept even when every counter is zero, because a measured zero and an
    -- unrecorded export are different facts.
    redaction_summary_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    -- The durable command that produced this bundle, so a replayed
    -- `ExportBundle` can return the prior receipt instead of re-archiving.
    command_id TEXT REFERENCES commands(id)
);

-- One artifact is one bundle: a second row against the same blob would make
-- "which manifest describes these bytes" ambiguous.
CREATE UNIQUE INDEX idx_session_bundles_artifact
    ON session_bundles (artifact_id);
CREATE INDEX idx_session_bundles_owner
    ON session_bundles (owner_uid, created_at DESC, id);

-- The (bundle, source session) pairs behind `BundleManifest.source_session_ids`.
CREATE TABLE session_bundle_sources (
    bundle_id TEXT NOT NULL REFERENCES session_bundles(id),
    session_id TEXT NOT NULL REFERENCES sessions(id),
    -- Preserves the request's session order. The manifest hash is computed over
    -- a canonical ordering, so losing order would make a re-export of the same
    -- input produce a different `manifest_sha256`.
    ordinal INTEGER NOT NULL,
    PRIMARY KEY (bundle_id, session_id)
);

-- Ownership is joined through `sessions`; it is intentionally not duplicated
-- here where it could drift (same rationale as `session_search_sources`, 0040).
CREATE INDEX idx_session_bundle_sources_session
    ON session_bundle_sources (session_id);

-- One row per `BundleEntryManifest`.
CREATE TABLE session_bundle_entries (
    bundle_id TEXT NOT NULL REFERENCES session_bundles(id),
    -- Normalized relative archive path. Part of the primary key, which is what
    -- makes the "duplicate paths" hostile-import case (plan §270) a constraint
    -- violation rather than a hand-rolled check somebody can forget.
    path TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN (
        'TranscriptEvents', 'RoutingMetadata', 'Approvals',
        'ArtifactManifest', 'Patch', 'EnvironmentDiagnostics'
    )),
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
    byte_length INTEGER NOT NULL CHECK (byte_length >= 0),
    media_type TEXT NOT NULL,
    -- `DataClassification`. An importer may only ever RAISE this above the
    -- operator ceiling, never lower it (conventions §8) — so the exported value
    -- is a floor, not a fact to be re-derived.
    classification TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    PRIMARY KEY (bundle_id, path)
);

CREATE INDEX idx_session_bundle_entries_hash
    ON session_bundle_entries (sha256);

-- One row per completed import.
CREATE TABLE session_bundle_imports (
    id TEXT PRIMARY KEY,
    owner_uid INTEGER NOT NULL,
    -- `BundleImportRequest.bundle` as uploaded. The ownership gate resolves this
    -- through `NamedResource::Artifact` before the handler runs.
    bundle_artifact_id TEXT NOT NULL REFERENCES artifacts(id),
    -- SHA-256 of the bytes this daemon ACTUALLY read and hashed — never the
    -- `ArtifactRef.sha256` the client asserted. The two disagreeing is the
    -- "hash mismatch" hostile case.
    bundle_sha256 TEXT NOT NULL CHECK (length(bundle_sha256) = 64),
    -- The hash asserted by the manifest inside the verified archive.
    manifest_sha256 TEXT NOT NULL CHECK (length(manifest_sha256) = 64),
    format_version INTEGER NOT NULL,
    collision_policy TEXT NOT NULL
        CHECK (collision_policy IN ('Reject', 'Remap', 'Skip')),
    imported_at TEXT NOT NULL,
    -- `BundleImportReceipt.skipped_entries`. Reported, never inferred.
    skipped_entries INTEGER NOT NULL DEFAULT 0 CHECK (skipped_entries >= 0),
    command_id TEXT REFERENCES commands(id)
);

-- Deliberately NOT unique on (owner_uid, bundle_sha256): under `Remap` a second
-- import of the same archive is a legitimate request for a second copy.
-- Idempotency belongs to `commands.idempotency_key` (UNIQUE, 0002), which is
-- what a retried delivery replays against.
CREATE INDEX idx_session_bundle_imports_owner
    ON session_bundle_imports (owner_uid, imported_at DESC, id);
CREATE INDEX idx_session_bundle_imports_hash
    ON session_bundle_imports (bundle_sha256);

-- `BundleIdentityMapping`: source identity -> freshly minted local identity.
CREATE TABLE session_bundle_identity_map (
    import_id TEXT NOT NULL REFERENCES session_bundle_imports(id),
    kind TEXT NOT NULL CHECK (kind IN (
        'Session', 'Run', 'Artifact', 'Approval', 'ChangeSet'
    )),
    -- Opaque. The exporting daemon's id, retained as provenance ONLY. It is
    -- never used as a local key: reusing it is how an import would resurrect a
    -- tombstoned session (`sessions.tombstoned_at`, 0040).
    source_id TEXT NOT NULL,
    -- The local id this import minted. An 'Approval' row records that a source
    -- approval was SEEN AND DROPPED — approvals are never restored — so
    -- `local_id` may be empty for that kind, and there is deliberately no
    -- foreign key (the column spans five different tables).
    local_id TEXT NOT NULL,
    PRIMARY KEY (import_id, kind, source_id)
);

CREATE INDEX idx_session_bundle_identity_local
    ON session_bundle_identity_map (kind, local_id);

-- Provenance label on every session an import created, so the Session Library
-- and any later publication decision can tell imported content from local
-- content without a join through five tables. NULL = created locally.
ALTER TABLE sessions ADD COLUMN imported_from_bundle TEXT
    REFERENCES session_bundle_imports(id);

CREATE INDEX idx_sessions_imported_bundle
    ON sessions (imported_from_bundle) WHERE imported_from_bundle IS NOT NULL;
