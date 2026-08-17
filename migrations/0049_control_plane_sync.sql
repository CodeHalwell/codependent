-- M7 Task 7.6 — outbound synchronization with an optional control plane.
--
-- Local-only operation must remain fully functional with every table here
-- empty. Nothing in this file may be read on a startup path that has no
-- pairing.

-- One row per paired control plane. Multiple rows are legal (a daemon may serve
-- a personal org and a work org), which is why nothing here is a singleton.
CREATE TABLE control_plane_pairings (
    id TEXT PRIMARY KEY,
    -- Kernel-derived uid of the human who confirmed the pairing dialog. A
    -- pairing is an act by a person at this machine, never by the daemon.
    owner_uid INTEGER NOT NULL,
    -- Base URL, normalized and scheme-checked. Stored so a moved endpoint is a
    -- visible change rather than a silent redirect.
    endpoint TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    organization_display_name TEXT NOT NULL,
    -- Exactly what the consent dialog displayed, serialized. Persisted so a
    -- later audit can prove what the human agreed to — design §5.2 requires the
    -- display to name organization, repositories, sync classes, approval
    -- authority, runner authority, expiry and revocation.
    consent_manifest TEXT NOT NULL,
    consent_manifest_hash TEXT NOT NULL CHECK (length(consent_manifest_hash) = 64),
    -- The MOST PERMISSIVE class this pairing may ever publish, chosen by the
    -- human. Intersected with (never replaced by) per-repository policy from
    -- 0047. Default is metadata-only per the locked decision in design §2.
    max_publication_class TEXT NOT NULL DEFAULT 'metadata-shared' CHECK (
        max_publication_class IN (
            'private-local', 'metadata-shared', 'content-shared',
            'organization-knowledge', 'public-marketplace'
        )
    ),
    -- Whether the control plane may deliver approval requests to this daemon,
    -- and whether it may dispatch runner jobs. Both default off; both are
    -- narrowing switches only — turning them on does not itself grant the
    -- remote anything the local policy denies.
    accepts_remote_approvals INTEGER NOT NULL DEFAULT 0 CHECK (accepts_remote_approvals IN (0, 1)),
    accepts_runner_dispatch INTEGER NOT NULL DEFAULT 0 CHECK (accepts_runner_dispatch IN (0, 1)),
    state TEXT NOT NULL CHECK (state IN ('pending', 'active', 'revoked', 'expired')),
    paired_at TEXT,
    expires_at TEXT,
    revoked_at TEXT,
    revoked_reason TEXT,
    created_at TEXT NOT NULL,
    UNIQUE (owner_uid, endpoint, organization_id),
    CHECK (state <> 'active' OR paired_at IS NOT NULL),
    CHECK (state <> 'revoked' OR revoked_at IS NOT NULL)
);

-- Credential handles. Design §6.3: reusable bearer tokens are NOT stored in
-- plaintext. This table stores a REFERENCE (OS keychain item id) plus a
-- verification hash — never the secret. The short-lived access token lives in
-- memory only and is never persisted at all.
CREATE TABLE control_plane_credentials (
    pairing_id TEXT PRIMARY KEY REFERENCES control_plane_pairings(id),
    -- e.g. 'keychain:codypendent.control-plane.<pairing_id>'. Resolving it is
    -- the OS keychain's job; this column is not a secret.
    credential_ref TEXT NOT NULL,
    -- SHA-256 of the refresh credential, so a rotation can be detected and a
    -- mismatch refused without the value ever being readable here.
    credential_hash TEXT NOT NULL CHECK (length(credential_hash) = 64),
    -- Audience and purpose the credential is bound to. Checked before every
    -- use: a token minted for sync must not be presentable to a runner
    -- endpoint (design §5.2).
    audience TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('sync', 'pairing')),
    issued_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    rotated_at TEXT
);

