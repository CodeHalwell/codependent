-- Shared, class-bounded projections. NOT transcripts: `title` is present only
-- when the delta's class is content-shared or wider, and the column is NULL
-- otherwise (never '').
CREATE TABLE shared_sessions (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    repository_id uuid NOT NULL REFERENCES repositories(id),
    daemon_id uuid NOT NULL REFERENCES daemons(id),
    -- The daemon's local SessionId, opaque here. Unique per daemon so a
    -- redelivered delta updates rather than duplicates.
    remote_session_key text NOT NULL,
    class text NOT NULL,
    title text,
    state text NOT NULL,
    started_at timestamptz NOT NULL,
    last_activity_at timestamptz,
    -- Tombstoned rows stay for the retention window so a redelivered older
    -- delta can be rejected as stale rather than resurrecting the row.
    tombstoned_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (daemon_id, remote_session_key)
);
CREATE INDEX ON shared_sessions (organization_id, repository_id, last_activity_at DESC)
    WHERE tombstoned_at IS NULL;

-- Every accepted delta, for replay defence and audit.
CREATE TABLE sync_receipts (
    id uuid PRIMARY KEY,
    daemon_id uuid NOT NULL REFERENCES daemons(id),
    -- The daemon's monotonic outbox sequence. UNIQUE per daemon: this single
    -- constraint is what makes at-least-once delivery idempotent.
    daemon_sequence bigint NOT NULL,
    delta_kind text NOT NULL,
    payload_hash bytea NOT NULL,
    class text NOT NULL,
    accepted_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (daemon_id, daemon_sequence)
);

CREATE TABLE tombstones (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    subject_kind text NOT NULL,
    subject_key text NOT NULL,
    reason text NOT NULL CHECK (reason IN ('deleted', 'narrowed', 'revoked')),
    created_at timestamptz NOT NULL DEFAULT now(),
    -- Consumed-before-new-deltas ordering is enforced against this column.
    applied_at timestamptz,
    UNIQUE (organization_id, subject_kind, subject_key, created_at)
);

-- Mutation idempotency. The key is scoped to the PRINCIPAL, not global:
-- a global unique index lets one tenant probe another tenant's keys by
-- observing a 409. See §5.4.
CREATE TABLE idempotency_keys (
    principal_kind text NOT NULL CHECK (principal_kind IN ('user', 'daemon')),
    principal_id uuid NOT NULL,
    key text NOT NULL,
    -- Digest of the request body. A repeat of the key with a DIFFERENT body is
    -- a client bug and must be rejected, not silently served the old response.
    request_hash bytea NOT NULL,
    response_status integer NOT NULL,
    response_body jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    PRIMARY KEY (principal_kind, principal_id, key)
);

-- Persist-before-publish event log backing every resumable stream. The
-- monotonic id IS the resume cursor.
CREATE TABLE stream_events (
    id bigserial PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    -- NULL = organization-scoped event. Non-NULL events are only delivered to
    -- principals with a grant on that repository.
    repository_id uuid REFERENCES repositories(id),
    stream text NOT NULL,
    payload jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ON stream_events (organization_id, stream, id);

-- Object-store metadata. PostgreSQL is authoritative for policy; the bucket
-- holds bytes only (design §6.2).
CREATE TABLE published_objects (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    repository_id uuid REFERENCES repositories(id),
    -- Content address. The storage key is derived from it, so two uploads of
    -- identical bytes converge and a wrong-hash upload cannot be committed.
    content_hash bytea NOT NULL,
    byte_length bigint NOT NULL CHECK (byte_length >= 0),
    media_type text NOT NULL,
    class text NOT NULL,
    encryption text NOT NULL DEFAULT 'none' CHECK (encryption IN ('none', 'envelope')),
    -- 'uploading' rows are invisible to every read path. A partial upload can
    -- therefore never be served or counted.
    state text NOT NULL CHECK (state IN ('uploading', 'available', 'tombstoned')),
    uploaded_by_daemon uuid REFERENCES daemons(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (organization_id, content_hash)
);
