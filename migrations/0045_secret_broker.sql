-- Milestone 5: the secret broker's durable metadata.
--
-- INVARIANT, enforced by review and by the sentinel scan in the exit gate:
-- NO TABLE IN THIS FILE HAS A COLUMN THAT CAN HOLD SECRET MATERIAL. A reference
-- names where material lives; a lease records that material was issued; the
-- audit records that it was used. None of them stores it. If a future column
-- looks like it might carry a value, it is wrong.

-- An opaque, stable handle to credential material held by some backend.
CREATE TABLE secret_references (
    id TEXT PRIMARY KEY,
    -- Kernel-derived from peer credentials at creation, never from the wire —
    -- the same rule as automation_bindings.owner_uid and artifacts.owner_uid
    -- (migrations/0039_artifact_owner.sql).
    owner_uid INTEGER NOT NULL,
    -- The operator-facing name a manifest declares in `capabilities.secrets`
    -- and a guest passes as HostRequest::ReadSecret.name
    -- (crates/sandbox/src/gate.rs:113). This is what the declaration ceiling
    -- matches on at crates/sandbox/src/gate.rs:314.
    name TEXT NOT NULL,
    backend TEXT NOT NULL CHECK (backend IN
        ('environment', 'keychain', 'managed', 'vault', 'workload_identity')),
    -- Backend-specific, NON-SECRET locator: an env var NAME, a keychain
    -- service/account pair, a Vault path, an audience. Never a value.
    locator TEXT NOT NULL,
    -- The capability the material may be used for (`github.api`, `slack.chat`).
    -- A lease is bound to this at issue; a job cannot widen it afterwards
    -- (design spec §9: "A runner cannot ask for a broader secret after
    -- accepting a job").
    capability TEXT NOT NULL,
    -- Optional narrowing scopes. NULL means "not scoped", never "any".
    organization_id TEXT,
    repository_id TEXT,
    -- Digest of the accepted reference as presented to the principal at
    -- acceptance time. A later resolve recomputes it and refuses on mismatch,
    -- so swapping the locator under an accepted reference is detectable. Same
    -- approve-then-substitute defence as hooks.approved_content_hash
    -- (migrations/0027_hooks.sql).
    accepted_digest TEXT NOT NULL,
    -- Rotation is recorded, not destructive: audit rows must stay resolvable.
    created_at TEXT NOT NULL,
    rotated_at TEXT,
    revoked_at TEXT,
    revoked_reason TEXT CHECK (revoked_reason IS NULL OR revoked_at IS NOT NULL),
    UNIQUE (owner_uid, name, capability)
);

CREATE INDEX idx_secret_references_owner
    ON secret_references (owner_uid, revoked_at, name);
CREATE INDEX idx_secret_references_repository
    ON secret_references (repository_id) WHERE repository_id IS NOT NULL;

-- One short-lived issuance. A lease is the *record* that material was handed to
-- a bounded context; the material itself lives only in non-Clone, non-Serialize
-- memory for the duration of one transport injection.
CREATE TABLE secret_leases (
    id TEXT PRIMARY KEY,
    reference_id TEXT NOT NULL REFERENCES secret_references(id),
    -- The full LeaseContext, all five axes from design spec §9. Every one is
    -- server-derived; a job that asks for a lease it was not admitted for is
    -- refused, not narrowed.
    principal_uid INTEGER NOT NULL,
    organization_id TEXT,
    repository_id TEXT,
    job_id TEXT NOT NULL,               -- run id / workflow run id / plugin instance
    capability TEXT NOT NULL,
    -- Idempotency: the same job asking twice for the same capability gets the
    -- SAME lease rather than a second one, so a retried node does not multiply
    -- outstanding credentials.
    issue_key TEXT NOT NULL,
    issued_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    -- Backend-reported handle for revocation (a Vault lease id, a token jti).
    -- Non-secret by construction; if a backend's handle IS the credential, do
    -- not store it.
    backend_lease_handle TEXT,
    state TEXT NOT NULL CHECK (state IN ('active', 'expired', 'revoked', 'failed')),
    revoked_at TEXT,
    revoked_reason TEXT,
    CHECK (state <> 'revoked' OR revoked_at IS NOT NULL),
    UNIQUE (issue_key)
);

CREATE INDEX idx_secret_leases_reference ON secret_leases (reference_id, issued_at DESC);
CREATE INDEX idx_secret_leases_job ON secret_leases (job_id, state);
CREATE INDEX idx_secret_leases_expiry ON secret_leases (expires_at) WHERE state = 'active';

-- Append-only, non-secret audit. Design spec §9: "Every lease, use, denial,
-- rotation, and revocation is audited without recording values."
CREATE TABLE secret_audit (
    id TEXT PRIMARY KEY,
    -- Nullable because a DENIAL may have no reference (an unknown name) and no
    -- lease. A denial with a NULL reference must still be recorded, or the most
    -- interesting events are the ones that leave no trace.
    reference_id TEXT REFERENCES secret_references(id),
    lease_id TEXT REFERENCES secret_leases(id),
    event TEXT NOT NULL CHECK (event IN
        ('issued', 'used', 'denied', 'expired', 'rotated', 'revoked', 'backend_error')),
    principal_uid INTEGER NOT NULL,
    job_id TEXT,
    capability TEXT,
    -- A DOTTED CODE ONLY (`secrets.unknown-reference`, `policy.denied`,
    -- `secrets.backend-unavailable`). Never a rendered message and never
    -- backend output: a Vault error body can echo a path or a token prefix.
    outcome_code TEXT NOT NULL,
    -- The name the guest asked for, so an operator can see WHAT was requested.
    -- Requested names are declared in manifests and are not secret; the value
    -- behind the name never appears anywhere in this table.
    requested_name TEXT,
    occurred_at TEXT NOT NULL
);

CREATE INDEX idx_secret_audit_reference ON secret_audit (reference_id, occurred_at DESC);
CREATE INDEX idx_secret_audit_job ON secret_audit (job_id, occurred_at DESC);
CREATE INDEX idx_secret_audit_event ON secret_audit (event, occurred_at DESC);
