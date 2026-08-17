-- Milestone 5: durable marketplace distribution, trust and lifecycle.
--
-- The sandbox crate remains the final execution authority: this schema records
-- WHAT was distributed and WHETHER it was approved. Whether a package may RUN
-- is still decided by InstalledPlugin's lifecycle state
-- (crates/sandbox/src/lifecycle.rs:127) and by the two gates in
-- crates/sandbox/src/gate.rs. Never let a row here become an execution grant.

-- Publisher trust is DISTINCT from registry trust (plan Task 5.4): trusting the
-- registry that lists a package says nothing about who signed it.
CREATE TABLE marketplace_publishers (
    id TEXT PRIMARY KEY,                    -- the manifest's `publisher` string
    display_name TEXT NOT NULL,
    -- Raw ed25519 public key, hex. Public by definition; verify_artifact takes
    -- exactly 32 bytes (crates/sandbox/src/verify.rs:160-164).
    public_key_hex TEXT NOT NULL CHECK (length(public_key_hex) = 64),
    trust_tier TEXT NOT NULL DEFAULT 'untrusted'
        CHECK (trust_tier IN ('untrusted', 'trusted', 'first_party')),
    trusted_at TEXT,
    trusted_by TEXT,
    -- Revocation is a state, not a delete: revoking must be able to disable
    -- already-installed packages, which requires the key to remain resolvable.
    revoked_at TEXT,
    revoked_reason TEXT,
    created_at TEXT NOT NULL,
    CHECK (trust_tier = 'untrusted' OR trusted_at IS NOT NULL),
    UNIQUE (id, public_key_hex)
);

CREATE INDEX idx_marketplace_publishers_trust
    ON marketplace_publishers (trust_tier, revoked_at);

