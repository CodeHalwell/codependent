-- Runner registration and the job queue. PostgreSQL; forward-only.
-- Foreign keys reference M7's 0001_identity.sql / 0002 repository tables.

CREATE TABLE runners (
    id                  UUID PRIMARY KEY,
    organization_id     UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- Stable operator-chosen name, unique inside the org: how a human recognises
    -- this runner in the UI and in audit rows.
    name                TEXT NOT NULL,
    -- 'container' | 'kubernetes' | 'microvm' | 'macos'. The deployment shape, NOT
    -- a trust level: every kind claims through the identical protocol (design §7.4).
    kind                TEXT NOT NULL,
    os                  TEXT NOT NULL,   -- 'linux' | 'macos'
    arch                TEXT NOT NULL,   -- 'x86_64' | 'aarch64'
    -- SandboxBackend::as_str() equivalent ('seatbelt' | 'bubblewrap' | 'none').
    -- 'none' MUST NOT be eligible for any job: see §6.1.
    sandbox_backend     TEXT NOT NULL,
    -- Advertised capabilities: tool names/versions, image digest, region, policy
    -- labels. Advertised means CLAIMED — eligibility filters on it, attestation
    -- proves it after the fact (§4.2).
    capabilities        JSONB NOT NULL,
    -- Data-residency region the operator assigned. Compared against the job's
    -- required region; NULL means "no region asserted" and matches only jobs with
    -- no region requirement.
    region              TEXT,
    -- Ed25519 public key (32 raw bytes) this runner signs attestations with.
    -- Rotating a key is an UPDATE plus an audit row; a revoked key sets
    -- revoked_at and every attestation signed after that instant is rejected.
    attestation_pubkey  BYTEA NOT NULL,
    revoked_at          TIMESTAMPTZ,
    -- Max concurrent leases this runner may hold. Enforced at claim time.
    max_concurrency     INTEGER NOT NULL DEFAULT 1 CHECK (max_concurrency > 0),
    registered_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at        TIMESTAMPTZ,
    CONSTRAINT runners_name_unique UNIQUE (organization_id, name)
);

CREATE INDEX ix_runners_eligible
    ON runners (organization_id, os, arch, region)
    WHERE revoked_at IS NULL;

CREATE TABLE runner_jobs (
    id                  UUID PRIMARY KEY,
    organization_id     UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    repository_id       UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    -- The submitting daemon's workload identity (M7 0001). Authorization is
    -- re-derived from the token on every call; this column is provenance, never
    -- an authority source (design §4.1, plan rule 21).
    submitted_by_daemon UUID NOT NULL REFERENCES daemons(id),
    -- Caller-chosen idempotency key. A duplicate submission returns the EXISTING
    -- job receipt (design §11.4) rather than creating a second job. For workflow
    -- dispatch this is derived deterministically — see §5.3.
    idempotency_key     TEXT NOT NULL,
    -- The full JobSpec: argv, workspace layout, input manifest ref, SandboxSpec,
    -- ResourceSpec, output declarations. Immutable after insert.
    job_spec            JSONB NOT NULL,
    -- sha256 of the canonical job_spec bytes. Bound into the attestation so a
    -- runner cannot claim to have executed a different specification.
    job_spec_hash       TEXT NOT NULL,
    -- Content-addressed input bundle: 'sha256:<hex>' of the input manifest.
    input_manifest_hash TEXT NOT NULL,
    -- Eligibility requirements, ANDed at claim: required tool capabilities,
    -- image digest, os/arch, region, policy labels.
    eligibility         JSONB NOT NULL,
    -- The strictest DataClassification of anything in the input bundle, as
    -- codypendent_routing::DataClassification's wire string. The scheduler
    -- refuses to place a job whose classification the org's off-device ceiling
    -- forbids — see §6.4. NOT NULL: an unclassified job is 'unknown' and
    -- 'unknown' fails closed, it does not mean "safe".
    data_classification TEXT NOT NULL,
    -- Budget ceiling in micro-USD. NULL = no ceiling declared, which is NOT
    -- "unlimited": the scheduler treats NULL as ineligible for any org that
    -- requires a ceiling. Never coerce a missing budget to 0.
    budget_micro_usd    BIGINT CHECK (budget_micro_usd IS NULL OR budget_micro_usd > 0),
    -- queued | leased | executing | uploading | verifying | succeeded | failed
    -- | cancelled | quarantined. Terminal set: succeeded/failed/cancelled/
    -- quarantined. Every terminal write is a compare-and-set (§4.5).
    state               TEXT NOT NULL DEFAULT 'queued',
    -- The one attempt whose outputs were ACCEPTED. Set exactly once, by the
    -- terminal CAS. The partial unique index below is the database-level
    -- enforcement of acceptance criterion 13.
    accepted_attempt_id UUID,
    attempt_count       INTEGER NOT NULL DEFAULT 0,
    max_attempts        INTEGER NOT NULL DEFAULT 1 CHECK (max_attempts > 0),
    -- Set by a cancel request that arrives before or during a lease. The claim
    -- path consumes it, exactly like RunControlRegistry::register consumes a
    -- pending cancellation (crates/codypendentd/src/executor.rs:159-167) — but
    -- durably, so it survives a scheduler restart.
    cancel_requested_at TIMESTAMPTZ,
    cancel_requested_by UUID,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    terminal_at         TIMESTAMPTZ,
    CONSTRAINT runner_jobs_idempotent UNIQUE (organization_id, idempotency_key)
);

