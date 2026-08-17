-- Append-only by DATABASE PERMISSION, not by convention (plan Task 7.2).
CREATE TABLE audit_records (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    actor_kind text NOT NULL CHECK (actor_kind IN ('user', 'daemon', 'system')),
    actor_id uuid,
    action text NOT NULL,
    target_kind text NOT NULL,
    target_id text NOT NULL,
    -- Digest of the exact action, mirroring the local approvals convention at
    -- crates/daemon/src/approvals.rs:970 so a local and remote record of the
    -- same action are comparable.
    action_digest bytea NOT NULL,
    correlation_id uuid,
    -- Hash chain over (prev_hash || canonical row bytes), per organization.
    -- Makes deletion or reordering detectable even by someone with table
    -- ownership, which append-only permissions alone do not.
    prev_hash bytea,
    record_hash bytea NOT NULL,
    -- Never a secret value and never product content: metadata about the
    -- action only (design §12.2).
    detail jsonb NOT NULL DEFAULT '{}'::jsonb,
    occurred_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ON audit_records (organization_id, occurred_at DESC);
CREATE UNIQUE INDEX ON audit_records (organization_id, record_hash);

-- Two roles: the migration role owns the schema, the runtime role cannot
-- UPDATE or DELETE audit rows. Self-hosters get this in the deployment docs.
REVOKE UPDATE, DELETE ON audit_records FROM PUBLIC;
