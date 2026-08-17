-- Time-bounded leases with generation-matched renewal.

CREATE TABLE runner_leases (
    id                  UUID PRIMARY KEY,
    job_id              UUID NOT NULL REFERENCES runner_jobs(id) ON DELETE CASCADE,
    attempt_id          UUID NOT NULL REFERENCES runner_job_attempts(id) ON DELETE CASCADE,
    runner_id           UUID NOT NULL REFERENCES runners(id),
    -- Monotonic per lease. Renewal must present the CURRENT generation; a
    -- renewal at generation N-1 is a stale message from a partitioned runner and
    -- is refused. This is what makes replayed renew messages inert (plan 8.8).
    generation          BIGINT NOT NULL DEFAULT 1 CHECK (generation > 0),
    -- Opaque high-entropy lease secret, stored HASHED (sha256). The runner holds
    -- the plaintext; the control plane only ever compares hashes, so a database
    -- read does not yield a usable lease credential.
    lease_token_hash    BYTEA NOT NULL,
    acquired_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Absolute expiry. The scheduler's reaper transitions expired leases; the
    -- runner must self-terminate at this instant WITHOUT waiting to be told,
    -- because a partitioned runner cannot be told.
    expires_at          TIMESTAMPTZ NOT NULL,
    last_heartbeat_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- active | released | expired | revoked
    state               TEXT NOT NULL DEFAULT 'active',
    released_at         TIMESTAMPTZ,
    CONSTRAINT runner_leases_attempt UNIQUE (attempt_id)
);

-- One active lease per job at a time. The partial unique index is the
-- structural guarantee; the claim transaction relies on it rather than on
-- application-level care.
CREATE UNIQUE INDEX ux_runner_leases_one_active
    ON runner_leases (job_id) WHERE state = 'active';

-- The reaper's scan: active leases past expiry, cheapest first.
CREATE INDEX ix_runner_leases_expiry
    ON runner_leases (expires_at) WHERE state = 'active';

CREATE INDEX ix_runner_leases_runner
    ON runner_leases (runner_id) WHERE state = 'active';

-- Bounded live logs. Chunked, append-only, per attempt. Bytes stay in object
-- storage past a threshold; small chunks inline. Retention is a policy sweep,
-- not a cascade from the job.
CREATE TABLE runner_log_chunks (
    id                  BIGSERIAL PRIMARY KEY,
    attempt_id          UUID NOT NULL REFERENCES runner_job_attempts(id) ON DELETE CASCADE,
    -- Monotonic per attempt. A duplicate (attempt_id, sequence) is an at-least-
    -- once redelivery and is IGNORED, not appended twice.
    sequence            BIGINT NOT NULL,
    stream              TEXT NOT NULL CHECK (stream IN ('stdout', 'stderr')),
    -- Inline bytes for a small chunk, else NULL with object_key set.
    body                BYTEA,
    object_key          TEXT,
    byte_length         INTEGER NOT NULL CHECK (byte_length >= 0),
    -- TRUE when the runner truncated at the profile's maximum_output_mb ceiling.
    -- An honest marker, so a reader never mistakes a bounded log for a complete
    -- one.
    truncated           BOOLEAN NOT NULL DEFAULT FALSE,
    received_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT runner_log_chunks_sequence UNIQUE (attempt_id, sequence),
    CONSTRAINT runner_log_chunks_body_xor
        CHECK ((body IS NULL) <> (object_key IS NULL))
);