-- The claim query's index: queued jobs in submission order, per org.
CREATE INDEX ix_runner_jobs_queue
    ON runner_jobs (organization_id, created_at)
    WHERE state = 'queued';

CREATE INDEX ix_runner_jobs_repository ON runner_jobs (repository_id, created_at DESC);

CREATE TABLE runner_job_attempts (
    id                  UUID PRIMARY KEY,
    job_id              UUID NOT NULL REFERENCES runner_jobs(id) ON DELETE CASCADE,
    -- 1-based, monotonic per job. UNIQUE with job_id so a retry can never
    -- silently reuse an attempt number and collide with its predecessor's
    -- artifacts or attestation.
    attempt_number      INTEGER NOT NULL CHECK (attempt_number > 0),
    runner_id           UUID NOT NULL REFERENCES runners(id),
    -- claimed | executing | uploading | verified | rejected | expired | cancelled
    state               TEXT NOT NULL DEFAULT 'claimed',
    -- The image the runner asserts it executed under, as a digest
    -- ('sha256:<hex>'). Compared against runner_images at verification; an
    -- unknown or revoked digest quarantines (§4.6).
    image_digest        TEXT,
    exit_code           INTEGER,
    -- Free-form failure detail for a rejected/expired attempt. Never used as an
    -- authority signal; diagnostics only.
    failure_reason      TEXT,
    started_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at            TIMESTAMPTZ,
    CONSTRAINT runner_job_attempts_number UNIQUE (job_id, attempt_number)
);

CREATE INDEX ix_runner_job_attempts_job ON runner_job_attempts (job_id, attempt_number);

-- Acceptance criterion 13, enforced by the database rather than by care:
-- at most one attempt per job may ever be the accepted one.
CREATE UNIQUE INDEX ux_runner_jobs_one_accepted
    ON runner_jobs (id) WHERE accepted_attempt_id IS NOT NULL;

ALTER TABLE runner_jobs
    ADD CONSTRAINT runner_jobs_accepted_attempt
    FOREIGN KEY (accepted_attempt_id) REFERENCES runner_job_attempts(id);

-- Revocable runner images (design §12.2, "revocable runner images and publisher
-- keys"). An attempt executing under a digest revoked mid-run is quarantined at
-- verification, not retroactively accepted.
CREATE TABLE runner_images (
    digest              TEXT PRIMARY KEY,          -- 'sha256:<hex>'
    organization_id     UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    display_name        TEXT NOT NULL,
    approved_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at          TIMESTAMPTZ,
    revoked_reason      TEXT
);
