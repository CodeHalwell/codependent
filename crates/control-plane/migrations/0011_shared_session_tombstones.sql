-- A session deletion must dominate a late or out-of-order summary, but only
-- inside the exact authorization scope that accepted it. The generic
-- tombstones table intentionally lacks repository_id and daemon_id, so it is
-- retained as the audit projection while this table drives resurrection
-- prevention.
CREATE TABLE shared_session_tombstones (
    organization_id uuid NOT NULL REFERENCES organizations(id),
    repository_id uuid NOT NULL REFERENCES repositories(id),
    daemon_id uuid NOT NULL REFERENCES daemons(id),
    remote_session_key text NOT NULL,
    tombstoned_at timestamptz NOT NULL,
    PRIMARY KEY (organization_id, repository_id, daemon_id, remote_session_key)
);