-- Stable package identity. Metadata only; nothing here is executable.
CREATE TABLE marketplace_packages (
    id TEXT PRIMARY KEY,                    -- the manifest's `id` slug
    publisher_id TEXT NOT NULL REFERENCES marketplace_publishers(id),
    kind TEXT NOT NULL CHECK (kind IN
        ('native-process', 'wasm-component', 'mcp-remote', 'ui-component', 'theme-pack')),
    display_name TEXT NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    -- A package the org policy hides must be non-disclosing: a query for it
    -- answers exactly as a query for a package that does not exist (plan Task
    -- 5.4, "hidden-package non-disclosure").
    hidden INTEGER NOT NULL DEFAULT 0 CHECK (hidden IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_marketplace_packages_publisher ON marketplace_packages (publisher_id);
CREATE INDEX idx_marketplace_packages_visible ON marketplace_packages (hidden, id);

-- IMMUTABLE once written. A version row is the durable statement "these bytes,
-- under this manifest, from this source". Updating any column here would let a
-- published version be swapped after review; publish a new version instead.
CREATE TABLE marketplace_versions (
    id TEXT PRIMARY KEY,
    package_id TEXT NOT NULL REFERENCES marketplace_packages(id),
    version TEXT NOT NULL,
    -- `sha256:<hex>` in the canonical form checksum_of produces
    -- (crates/sandbox/src/verify.rs:67). Also the content-addressed store key.
    content_hash TEXT NOT NULL CHECK (content_hash LIKE 'sha256:%'),
    -- The manifest TOML as published. Stored verbatim because signing_digest
    -- covers the WHOLE manifest (crates/sandbox/src/verify.rs:98): a
    -- re-serialized copy would not re-verify.
    manifest_toml TEXT NOT NULL,
    signature_b64 TEXT,
    signed INTEGER NOT NULL CHECK (signed IN (0, 1)),
    -- The source the artifact was fetched from, retained for audit and for the
    -- allowlist check on re-download. Redirects are followed only within the
    -- allowlist and the final URL is what is recorded.
    source_url TEXT NOT NULL,
    artifact_bytes INTEGER NOT NULL CHECK (artifact_bytes > 0),
    -- Host-computed, never publisher-asserted: a package cannot declare itself
    -- compatible (plan Task 5.3, "host computes compatibility").
    min_daemon_version TEXT,
    max_daemon_version TEXT,
    published_at TEXT NOT NULL,
    yanked_at TEXT,
    CHECK (signed = 0 OR signature_b64 IS NOT NULL),
    UNIQUE (package_id, version),
    UNIQUE (content_hash)
);

CREATE INDEX idx_marketplace_versions_package
    ON marketplace_versions (package_id, published_at DESC);

-- One row per local installation. Owner-scoped so the desktop can show "my
-- installs", but note the ownership gate treats the store as daemon-wide (§3).
CREATE TABLE marketplace_installs (
    id TEXT PRIMARY KEY,
    package_id TEXT NOT NULL REFERENCES marketplace_packages(id),
    version_id TEXT NOT NULL REFERENCES marketplace_versions(id),
    owner_uid INTEGER NOT NULL,
    -- Mirrors LifecycleState (crates/sandbox/src/lifecycle.rs:42). The sandbox
    -- value is authoritative at execution; this column exists so the daemon can
    -- answer list/inspect without loading every package.
    lifecycle TEXT NOT NULL CHECK (lifecycle IN
        ('installed_disabled', 'smoke_tested', 'enabled', 'disabled', 'revoked')),
    -- Pinning is exact-version and explicit; an update check must not move a
    -- pinned install even when a newer compatible version exists.
    pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    pinned_version TEXT CHECK (pinned = 1 OR pinned_version IS NULL),
    -- The optional session/scope an enable is limited to, mirroring
    -- InstalledPlugin::enabled_scope (crates/sandbox/src/lifecycle.rs:185).
    enabled_scope TEXT CHECK (lifecycle = 'enabled' OR enabled_scope IS NULL),
    -- Set when a publisher key or version is revoked. A revoked install must be
    -- refused at launch even if `lifecycle` was 'enabled' a moment earlier.
    revoked_at TEXT,
    revoked_reason TEXT,
    installed_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    -- One install per package per principal; a second install is an update.
    UNIQUE (package_id, owner_uid)
);

CREATE INDEX idx_marketplace_installs_owner
    ON marketplace_installs (owner_uid, lifecycle, updated_at DESC, id);
CREATE INDEX idx_marketplace_installs_version ON marketplace_installs (version_id);

-- The human decision on a permission change. Bound to the exact manifest hash
-- that was reviewed, so approve-then-substitute fails closed — the same binding
-- hooks.approved_content_hash uses (migrations/0027_hooks.sql:31-35).
CREATE TABLE marketplace_permission_receipts (
    id TEXT PRIMARY KEY,
    install_id TEXT NOT NULL REFERENCES marketplace_installs(id) ON DELETE CASCADE,
    from_version_id TEXT REFERENCES marketplace_versions(id),
    to_version_id TEXT NOT NULL REFERENCES marketplace_versions(id),
    -- The rendered PermissionDiff (crates/sandbox/src/permission.rs:413) — the
    -- text the human was actually shown, kept so an audit can reconstruct it.
    diff_rendered TEXT NOT NULL,
    -- diff.expands_permissions() (permission.rs:383). An expansion may never be
    -- auto-approved, regardless of publisher trust tier.
    expands_permissions INTEGER NOT NULL CHECK (expands_permissions IN (0, 1)),
    approved_manifest_hash TEXT NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('pending', 'approved', 'rejected')),
    decided_by TEXT,
    decided_at TEXT,
    -- Revocation invalidates PENDING receipts (plan Task 5.4), so a receipt
    -- approved after its publisher was revoked cannot be spent.
    invalidated_at TEXT,
    created_at TEXT NOT NULL,
    CHECK (decision = 'pending' OR decided_at IS NOT NULL)
);

CREATE INDEX idx_marketplace_permission_receipts_install
    ON marketplace_permission_receipts (install_id, created_at DESC);
CREATE INDEX idx_marketplace_permission_receipts_pending
    ON marketplace_permission_receipts (decision) WHERE decision = 'pending';

-- Append-only revocation feed. Kept separate from the publisher/version rows so
-- a revocation is a durable event with provenance, not a mutable flag, and so a
-- replay can reconstruct what was known when.
CREATE TABLE marketplace_revocations (
    id TEXT PRIMARY KEY,
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('publisher', 'package', 'version')),
    subject_id TEXT NOT NULL,
    reason TEXT NOT NULL,
    -- Where the revocation came from: a local operator, or a signed registry
    -- feed. A registry-sourced revocation is only honoured from an allowlisted
    -- source, and never grants anything — revocation is one-way.
    source TEXT NOT NULL CHECK (source IN ('operator', 'registry')),
    recorded_at TEXT NOT NULL,
    UNIQUE (subject_kind, subject_id, recorded_at)
);

CREATE INDEX idx_marketplace_revocations_subject
    ON marketplace_revocations (subject_kind, subject_id);
