-- Output manifests and signed execution attestations.

CREATE TABLE runner_outputs (
    id                  UUID PRIMARY KEY,
    attempt_id          UUID NOT NULL REFERENCES runner_job_attempts(id) ON DELETE CASCADE,
    -- The declared output name from the JobSpec. An output the spec did not
    -- declare is REFUSED at upload, not stored and ignored.
    name                TEXT NOT NULL,
    -- 'sha256:<hex>', same canonical form as
    -- codypendent_sandbox::verify::checksum_of (crates/sandbox/src/verify.rs:67).
    content_hash        TEXT NOT NULL,
    byte_length         BIGINT NOT NULL CHECK (byte_length >= 0),
    media_type          TEXT NOT NULL,
    object_key          TEXT NOT NULL,
    -- Inherited from the job's data_classification; an output is never LESS
    -- classified than the input that produced it (design §6.4: indexes and edges
    -- inherit the strictest classification of their sources).
    classification      TEXT NOT NULL,
    -- pending | verified | mismatched. A job stays 'uploading' until every
    -- declared output row is 'verified' (design §11.4).
    verify_state        TEXT NOT NULL DEFAULT 'pending',
    uploaded_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    verified_at         TIMESTAMPTZ,
    CONSTRAINT runner_outputs_name UNIQUE (attempt_id, name)
);

CREATE INDEX ix_runner_outputs_unverified
    ON runner_outputs (attempt_id) WHERE verify_state <> 'verified';

CREATE TABLE runner_attestations (
    id                  UUID PRIMARY KEY,
    attempt_id          UUID NOT NULL REFERENCES runner_job_attempts(id) ON DELETE CASCADE,
    job_id              UUID NOT NULL REFERENCES runner_jobs(id) ON DELETE CASCADE,
    lease_id            UUID NOT NULL REFERENCES runner_leases(id),
    runner_id           UUID NOT NULL REFERENCES runners(id),
    -- Scheme tag, e.g. 'codypendent-runner-attestation-v1'. A signature only
    -- verifies under the scheme it was produced with; a future scheme bumps the
    -- tag so the two never collide. Mirrors
    -- codypendent_sandbox::verify::signing_digest's domain separation
    -- (crates/sandbox/src/verify.rs:89-97).
    scheme              TEXT NOT NULL,
    -- The canonical statement bytes that were signed. Stored verbatim so
    -- verification is reproducible from this row alone, years later.
    statement           BYTEA NOT NULL,
    -- sha256 over `scheme || len_be64(statement) || statement`.
    statement_digest    BYTEA NOT NULL,
    signature           BYTEA NOT NULL,          -- Ed25519, 64 bytes
    -- The runner key the signature verified against, captured at verification
    -- time. A later key revocation therefore does not rewrite history — it
    -- invalidates FUTURE attestations only.
    signer_pubkey       BYTEA NOT NULL,
    -- verified | bad-signature | unknown-signer | revoked-signer
    -- | lease-mismatch | hash-mismatch | malformed
    verify_result       TEXT NOT NULL,
    verified_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT runner_attestations_attempt UNIQUE (attempt_id)
);

CREATE INDEX ix_runner_attestations_job ON runner_attestations (job_id);

-- Suspicious outputs are quarantined, never silently dropped: the evidence must
-- survive for the operator to inspect (design §11.4, §12.2 append-only audit).
CREATE TABLE runner_quarantine (
    id                  UUID PRIMARY KEY,
    job_id              UUID NOT NULL REFERENCES runner_jobs(id) ON DELETE CASCADE,
    attempt_id          UUID NOT NULL REFERENCES runner_job_attempts(id) ON DELETE CASCADE,
    -- attestation-invalid | hash-mismatch | undeclared-output | revoked-image
    reason              TEXT NOT NULL,
    detail              JSONB NOT NULL DEFAULT '{}'::JSONB,
    quarantined_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
