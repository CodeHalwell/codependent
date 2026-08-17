CREATE TABLE daemons (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    -- The human who paired it. A daemon's authority is bounded by this user's
    -- grants at all times, re-evaluated per request — not frozen at pairing.
    paired_by uuid NOT NULL REFERENCES users(id),
    display_name text NOT NULL,
    -- Hash of the consent manifest the human confirmed locally. Compared on
    -- every reconnect: a daemon presenting a different manifest is refused, so
    -- scope cannot silently widen after consent.
    consent_manifest_hash bytea NOT NULL,
    max_publication_class text NOT NULL,
    accepts_remote_approvals boolean NOT NULL DEFAULT false,
    accepts_runner_dispatch boolean NOT NULL DEFAULT false,
    state text NOT NULL CHECK (state IN ('pending', 'active', 'revoked', 'expired')),
    paired_at timestamptz,
    revoked_at timestamptz,
    last_seen_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ON daemons (organization_id) WHERE state = 'active';

-- Pairing challenges. Single-use, short-lived, and bound to the user who
-- started them; a challenge is not transferable between users.
CREATE TABLE pairing_challenges (
    code_hash bytea PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    initiated_by uuid NOT NULL REFERENCES users(id),
    requested_scope jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    daemon_id uuid REFERENCES daemons(id)
);

-- Workload credentials (daemon and, in M8, runner). Only hashes.
CREATE TABLE workload_credentials (
    id uuid PRIMARY KEY,
    daemon_id uuid REFERENCES daemons(id),
    -- Audience and purpose are part of the row so validation is a lookup, not a
    -- claim-trust: a sync credential presented at a runner endpoint fails on the
    -- stored value, not on a JWT claim the holder could have influenced.
    audience text NOT NULL,
    purpose text NOT NULL CHECK (purpose IN ('sync', 'pairing', 'runner-job')),
    token_hash bytea NOT NULL UNIQUE,
    rotated_from uuid REFERENCES workload_credentials(id),
    issued_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz
);
CREATE INDEX ON workload_credentials (daemon_id) WHERE revoked_at IS NULL;