-- Durable outbound queue. Same shape as knowledge's index_outbox
-- (crates/knowledge/src/outbox.rs:55): the row is appended in the SAME
-- transaction as the authoritative local write, so a crash between the write
-- and the send cannot lose the delta.
CREATE TABLE control_plane_outbox (
    id TEXT PRIMARY KEY,
    pairing_id TEXT NOT NULL REFERENCES control_plane_pairings(id),
    -- 'session-summary' | 'run-summary' | 'inbox-entry' | 'graph-batch' |
    -- 'tombstone' | 'approval-decision' | 'usage-aggregate'
    delta_kind TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    -- Serialized control-plane-protocol payload, already redacted to
    -- `class`. Redaction happens at ENQUEUE time, not at send time: a policy
    -- narrowing between enqueue and send must never be able to send the wider
    -- payload, and re-reading local authority at send time is exactly how that
    -- happens.
    payload TEXT NOT NULL,
    class TEXT NOT NULL,
    payload_hash TEXT NOT NULL CHECK (length(payload_hash) = 64),
    -- Monotonic per pairing. The control plane echoes it back as the resume
    -- cursor; a gap is a hard error, not a skip.
    sequence INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    -- Set when the control plane returns a receipt. Rows are retained after
    -- acknowledgement (for a retention window) so a duplicate receipt can be
    -- recognized rather than reprocessed.
    acknowledged_at TEXT,
    remote_receipt TEXT,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error TEXT,
    UNIQUE (pairing_id, sequence),
    UNIQUE (pairing_id, delta_kind, subject_id, payload_hash)
);

CREATE INDEX idx_control_plane_outbox_pending
    ON control_plane_outbox (pairing_id, sequence)
    WHERE acknowledged_at IS NULL;

-- Inbound idempotency. At-least-once delivery means the control plane WILL
-- redeliver; this table is what stops a redelivered approval or schedule from
-- producing a second local effect (design §7.3).
CREATE TABLE control_plane_inbound_receipts (
    pairing_id TEXT NOT NULL REFERENCES control_plane_pairings(id),
    -- The control plane's idempotency key for this message.
    remote_message_id TEXT NOT NULL,
    message_kind TEXT NOT NULL,
    -- The local effect this message produced, so a redelivery returns the
    -- ORIGINAL outcome rather than re-applying or erroring.
    local_effect_id TEXT,
    outcome_hash TEXT NOT NULL CHECK (length(outcome_hash) = 64),
    received_at TEXT NOT NULL,
    PRIMARY KEY (pairing_id, remote_message_id)
);

-- Resume position for the inbound stream. Persist-before-publish: the cursor
-- advances only after the effect is committed, so a reconnect replays rather
-- than skips.
CREATE TABLE control_plane_sync_cursors (
    pairing_id TEXT NOT NULL REFERENCES control_plane_pairings(id),
    stream TEXT NOT NULL CHECK (stream IN (
        'notifications', 'approvals', 'schedules', 'runner-events', 'policy'
    )),
    cursor TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (pairing_id, stream)
);

-- Local id ↔ remote id map. The remote id is NEVER used as a local
-- authorization key; this exists so a redelivery can be correlated and so a
-- deletion can name the remote object in a tombstone.
CREATE TABLE control_plane_remote_objects (
    pairing_id TEXT NOT NULL REFERENCES control_plane_pairings(id),
    local_kind TEXT NOT NULL,
    local_id TEXT NOT NULL,
    remote_id TEXT NOT NULL,
    class TEXT NOT NULL,
    published_at TEXT NOT NULL,
    PRIMARY KEY (pairing_id, local_kind, local_id),
    UNIQUE (pairing_id, remote_id)
);

-- The narrowed-only cache of organization policy the control plane sent. It is
-- an INPUT to an intersection, never a source of permission: every read of it
-- is `local.strictest(remote)`. Nothing here can enable a local capability.
CREATE TABLE control_plane_policy_snapshot (
    pairing_id TEXT PRIMARY KEY REFERENCES control_plane_pairings(id),
    policy_version INTEGER NOT NULL,
    max_publication_class TEXT NOT NULL,
    max_classification TEXT NOT NULL,
    -- Serialized allow/deny lists (providers, models, regions, integrations).
    -- Applied as additional DENIES only; an allow entry absent locally does not
    -- become allowed.
    restrictions TEXT NOT NULL,
    received_at TEXT NOT NULL,
    payload_hash TEXT NOT NULL CHECK (length(payload_hash) = 64)
);
