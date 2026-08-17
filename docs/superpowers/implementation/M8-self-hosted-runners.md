# M8 — Self-hosted remote runners: implementation guide

**Companion to** `docs/superpowers/plans/2026-08-16-hybrid-platform-program.md` §Milestone 8
(tasks 8.1–8.8) and `docs/superpowers/specs/2026-08-16-hybrid-platform-program-design.md`
§4.1, §5.2, §7.4, §11, §12.

This document does **not** restate the plan's task list, **Files:** lines, or commit messages.
It supplies what the plan leaves to the implementer: the DDL, the wire semantics, the exact
reuse points in shipped code, the failure algebra, and the traps.

---

## 1. Status — verified before writing

Verified against the working tree at `b8e17bd` (branch `release/v0.9.0`):

| Path | State |
|---|---|
| `crates/runner/` | **absent** |
| `crates/runner-controller/` | **absent** |
| `crates/runner-provider/` | **absent** |
| `crates/codypendentd/src/remote_node_executor.rs` | **absent** |
| `crates/control-plane/` | **absent** (M7 creates it) |
| `crates/control-plane-protocol/` | **absent** (M7 creates it) |
| `crates/control-plane/migrations/` | **absent** |

The workspace members are enumerated at `Cargo.toml:3-23`; none of the above appear. Every
internal crate is declared as a path dependency under `[workspace.dependencies]`
(`Cargo.toml:39-55`) using the `codypendent-<name>` package-name convention — the new crates
must follow it (`codypendent-runner`, `codypendent-runner-controller`).

Nothing in M8 is a partial. It is all new code standing on shipped engines.

---

## 2. What M8 reuses rather than reimplements

Design §4.2 is explicit: *"Reuse `crates/sandbox`, `crates/workflow`, `crates/routing`,
`crates/eval`, and the existing runtime rather than wrapping or duplicating them."* Concretely:

### 2.1 OS confinement — `crates/sandbox`

The runner does **not** get a new confinement model. It gets the shipped one, plus a container
boundary outside it.

- `SandboxExecutor` (`crates/sandbox/src/executor.rs:409-430`) is the enforcement seam:
  `capability_report()`, `run(&SandboxProfile, &SandboxCommand)`, and
  `prepare_interactive(...) -> SandboxProcessSpec`. Its doc comment on `run` states the rule
  verbatim: *"Fails **closed** on any setup problem — it never runs the command unconfined."*
- `enforcing_executor()` (`crates/sandbox/src/executor.rs:436-451`) resolves `MacosSandbox`
  (Seatbelt) on macOS, `LinuxSandbox` (bubblewrap) on Linux, and returns
  `SandboxError::UnsupportedPlatform { platform }` everywhere else. Note: it returns the
  **error**, not `RefusingSandbox` — there is no unconfined fallback to accidentally take.
- `RefusingSandbox` (`crates/sandbox/src/executor.rs:457-496`) is the constructible-everywhere
  refusing implementation; every field of its `CapabilityReport` is `false` and both `run` and
  `prepare_interactive` return `UnsupportedPlatform`. Use it to test the refuse path on any
  host.
- Availability is probed by execute-bit, not existence (`is_executable_file`,
  `executor.rs:932-946`). A present-but-non-executable `bwrap` yields
  `SandboxError::ToolUnavailable { tool, diagnostic }` with *"refusing to run unconfined"* in
  the message (`executor.rs:386-392`, macOS probe `:980-983`, Linux probe `:1157-1162`).
- `SandboxProfile` (`crates/sandbox/src/profile.rs:28-51`) is the confinement vocabulary:
  `env_allowlist`, `read_paths`, `write_paths`, `network_allowlist`, `brokered_secrets`
  (*"never placed in env"*, `profile.rs:39-40`), `allow_subprocess`, `memory_mb`,
  `cpu_seconds`, `wall_seconds`, `maximum_output_mb`. `ENV_ALLOWLIST` is the fixed set
  `["PATH", "LANG", "LC_ALL", "TZ"]` (`profile.rs:24`). Query methods are deny-first:
  `allows_read` chains write paths (write implies read, `profile.rs:96-101`),
  `allows_network` is exact string equality over a list that is normally empty
  (`profile.rs:112-114`), and `path_within` (`profile.rs:126-152`) rejects any candidate
  containing `..` or `.` components.
- `SandboxProfile::derive(manifest, granted)` (`crates/sandbox/src/profile.rs:59-90`) already
  encodes narrow-never-broaden: it reads only the *granted* set, so the caller narrows by
  passing less. The ceiling itself is asserted in `crates/sandbox/src/lifecycle.rs:508`
  (`assert_granted_within_manifest` → `LifecycleError::GrantExceedsManifest`), and an update
  intersects rather than replaces (`lifecycle.rs:381`).
- `validate_enforceable_profile` (`crates/sandbox/src/executor.rs:1283-1313`) is the shipped
  deny-first gate both backends run *before spawning anything*. It refuses three things
  outright — read it before designing `SandboxSpec`, because two of them will surprise you:
  1. **a non-empty `network_allowlist`** →
     `UnsupportedCapability("host:port network allowlists require a broker; refusing
     unrestricted outbound access")` (`:1284-1289`);
  2. **any resource cap equal to zero** → `InvalidCommand("resource cap `{field}` must be
     greater than zero")` (`:1290-1301`) — zero means *invalid*, never *unlimited*;
  3. **an empty or root filesystem grant** → `InvalidCommand("root/empty filesystem grants are
     forbidden")` (`:1302-1311`).
- `CapabilityBroker` / `RunPolicyGate` / `DenyAllGate` (`crates/sandbox/src/gate.rs:267-368`)
  is the two-gate host-request path: the manifest is a *ceiling* (`SandboxProfile::permits`,
  `gate.rs:303-316`), the daemon's run policy is the *grant*, and both must agree. Denial codes
  are stable dotted strings: `sandbox.undeclared-capability` (`:347`, deliberately class-only
  so a guest cannot enumerate held capabilities), `sandbox.no-run-policy` (`:286`),
  `sandbox.grant-mismatch` (`:363`). `crates/daemon/src/policy_gate.rs:22-28` records that the
  shipped default is `DenyAllGate` — *"a guest can compute and nothing else."*
- `GateGrant` (`gate.rs:201-231`) is **not** `Clone`, `Default`, or `Deserialize`, and carries
  a request digest so it cannot be replayed against a different request
  (`a_grant_cannot_be_replayed_against_a_different_request`, `gate.rs:496`). This is the
  unforgeable-capability pattern M8's lease token should copy.
- Signing/verification: `checksum_of` (`crates/sandbox/src/verify.rs:67-70`), `signing_digest`
  (`crates/sandbox/src/verify.rs:98-110`), `verify_artifact`
  (`crates/sandbox/src/verify.rs:125-181`, ed25519-dalek pinned `=2.2.0`, `verify_strict`),
  `TrustedPublishers` (`crates/sandbox/src/trust_store.rs:99-249`), `UnsignedPolicy::Deny` as
  the `#[default]` (`crates/sandbox/src/verify.rs:26-32`).
- Domain-separation tags are an established convention here, not an invention:
  `b"codypendent-plugin-signature-v1"` (`verify.rs:107`) and `b"codypendent-host-request-v1"`
  (`gate.rs:152-181`). Follow it.

**M8's `SandboxSpec` on the wire must be a serializable projection of `SandboxProfile`, not a
new vocabulary.** Task 8.4's "translate `SandboxSpec` into enforceable container controls"
means: `write_paths` → the only writable mounts; `memory_mb`/`cpu_seconds` → cgroup limits;
`env_allowlist` → the only environment that survives; `brokered_secrets` → names, resolved
per-call, never materialized into the container environment or image. **Network is the
exception — see §6.3.**

### 2.2 Durable execution — `crates/workflow`

- `NodeExecutor` (`crates/workflow/src/drive.rs:137-142`) is a one-method async trait:
  `async fn execute(&self, ctx: NodeContext<'_>) -> NodeOutcome`. `remote_node_executor.rs`
  implements exactly this. No new trait, no `WorkflowDriver` change (plan task 8.6 says the
  same).
- `NodeContext` (`crates/workflow/src/drive.rs:124-131`) carries `workflow_run_id: &str`,
  `node: &CompiledNode`, `attempt: u32` — and nothing else. **That triple is the entire
  idempotency key material available to a remote executor.**
- `NodeOutcome` (`crates/workflow/src/drive.rs:66-91`): `Completed { agent_run_id, cost,
  warnings }` / `Failed { error }` / `Blocked { error, cost }`. Remote results map onto these
  three; there is no fourth.
- The recovery contract is at `crates/workflow/src/drive.rs:362-378`: on drive start, every
  `Running`/`WaitingApproval` node is passed to `WorkflowStore::reset_interrupted_node` and
  every `Blocked` node to `reset_blocked_node`. The comment states the assumption plainly —
  *"a `Running` node was interrupted mid-execution (a crash), so reset it to `Pending` to
  re-drive exactly once (effects are idempotent — the resume contract)"*.
- `reset_interrupted_node` (`crates/workflow/src/store.rs:1005-1040`) **preserves `attempt`**
  and clears `agent_run_id`/`cost_json`/`ended_at`. `reset_blocked_node`
  (`store.rs:962-994`) does the opposite — it *preserves* cost so the pre-gate re-blocks
  without re-spending. Both are load-bearing for M8: see §5.3.
- Idempotent run creation already exists and is the template for the remote job key:
  `create_run_idempotent_owned` (`store.rs:354`), `deterministic_run_id`
  (`store.rs:1144-1153`), and `workflow_request_signature` (`store.rs:1157`) — a duplicate key
  with a *different* request signature is rejected `workflow.idempotency-mismatch`
  (`crates/codypendentd/src/workflows.rs:551-578`) rather than silently reusing the run.
- The graph-signature guard (`drive.rs:303-314`, `store.rs:851-857`) refuses to half-drive a
  run whose manifest changed underneath it. A remote job spec must therefore be pinned to the
  compiled node, not re-derived from a possibly-newer manifest at claim time.
- Budget: `NodeCost { wall_time_secs, tool_calls, cost_micros: Option<u64>, tokens:
  Option<u64> }` (`crates/workflow/src/budget.rs:51-71`), `BudgetLimits::charge`
  (`budget.rs:308`), and the honesty gate at `budget.rs:331-343` — **an unmeasured (`None`)
  cost is never charged.** Remote costs obey the same rule.

### 2.3 Run control — two registries, and the remote executor needs the second

`RunControlRegistry` (`crates/codypendentd/src/executor.rs:143-152`) holds
`live: HashMap<RunId, CancellationHandle>`, `pending_cancellations`, `pending_pauses` under
**one** mutex (the single-lock invariant is documented at `executor.rs:121-142` — three mutexes
deadlocked once; do not re-split). `register()` (`executor.rs:161-168`) consumes a pending
cancel *at registration time* so a `CancelRun` racing the spawn *"is either consumed here or
finds the handle in `live` — never lost between the two."* `cancel_run` is at
`executor.rs:2701-2711`, `pause_run` at `:2713-2720`, `resume_run` at `:2722-2730`. Regression
guard: `run_control_survives_concurrent_start_stop_traffic` (`executor.rs:3644`).

**It is keyed by `RunId` — an agent run, not a workflow node.** The registry a node dispatcher
must use is `WorkflowRunCancellations` (`crates/codypendentd/src/workflow_exec.rs:560-688`),
keyed by workflow-run id string, with `begin` (`:595`), `register(&self, workflow_run_id) ->
(u64, CancellationToken)` (`:606`), `deregister` (`:623`), `cancel` (`:650`), `finish`
(`:666`), `is_cancelled` (`:677`). `WorkflowConductorHost::cancel`
(`crates/codypendentd/src/workflows.rs:889-944`) fires it at `:911`.

Both are in-memory and process-local; neither survives a restart and neither can reach another
machine. Their pending-set pattern is exactly the shape the runner scheduler needs, **promoted
to durable storage**: a cancel that arrives before a runner claims must be *stored* so the
claim consumes it. See `runner_jobs.cancel_requested_at` in §3.2 and §5.4.

Cancellation primitives themselves live in `crates/runtime/src/agent.rs`: `CancellationToken`
(`:1018-1021`) over a `watch::Receiver<RunControl>`, `RunControl { Running, Paused, Cancelled }`
(`:1023-1027`), `CancellationHandle` (`:1096-1098`), `cancellation()` (`:1132-1135`).

### 2.4 Content-addressed storage — `crates/daemon/src/artifacts.rs`

The local artifact store already lays blobs out as `<root>/sha256/<xx>/<full-hex>` with a
write-to-`tmp/<uuid>`-then-atomic-rename discipline (`crates/daemon/src/artifacts.rs:101-141`,
`store_blob` at `:280-297`), and every `ArtifactRef` carries a `classification` column and a
`Provenance` (`artifacts.rs:38-99`). Runner input/output bundles use the same addressing
scheme and the same `sha256:<hex>` canonical string form as
`codypendent_sandbox::verify::checksum_of` (`crates/sandbox/src/verify.rs:67-71`). Do not
invent a second digest encoding.

### 2.5 Process supervision — `crates/daemon/src/unified_exec/`

`crates/daemon/src/unified_exec/{manager,process,process_state,head_tail_buffer}.rs` is the
shipped PTY/process manager (1,287 lines). Reuse rather than re-derive:

- `HeadTailBuffer` (`crates/daemon/src/unified_exec/head_tail_buffer.rs:9-155`) is the
  bounded-output primitive — a symmetric 50/50 head+tail cap that drops the middle and reports
  `omitted_bytes()`. Design §7.4 requires *"bounded live logs"*; this is the shipped shape, and
  `format_output_omission_marker` (`unified_exec/mod.rs:140-142`) is the honest marker.
- The shipped ceilings: `UNIFIED_EXEC_OUTPUT_MAX_BYTES = 1 MiB`, `MAX_UNIFIED_EXEC_PROCESSES =
  64`, `MAX_YIELD_TIME_MS = 30_000`, `DEFAULT_MAX_BACKGROUND_TERMINAL_TIMEOUT_MS = 300_000`
  (`unified_exec/mod.rs:17-27`); `MAX_STDIN_BYTES = 1 MiB`
  (`crates/sandbox/src/executor.rs:154`), where oversized stdin is **refused, not truncated**
  (`oversized_stdin_is_refused_not_truncated`, `executor.rs:1603`).
- Environment is `env_clear()`ed then rebuilt from a sanitized overlay `UNIFIED_EXEC_ENV`
  (`unified_exec/mod.rs:29-40`, includes `TERM=dumb`, `NO_COLOR=1`, `CODYPENDENT_CI=1`) —
  the same discipline `spawn_capture_kill` uses (`crates/sandbox/src/executor.rs:748-756`).
- `UnifiedExecManager::write_stdin` refuses a process id belonging to another session
  (`manager.rs:269-271`, returns `UnknownProcessId`). That "unauthorized is indistinguishable
  from absent" shape is required of every runner endpoint too (design §12.4).
- Process-group kill is a shell-out to `/bin/kill -KILL -- -<pgid>`
  (`unified_exec/process.rs:236-247`, `crates/sandbox/src/executor.rs:911-928`) because the
  workspace denies `unsafe_code`. The runner cannot call `killpg` either.

---

## 3. Data model

### 3.1 Which migration directory

There are **two** migration sequences and they must not be confused.

| | Root `migrations/` | `crates/control-plane/migrations/` |
|---|---|---|
| Engine | SQLite | **PostgreSQL** |
| Discipline | append-only, **checksum-gated** | forward-only |
| Gate | `.github/scripts/check_migration_immutability.py` (reads `migrations/checksums.json`, `check_migration_immutability.py:35`, and diffs against `git show HEAD^:migrations/checksums.json`, `:110`) | none of the above |
| Highest shipped | `0040_session_library.sql` | *(directory does not exist yet)* |
| M8 adds | **nothing** | `0006_runner_jobs.sql`, `0007_runner_leases.sql`, `0008_runner_attestations.sql` |

Plan program rules 18–19 say the same. M7 task 7.2 creates
`0001_identity.sql`..`0005_audit.sql`; M8 continues at `0006`.

**The `sqlx` workspace dependency is SQLite-only today** — `Cargo.toml:132` reads
`sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio", "sqlite",
"migrate", "macros", "chrono", "uuid"] }`. Cargo feature unification is workspace-wide and
additive, so adding `"postgres"` there turns it on for every crate that depends on `sqlx`
(`crates/daemon`, `crates/workflow`, `crates/eval`, `crates/knowledge`, `crates/codypendentd`).
This is M7's problem to solve, but M8 inherits it: verify at the start of M8 that
`crates/control-plane` gets PostgreSQL through its **own** `[dependencies] sqlx` entry with a
crate-local feature set, or that the workspace entry was widened deliberately with a recorded
decision. Do not discover this at task 8.2.

Types below are PostgreSQL. `TIMESTAMPTZ` throughout (the SQLite side stores RFC3339 `TEXT` —
do not copy that here). `JSONB`, not `JSON`. `BYTEA` for raw signature/key bytes.

### 3.2 `0006_runner_jobs.sql`

```sql
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
```

### 3.3 `0007_runner_leases.sql`

```sql
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
```

### 3.4 `0008_runner_attestations.sql`

```sql
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
    -- | revoked-key | lease-mismatch | oversized
    reason_code         TEXT NOT NULL,
    detail              JSONB NOT NULL,
    quarantined_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX ix_runner_quarantine_job ON runner_quarantine (job_id, quarantined_at DESC);
```

### 3.5 The canonical attestation statement

Task 8.1 requires the attestation bytes to bind *"job, attempt, lease, runner/image,
input/output hashes, timestamps, and result."* Follow the shipped signing contract rather than
inventing one — `crates/sandbox/src/verify.rs:89-110` documents why:

```text
digest = SHA256( b"codypendent-runner-attestation-v1"
               || be_u64(len(canonical))
               || canonical )
```

where `canonical` is the deterministic JSON serialization of a struct with **no optional or
map-typed fields** (the sandbox digest's own constraint: *"all named struct fields, enums,
scalars, and ordered `Vec`s — no maps or floats"*) containing:

`job_id`, `job_spec_hash`, `attempt_id`, `attempt_number`, `lease_id`, `lease_generation`,
`runner_id`, `image_digest`, `input_manifest_hash`, ordered `outputs: Vec<(name, content_hash,
byte_length)>` sorted by `name`, `started_at`, `ended_at` (RFC3339 UTC), `exit_code`, and
`result` (the terminal state string).

The length prefix is not decoration — it commits the statement's length under the hash so no
field value can forge a boundary. Reject any signature that does not verify under the current
scheme tag; **do not** add a compatibility fallback to an older tag
(`crates/sandbox/src/verify.rs:103-110` refuses exactly this for the same reason).

---

## 4. The runner protocol

Design §7.4 enumerates the contract. This section supplies the semantics the spec leaves
open.

### 4.1 Transport and mutual authentication

- The runner initiates **outbound** to the control plane. There is no inbound port on a runner,
  exactly as the daemon has none (design §7.3). A Kubernetes runner Pod needs no Service.
- Workload authentication is mTLS **or equivalent** (design §12.2). Whichever is chosen, both
  directions authenticate: the runner proves its identity to the control plane, and the control
  plane's certificate is pinned by the runner so a hijacked DNS answer cannot feed a runner a
  forged job.
- Runner credentials are **short-lived and audience-bound** (design §5.2). The audience is the
  control-plane API; a runner token presented to the object store is rejected, and the
  object-store credential is a separate, per-attempt, minimum-scope presign (M7 task 7.5).
- **Job-scoped**: design §5.2 — *"Runner credentials are job-scoped and cannot claim unrelated
  jobs or repositories."* After a claim, the lease token authorizes exactly one
  `(job_id, attempt_id)`. Task 8.3's "compromised runner tests requesting another job/
  repository/secret" test this directly.

### 4.2 How a runner proves what it is

Distinguish two claims, and never conflate them:

| Claim | When | Enforced by |
|---|---|---|
| **Identity** — "I am runner X in org Y" | Every request | Workload credential (mTLS peer identity / audience-bound token) resolved to `runners.id`. Never read from the request body. |
| **Capability** — "I have cargo 1.88, image D, region eu-west" | Registration + claim | *Advertised*, therefore *untrusted*. It filters eligibility only. |

An advertised capability is a scheduling hint, not a security property. The security properties
are the ones the control plane can check itself: the runner's registered `sandbox_backend`, its
`region`, its `image_digest` against `runner_images`, and — after the fact — the signed
attestation binding `image_digest` into the statement. A runner that lies about its image
produces an attestation whose `image_digest` is unknown or revoked, and the job quarantines
(§4.6). A runner that lies about having `cargo` fails its job honestly.

Design §12.1 is the governing sentence: *"Authentication does not imply repository, artifact,
runner, or approval authority."*

### 4.3 Claim

One transaction:

```sql
WITH candidate AS (
    SELECT j.id
      FROM runner_jobs j
     WHERE j.organization_id = $org
       AND j.state = 'queued'
       AND j.cancel_requested_at IS NULL
       AND <eligibility predicate against $runner's registered facts>
     ORDER BY j.created_at
     FOR UPDATE SKIP LOCKED
     LIMIT 1
)
UPDATE runner_jobs SET state = 'leased', attempt_count = attempt_count + 1,
       updated_at = now()
 WHERE id IN (SELECT id FROM candidate)
RETURNING id, job_spec, input_manifest_hash;
```

`FOR UPDATE SKIP LOCKED` (plan task 8.2) is what makes N concurrent claimers each get a
distinct job with no coordination. Insert the `runner_job_attempts` row and the
`runner_leases` row in the **same** transaction; `ux_runner_leases_one_active` then makes a
double-lease structurally impossible rather than merely unlikely.

**Cancel consumed at claim.** If `cancel_requested_at IS NOT NULL`, the job is not claimable
and the claim transaction instead transitions it `queued → cancelled`. This is the durable
analogue of `RunControlRegistry::register` consuming a pending cancellation
(`crates/codypendentd/src/executor.rs:159-167`) — same race, same resolution, different
storage.

### 4.4 Lease renewal, heartbeat, cancellation

- **Renewal presents `(lease_id, generation)`.** The control plane bumps `generation`, extends
  `expires_at`, and returns the new generation. A renewal at a stale generation is refused
  (`lease.stale-generation`) — this is why a replayed renew message from a partitioned runner
  cannot resurrect a lease the reaper already expired.
- **Heartbeat is the renewal.** Do not add a second liveness channel; two channels means two
  truths. `last_heartbeat_at` is written by the renewal.
- **Expiry is absolute and runner-enforced.** The runner must kill its own workload at
  `expires_at` without being told. A partitioned runner cannot receive a cancel, so
  self-termination is the only mechanism that actually bounds a partitioned execution. Design
  §11.4: *"A disconnected runner may finish within its lease but cannot claim another job."*
- **Cancellation is a durable request, not a message.** `cancel_requested_at` is set; the
  runner observes it on its next renewal response and terminates; the reaper handles the runner
  that never asks again. Cancellation racing completion resolves by the terminal CAS (§4.5),
  per design §11.4.

### 4.5 Terminal compare-and-set

Every terminal transition is `UPDATE ... WHERE id = $job AND state = $expected` and checks
`rows_affected`. Acceptance is:

```sql
UPDATE runner_jobs
   SET state = 'succeeded', accepted_attempt_id = $attempt, terminal_at = now()
 WHERE id = $job
   AND state = 'verifying'
   AND accepted_attempt_id IS NULL;
```

Zero rows affected means someone else already terminalised the job — the caller must **not**
retry into a different terminal state, and must **not** accept a second output set. Combined
with `ux_runner_jobs_one_accepted`, this is acceptance criterion 13.

### 4.6 Results, artifacts, and verification order

1. Runner uploads each declared output by content hash to object storage (presigned,
   attempt-scoped, minimum-scope).
2. Runner registers each output; job moves `executing → uploading`.
3. **The job stays `uploading` until every declared output row is `verified`** (plan task 8.5,
   design §11.4). A partial upload is resumable: re-registering an output with the same
   `(attempt_id, name, content_hash)` is idempotent; a *different* hash for the same name is a
   conflict, not an overwrite.
4. Runner submits the attestation; job moves `uploading → verifying`.
5. Control plane verifies, **in this order**, refusing at the first failure:
   a. lease ownership — the attestation's `lease_id`/`generation` matches the active lease for
      this attempt;
   b. signature — against `runners.attestation_pubkey`, with `revoked_at` checked against the
      attempt's `started_at`;
   c. image — `image_digest` present in `runner_images` and not revoked;
   d. bindings — `job_spec_hash` and `input_manifest_hash` in the statement equal the job's;
   e. outputs — the statement's output list is exactly the declared set, with hashes matching
      the stored, re-verified bytes.
6. Any failure writes `runner_quarantine` and terminalises the job `quarantined`. **Quarantine
   blocks automatic continuation** — a quarantined job never feeds a workflow node.

Only step 5's full success permits the acceptance CAS in §4.5. Plan task 8.5: *"only accepted
attempt artifacts may continue workflows."*

### 4.7 Partition and crash mid-run

The local engine is crash-consistent through the workflow checkpoint/recovery path
(`crates/workflow/src/drive.rs:338-378`, `crates/daemon/src/recovery.rs:92` `recover_on_startup`,
`crates/daemon/src/checkpoints.rs:47` `record_checkpoint`, migration
`migrations/0035_run_checkpoints.sql`). Remote must be too. The matrix:

| Partition point | Runner does | Control plane does | Net effect |
|---|---|---|---|
| After claim, before materialize | self-terminates at `expires_at` | reaper expires lease, attempt → `expired`, job → `queued` if attempts remain | re-queued, new attempt number |
| Mid-execution | self-terminates at `expires_at` | same | same; the effect is bounded by the job being idempotent (§5.3) |
| Mid-upload | resumes uploads on reconnect within lease | job remains `uploading`; missing hashes retried | resumable, no duplicate |
| After attestation, before response | retries the attestation submit | `runner_attestations` has `UNIQUE (attempt_id)` — the retry is a no-op returning the stored result | at-most-once verification |
| Daemon offline while remote runs | n/a | job continues; results await sync | design §11.4: *"A disconnected daemon does not stop remote work"* |

**Never double-execute a non-idempotent effect.** Two mechanisms, both required:

1. The job's `idempotency_key` makes *submission* exactly-once (`runner_jobs_idempotent`).
2. Any external effect performed *by* the job (a GitHub write, a package publish) must itself
   carry a deterministic idempotency token derived from `(job_id, attempt_number)`. A retry
   under a new attempt number is a *new* token and **will** repeat the effect — so a job whose
   effect is not externally idempotent must be declared `max_attempts = 1`. Make this explicit
   in the `JobSpec`; do not leave it to the workflow author's memory.

---

## 5. Dispatching workflow nodes remotely (`remote_node_executor.rs`)

### 5.1 Shape

```rust
pub struct RemoteNodeExecutor {
    local: Arc<AgentLoopNodeExecutor>,       // the fallback, unchanged
    client: Arc<dyn ControlPlaneRunnerClient>,
    policy: RemoteDispatchPolicy,
}

#[async_trait]
impl NodeExecutor for RemoteNodeExecutor {
    async fn execute(&self, ctx: NodeContext<'_>) -> NodeOutcome { … }
}
```

`NodeExecutor` is `crates/workflow/src/drive.rs:136-142`. Nodes the policy does not select
delegate to `AgentLoopNodeExecutor` (`crates/codypendentd/src/workflow_exec.rs:694-729`, its
`NodeExecutor` impl at `:2592-2605`) verbatim — plan task 8.6 requires *"local nodes still use
`AgentLoopNodeExecutor`"*. Do not fork the local path.

**Two structural constraints on the implementation:**

1. `WorkflowDriver::run_observed` polls node futures with a borrowing `FuturesUnordered`,
   deliberately avoiding `'static` bounds (`crates/workflow/src/drive.rs:438-444`). A remote
   executor must therefore be a borrowing `async fn`, not a spawned task. If you find yourself
   wanting `tokio::spawn`, you are about to widen the trait — don't.
2. **The production caller is `build_workflow_host`**
   (`crates/codypendentd/src/workflow_exec.rs:481-535`), the single construction site for the
   node executor, called from `crates/codypendentd/src/executor.rs:377` (startup) and `:506`
   (`with_github` reconfiguration). Install the dispatcher there and carry the
   `DriveLockRegistry` and `WorkflowRunCancellations` forward. Plan rule 17: *"Every feature
   task must name and exercise a production caller."*

**Replicate the local executor's ordered steps**, or the remote path silently loses behaviour
the workflow depends on (`run_agent_node`, `workflow_exec.rs:863-1159`):

| Step | Local | Remote must |
|---|---|---|
| repository resolution | `node_repository` (`:1185-1200`) — run's stored repository, never `current_dir()` | pin the repository into the job spec |
| agent resolution | `resolve_agent` (`:785-818`) | resolve locally, ship the resolved profile |
| **budget pre-gate** | `budget_limits` (`:823-841`) + `budget_consumption` (`:2779`); `Exceeded` ⇒ `blocked()` **without running** | run the same pre-gate *before submitting the job* — never pay a runner to discover a block |
| run row creation | `create_agent_run` (`:1205-1226`) — also marks the session `internal = 1` with `parent_run_id`/`parent_session_id` | do the same, or the archival sweep in `workflows.rs:247-256` will not find the child session |
| cost measurement | `NodeCost` from measured wall time, `count_tool_calls` (`:847`), `node_cost_micros` (`:226`), `node_tokens` (`:308`) | take measurements from the verified attestation/result, never from an unverified runner claim |
| budget charge | `charge_node_budget` (`:1166-1179`) | identical, with `None` for anything unmeasured |

### 5.2 Result mapping

| Remote terminal | `NodeOutcome` |
|---|---|
| `succeeded` with verified outputs | `Completed { agent_run_id, cost, warnings }` — `cost` from measured remote spend only |
| `failed` (non-zero exit, declared) | `Failed { error }` |
| budget ceiling exhausted remotely | `Blocked { error, cost }` |
| `cancelled` | `Failed { error }` (the driver treats every non-success as retryable; the run-level cancel is handled above the node) |
| `quarantined` | `Failed { error }` — **never** `Completed`; a quarantined output set must not continue the workflow |

`cost` is `Option<Value>`; leave it `None` when the remote side measured nothing. Do not
synthesize a zero — the workspace rule (plan rule 23, design §8.5) is *unknown measurements
stay absent*.

### 5.3 The idempotency key — the load-bearing detail

`NodeContext` gives you exactly `(workflow_run_id, node.node_id, attempt)`
(`crates/workflow/src/drive.rs:124-131`). Derive the job's `idempotency_key` from that triple
and nothing else:

```text
idempotency_key = "wf:" || workflow_run_id || ":" || node_id || ":" || attempt
```

Why this is correct and why nothing else is:

- `reset_interrupted_node` (`crates/workflow/src/store.rs:1005-1022`) **preserves `attempt`**
  and **clears `agent_run_id` and `cost_json`**. So after a daemon crash the node re-enters the
  frontier at the *same* attempt number with *no memory of the job it submitted*.
- Deriving the key from the triple means the re-drive's submission collides with the original
  on `runner_jobs_idempotent` and returns the **existing** job receipt (design §11.4). The
  executor then attaches to the in-flight job instead of launching a second one.
- Storing the job id on the node row would not work: the reset clears exactly those columns.
- Adding wall-clock time, a UUID, or a retry counter to the key breaks it — every crash would
  spawn a duplicate job.

A genuine retry (the driver's retry policy, which *increments* `attempt`) produces a different
key and therefore a genuinely new job. That is the intended distinction: **crash-resume reuses,
retry re-submits.**

### 5.4 Cancellation reaching a remote node

`WorkflowRunCancellations` (`crates/codypendentd/src/workflow_exec.rs:560-688`) is in-memory
and process-local; it cannot cancel a remote job and does not survive a restart.
`WorkflowConductorHost::cancel` (`crates/codypendentd/src/workflows.rs:889-944`) fires it at
`:911` after `conductor.cancel` has already marked the run `Cancelled` and pending nodes
`Skipped`.

`RemoteNodeExecutor` must, on cancel, call the control plane's cancel endpoint (which sets
`runner_jobs.cancel_requested_at`) **in addition to** observing its local `CancellationToken`.
Firing only the local token abandons a running remote job that keeps burning budget on someone
else's hardware — and the workflow, having marked the node `Skipped`, will never look at it
again.

### 5.5 Reconnect

Plan task 8.6 requires a test for daemon disconnect while remote work finishes. On reconnect,
the workflow drive's existing recovery pass resets the `Running` node to `Pending`
(`drive.rs:363-370`), the node re-executes, the derived key collides, and the executor reads
the now-terminal job's result. That path — not a bespoke reconciliation loop — is the
production caller.

---

## 6. Security posture, concretely

A runner executes untrusted, model-driven work on hardware the job's owner does not control.
Design §12.1 lists runners among the independently untrusted principals. Four requirements,
each with a shipped precedent.

### 6.1 OS confinement reusing the sandbox posture, failing closed

Two nested boundaries, both mandatory:

1. **Container/microVM** — non-root, read-only root filesystem, all capabilities dropped,
   workspace-only writable mounts, CPU/memory/PID limits, deny-by-default network (plan task
   8.4).
2. **`crates/sandbox` inside it** — the runner materializes a `SandboxProfile` from the job's
   `SandboxSpec` and executes through `enforcing_executor()`
   (`crates/sandbox/src/executor.rs:436-450`).

**The fail-closed rule is not optional and not new.** If `enforcing_executor()` returns
`SandboxError::UnsupportedPlatform` or `ToolUnavailable` — or the probed backend reports
`CapabilityReport::available == false`, or `enforces_exit_criteria()` is false
(`crates/sandbox/src/executor.rs:115-121`) — the runner must **refuse the job and release the
lease**, not execute unconfined. Every one of those error messages already ends in *"refusing
to run unconfined"* (`executor.rs:380-392`). A runner registered with
`runners.sandbox_backend = 'none'` must be ineligible for every job at the scheduler, so the
refusal is caught before a claim rather than after.

Corollary: task 8.4's *"refuse unsupported grants rather than silently weakening them"* is
already the shipped behaviour, and it refuses more than you expect
(`validate_enforceable_profile`, `executor.rs:1283-1313`; and on Linux,
`UnsupportedCapability("Linux bubblewrap cannot prevent exec of bound system binaries when
subprocess=false")` at `executor.rs:1208-1211`). Executing with a weaker profile than requested
is the exact defect the fail-closed rule exists to prevent. Do not "fix" a refusal by relaxing
the spec.

Shipped fail-closed tests to model the runner's after:
`the_fail_closed_executor_refuses_to_run` (`crates/sandbox/tests/enforcement_it.rs:282`, runs on
every platform), `refusing_sandbox_refuses_every_run` (`executor.rs:1513`),
`availability_probe_fails_closed_on_a_missing_tool` (`executor.rs:1525`),
`zero_ceilings_are_refused_rather_than_meaning_unlimited` (`crates/sandbox/src/wasm.rs:1146`),
`a_missing_file_and_a_denied_file_are_indistinguishable_to_the_guest` (`wasm.rs:1086`).

### 6.2 No secret reaches runner-visible storage or logs

- `SandboxProfile.brokered_secrets` (`crates/sandbox/src/profile.rs:39-40`) is a list of
  **names**: *"Named secrets brokered per call (never placed in env)."* The runner receives
  names; values are fetched per-use through the M5 broker with a short-lived lease.
- Design §9: *"A runner cannot ask for a broader secret after accepting a job; the job must be
  rejected and resubmitted with a newly reviewed specification."* Enforce at the broker, not at
  the runner — the runner is the untrusted party.
- Environment is allowlist-only (`SandboxProfile.env_allowlist`, `profile.rs:31-32`,
  `ENV_ALLOWLIST` exported at `crates/sandbox/src/lib.rs:100`). No inherited process
  environment.
- `lease_token_hash` (§3.3) is stored hashed. Neither the database nor a backup yields a usable
  lease credential.
- Log chunks (`runner_log_chunks`) are runner-produced and therefore untrusted content. Run
  them through `codypendent_sandbox::sanitize::sanitize_untrusted`
  (`crates/sandbox/src/lib.rs:101`) before any rendering path. Design §12.2 requires
  *"append-only audit records without credential values."*

### 6.3 Egress control — read this before designing `SandboxSpec`

**The shipped sandbox cannot enforce a host:port allowlist, and refuses to pretend it can.**

- `validate_enforceable_profile` rejects any non-empty `network_allowlist` with
  `UnsupportedCapability("host:port network allowlists require a broker; refusing unrestricted
  outbound access")` (`crates/sandbox/src/executor.rs:1284-1289`).
- `bwrap_argv` **always** emits `--unshare-net` (`executor.rs:647`) — there is no code path that
  gives a bubblewrap guest a network namespace.
- `seatbelt_profile` starts `(deny default)` (`executor.rs:528`) and only widens to the coarse
  `(allow network-outbound (remote ip))` when the allowlist is non-empty
  (`executor.rs:566-572`) — which the validator has already made unreachable.
- Tests: `bwrap_argv_denies_network_and_clears_env_when_no_allowlist` (`executor.rs:1426`),
  `seatbelt_empty_network_denies_all_network` (`executor.rs:1384`),
  `a_non_allowlisted_host_is_unreachable_and_allowlists_fail_closed_without_a_broker`
  (`crates/sandbox/tests/enforcement_it.rs:98`).

So the runner's egress design has exactly two honest options, and M8 must pick one explicitly:

1. **No network for job workloads** — `network_allowlist: []`, `--unshare-net`, and the runner
   process (outside the guest sandbox) is the only thing that talks to the control plane and
   object store. This is the shipped posture and the default. Choose it unless a job genuinely
   needs egress.
2. **A brokered egress proxy** — the guest gets a network namespace whose only route is a
   proxy the runner controls, and the proxy enforces the allowlist by connecting on the guest's
   behalf. This is the "broker" the shipped error message names as the missing piece. It is new
   work, it is the SSRF surface design §12.2 warns about, and it must not be smuggled in by
   loosening `validate_enforceable_profile`.

Either way the container/Pod layer denies egress by default (`NetworkPolicy` with no egress
rule; no `--network` for the container backend), and the object-store presign the runner uses
is attempt-scoped and minimum-scope. Do not weaken the sandbox validator to make option 2 look
like option 1.

### 6.4 The data-classification ceiling travels with the job

This is the requirement most likely to be missed, because remote execution *feels* like a
capability rather than a disclosure.

The shipped invariant, at `crates/codypendentd/src/routing.rs:612-620`:

```rust
let ceiling = self.config.data_classification;
let effective_classification = run_classification
    .or_else(|| derive_run_classification(None, objective))
    .filter(|derived| derived.rank() > ceiling.rank())
    .unwrap_or(ceiling);
```

with the comment at `routing.rs:611-615`: *"The operator-declared ceiling is a FLOOR on
restrictiveness: a per-run or derived classification may only ever RAISE sensitivity above it,
never lower it."* `derive_run_classification` is at `routing.rs:758`; the off-device gate is
`RoutingPolicy::hosted_allows` (`crates/routing/src/policy.rs:157-162`), applied as the
router's security hard filter at `crates/routing/src/router.rs:291` and again on the pinned-
model path so a pin *"overrides the router's quality judgment, never its security constraint"*
(`router.rs:277-289`). The refusal message when nothing is eligible reads *"data classified
{:?} may not leave the device and no local model is available"* (`router.rs:457-462`).

**Sending a job to a runner is off-device execution.** Therefore:

1. `runner_jobs.data_classification` is derived by the *daemon* at dispatch, using the same
   `derive_run_classification` + ceiling-max composition — not supplied by the caller, and not
   recomputed by the control plane from a payload it cannot trust.
2. `RemoteNodeExecutor` refuses to dispatch when the effective classification fails the
   organization's off-device ceiling, and returns the node to local execution — or fails the
   node closed if no local path exists. It never downgrades the classification to make a runner
   eligible.
3. Design §4.1: *"A cloud grant can never broaden the local daemon's security decision. The
   effective authority is the intersection of remote RBAC, organization policy, repository
   policy, and local policy."* A runner being available is not permission. Plan rule 24 says
   the same: *"Cloud grants can narrow but never broaden."*
4. `DataClassification::Unknown` fails closed — `hosted_allows` returns `false` for it on both
   sides of the comparison (`policy.rs:158-161`), and `RoutingPolicy::validate` rejects a
   policy whose ceiling is `Unknown` outright (`policy.rs:167-169`). Preserve that: an
   unclassified job is not a safe job.

Precedent for the "narrow only" combinator lives in `SandboxProfile::derive`
(`crates/sandbox/src/profile.rs:53-58`) and in `crates/daemon/src/policy_gate.rs:12-20`, which
records the inverse failure mode: applying a ceiling twice *"would hide which gate refused and
re-create the second policy path."* One gate, one place.

---

## 7. Kubernetes controller (task 8.7)

- One hardened `Job` per **attempt**, never per logical job — the attempt is the unit with a
  lease and an attestation.
- PostgreSQL is authoritative for scheduler and lease state. Kubernetes annotations are a
  cache; a controller restart re-derives from the database. Plan task 8.7 states this; the
  practical consequence is that the reaper (§4.4) must work with the controller entirely down.
- No broad ServiceAccount. The Pod gets the attempt's credential and nothing else — in
  particular no ability to list or read other Pods, Secrets, or the cluster API.
- Security context: `runAsNonRoot: true`, `readOnlyRootFilesystem: true`,
  `allowPrivilegeEscalation: false`, `capabilities.drop: [ALL]`, `seccompProfile:
  RuntimeDefault`, an emptyDir workspace as the only writable mount, and a `NetworkPolicy`
  denying egress by default.
- Contract-test the container and Kubernetes backends against the **same** job vectors (plan
  task 8.7). This is the M9 acceptance criterion 12 rehearsal: one protocol, many backends.

---

## 8. Acceptance criteria

Objectively checkable, each tied to a test name. Names are the contract; place them in the
files the plan's **Files:** lines name.

1. **The protocol has golden vectors for every message.** `crates/control-plane-protocol`'s
   vector test writes `protocol-vectors/control-plane/v1/runner.json` and a regeneration test
   fails on drift — same mechanism as `crates/protocol/tests/golden_vectors.rs`.
   Test: `runner_protocol_vectors_are_stable`.
2. **Attestation bytes bind every required field.** Mutating any one of `job_spec_hash`,
   `attempt_id`, `lease_id`, `image_digest`, `input_manifest_hash`, any output hash, or the
   result changes the digest. Test: `attestation_digest_binds_every_field` (one assertion per
   field, table-driven).
3. **An older-scheme signature does not verify.** Test:
   `attestation_rejects_foreign_scheme_tag`.
4. **Concurrent claimers get disjoint jobs.** N tasks claim against one PostgreSQL instance;
   the union of claimed ids has no duplicate and equals the queued set.
   Test: `concurrent_claims_skip_locked_are_disjoint`.
5. **Renewal at a stale generation is refused.** Test: `renew_with_stale_generation_is_refused`.
6. **Duplicate submission returns the existing receipt.** Same `idempotency_key` twice ⇒ one
   `runner_jobs` row, same id returned. Test: `duplicate_submission_returns_existing_receipt`.
7. **Exactly one accepted attempt.** Two attempts complete and both attempt acceptance; exactly
   one CAS succeeds and `accepted_attempt_id` is stable.
   Test: `only_one_attempt_is_ever_accepted`.
8. **Cancel racing completion is resolved by CAS, not by ordering.** Test:
   `cancel_racing_completion_yields_one_terminal_state`.
9. **Cancel before claim is consumed at claim.** Test: `cancel_before_claim_is_consumed`.
10. **Malicious archives are refused.** Absolute path, `../` parent escape, symlink escape,
    hardlink escape, duplicate entry conflict, expansion-ratio bomb, size overflow, undeclared
    entry, wrong hash — nine cases, each refused before any byte lands on disk.
    Test: `materialize_refuses_hostile_archive_<case>`.
11. **A compromised runner cannot widen scope.** With a valid lease for job A, requests against
    job B, another repository, and a secret not in `brokered_secrets` are all refused.
    Test: `compromised_runner_cannot_claim_other_scope`.
12. **The runner refuses to execute unconfined.** With the enforcing executor unavailable, the
    job is refused and the lease released; no process is spawned.
    Test: `runner_refuses_job_when_enforcement_unavailable`.
13. **Container controls are real.** The job process is non-root, root filesystem is read-only,
    capabilities are empty, only the workspace is writable, and an outbound connection to a
    non-allowlisted host fails. Tests: `container_runs_non_root`,
    `container_root_filesystem_is_read_only`, `container_drops_all_capabilities`,
    `container_denies_undeclared_egress`.
14. **A partial upload leaves the job `uploading` and resumes.** Test:
    `partial_upload_resumes_without_duplicate_output`.
15. **Bad hash, unknown signer, revoked key, revoked image, wrong lease, and malformed
    attestation each quarantine.** Six cases; each writes `runner_quarantine` and blocks
    continuation. Test: `verification_quarantines_<case>`.
16. **A quarantined job never continues a workflow.** Test:
    `quarantined_outputs_do_not_continue_workflow`.
17. **Remote dispatch preserves `WorkflowDriver` semantics.** Node attempts, measured costs,
    attribution, retry policy, observer transitions, and workflow budget are identical to the
    local executor for an equivalent node. Test:
    `remote_dispatch_matches_local_node_semantics`.
18. **Crash-resume attaches rather than resubmits.** Kill the daemon mid-node; on restart the
    reset node re-executes, the derived key collides, one job exists.
    Test: `crash_resume_reuses_the_same_remote_job`.
19. **Classified data does not leave the device because a runner exists.** A node whose
    effective classification exceeds the off-device ceiling is never dispatched, and the failure
    message names the classification. Test:
    `remote_dispatch_honours_the_off_device_ceiling`.
20. **A cloud grant cannot broaden local policy.** A control-plane grant permitting a capability
    the local policy denies still results in a denial. Test:
    `cloud_grant_cannot_broaden_local_deny`.
21. **Container and Kubernetes backends agree on the same job vectors.** Test:
    `container_and_kubernetes_agree_on_job_vectors`.
22. **Control-plane migrations apply forward from every prior schema fixture.** Test:
    `control_plane_migrations_apply_from_each_fixture`.
23. **The root SQLite checksum gate still passes.** `python3
    .github/scripts/check_migration_immutability.py` exits 0 with `migrations/` unchanged — M8
    adds no SQLite migration.

---

## 9. Gotchas

1. **`sqlx` has no `postgres` feature in this workspace.** `Cargo.toml:132`. See §3.1. Resolve
   before task 8.2, not during it.

2. **`RunControlRegistry` is `pub(crate)` and keyed by `RunId`, not by node.**
   `crates/codypendentd/src/executor.rs:143`. The registry a node dispatcher needs is
   `WorkflowRunCancellations` (`workflow_exec.rs:560`). Both live in `crates/codypendentd`,
   which is why the plan puts `remote_node_executor.rs` there — keep it there;
   `crates/runner` cannot reach either.

3. **`reset_interrupted_node` clears `agent_run_id` and `cost_json`.**
   `crates/workflow/src/store.rs:1013-1014`. Any scheme that remembers the remote job id on the
   node row is silently erased by crash recovery. This is the single most likely way to ship a
   double-execution bug. Derive the key (§5.3).

4. **`reset_interrupted_node` *preserves* `attempt`** (`store.rs:1005-1040`), while the retry
   path increments it — and `reset_blocked_node` (`store.rs:962-994`) preserves *cost* instead.
   The three behaviours are deliberately different and the idempotency key depends on the
   difference. Read `crates/workflow/src/drive.rs:362-378` before writing the executor.

4b. **The node executor's futures must borrow, not be `'static`.** `drive.rs:438-444`
   deliberately avoids spawning. A remote executor that reaches for `tokio::spawn` is changing
   `WorkflowDriver` semantics, which task 8.6 forbids.

4c. **`create_agent_run` now marks child sessions `internal = 1`** with `parent_run_id` /
   `parent_session_id` (`workflow_exec.rs:1205-1226`, uncommitted at `b8e17bd`). A remote
   executor that skips this leaves orphan sessions the archival sweep
   (`workflows.rs:247-256`) never finds. Rebase onto the committed version before relying on
   the line numbers.

5. **`NodeOutcome` has three variants, not four.** `crates/workflow/src/drive.rs:66-91`. There
   is no `Quarantined`. Map it to `Failed` and put the reason in the string; adding a variant
   changes `WorkflowDriver` semantics, which task 8.6 forbids.

6. **The driver treats every non-success as retryable.** `drive.rs:137-141`. A remote failure
   that must not be retried (a non-idempotent effect that already fired) has to be expressed as
   `Blocked`, which the driver does not retry (`drive.rs:83-90`), or prevented at submission by
   `max_attempts = 1`.

7. **`Blocked` is not retried and pauses the run for a human.** `drive.rs:84-90`. Do not reach
   for it to mean "remote failure"; it means budget exhaustion, and it pauses the workflow.

8. **`DataClassification::Unknown` is the fail-closed default, not "unclassified is fine".**
   `hosted_allows` returns `false` when either side is `Unknown`
   (`crates/routing/src/policy.rs:158-161`), and `RoutingPolicy::validate` rejects a policy
   whose ceiling is `Unknown` (`policy.rs:167-169`). A job row defaulting
   `data_classification` to something permissive inverts the whole model.

9. **`derive_run_classification` is keyword-based and returns `Option`.**
   `crates/codypendentd/src/routing.rs:758-800`. `None` means "no sensitive signal found", not
   "public". The `.filter(rank > ceiling).unwrap_or(ceiling)` composition at `routing.rs:616-619`
   is what turns `None` into the ceiling. Copying the call without the composition silently
   drops the ceiling.

10. **The sandbox's `SandboxProfile` is plugin-shaped.** `plugin: String` at `profile.rs:29-30`
    and `derive()` takes a `PluginManifest`. A runner job is not a plugin. Add a job-shaped
    constructor rather than fabricating a manifest to satisfy `derive` — but keep the *field
    set* identical so the two paths cannot diverge in what they enforce. `SandboxProfile` has
    no `Default` and no `intersect`/`restrict` combinator; narrowing is done by the caller
    passing a smaller grant.

10b. **Zero is not "unlimited" anywhere in this codebase.**
    `validate_enforceable_profile` rejects a zero resource cap outright
    (`executor.rs:1290-1301`), and the WASM host has its own
    `zero_ceilings_are_refused_rather_than_meaning_unlimited` (`wasm.rs:1146`). A runner
    `ResourceSpec` that defaults a missing cap to 0 will be refused at execution, not silently
    unbounded — which is correct, but you should discover it at design time.

10c. **`crates/sandbox` has no cargo features at all.** No `[features]` table; platform
    selection is source-level `cfg` only, and `wasmi` is pinned `default-features = false`
    (`crates/sandbox/Cargo.toml`). Do not add a feature to make a runner backend optional —
    follow the `cfg` convention.

11. **`enforcing_executor()` is `cfg`-gated per platform.** `executor.rs:436-451`. macOS-only
    and Linux-only helpers around it are a recurring CI trap: this repository's Linux lint job
    fails on dead code that a macOS build never sees (note the shipped
    `#[cfg(any(target_os = "linux", test))]` on `locate_on_path`, `executor.rs:951`). Gate
    helpers with the same `cfg` as their caller and build for both targets before pushing.

12. **`protocol-vectors/` is regenerated by an `--ignored` test, not by CI.**
    `crates/protocol/tests/golden_vectors.rs:34-41`. The control-plane vectors must follow the
    same convention or CI will regenerate-and-pass instead of detecting drift.

13. **`docs/MANIFEST.json` indexes every file under `docs/` recursively.**
    `.github/scripts/check_docs_manifest.py:38-43`. Any doc M8 adds must be listed or
    `check_docs_manifest.py` fails — it is in the plan's common verification set.

14. **Log chunks are untrusted model output.** Sanitize before rendering
    (`codypendent_sandbox::sanitize::sanitize_untrusted`, exported at
    `crates/sandbox/src/lib.rs:101`). A runner is an adversary in the threat model
    (design §12.1); its logs reach a browser.

15. **Attestation verification order matters.** Verify lease ownership *before* signature, and
    signature *before* trusting any field in the statement. A statement is attacker-controlled
    bytes until its signature verifies — this is why `verify_artifact`
    (`crates/sandbox/src/verify.rs:125-140`) checks the checksum *first*: *"a signature over a
    checksum means nothing if the checksum does not describe the bytes in hand."*

---

## 10. Contradictions between plan/spec and shipped code

Recorded for M8's milestone review (plan §Milestone review template, item 8).

- **Plan rule 19** says control-plane migrations live only in `crates/control-plane/migrations/`,
  and task 8.2 names `0006`–`0008`. Neither the crate nor the directory exists at `b8e17bd`;
  M8 is unstartable until M7 task 7.2 lands `0001`–`0005`. Confirm before beginning.
- **Design §12.2** requires "mTLS or equivalent workload authentication". The shipped local
  socket has no TLS at all — it authenticates by connection principal
  (`crates/daemon/src/principal.rs`, `crates/protocol/src/command.rs:612`). There is no existing
  mTLS implementation in the workspace to reuse; M8 introduces the first one.
- **Design §7.4 lists "sandbox and resource specification" as a runner-protocol element, and
  plan task 8.4 says to "translate `SandboxSpec` into enforceable container controls".** The
  shipped sandbox refuses to enforce a network allowlist at all
  (`crates/sandbox/src/executor.rs:1284-1289`) and always unshares the network namespace
  (`:647`). A `SandboxSpec` with a populated `network_allowlist` is therefore not translatable
  today. Resolve this at task 8.1 (spec design), not at task 8.4 (implementation) — see §6.3.
- **Plan task 8.4 says "run the existing Codypendent runtime inside disposable job
  environments", and design §11.2 says "job environments are disposable".** The shipped
  local sandbox has no notion of a disposable environment; worktree isolation
  (`WorktreeReleaseGuard`, `crates/codypendentd/src/workflow_exec.rs:1013-1019`) is the closest
  analogue and it is a git worktree on the same filesystem, not a container. M8 introduces
  disposability; do not assume a shipped mechanism exists to reuse.
- **Design §11.1 step 5** says the runner "executes the existing Codypendent runtime or workflow
  node". The runtime's model-execution seam is documented as incomplete —
  `crates/codypendentd/src/routing.rs:480-487` states escalation's live re-drive *"awaits the
  runtime's mid-run model-switch hook"*, and `ROADMAP.md:522` names the live measured paths as
  the remaining Phase 7 slice. A remote node that needs mid-run model switching will hit the
  same missing seam; scope task 8.6 to nodes that do not.
- **`crates/routing/src/arms.rs:15-25`** states plainly that no shipped command drives the route
  arms and that the release gate *"is not evaluable by any shipped path"*. M8 does not depend on
  it, but M9 does — see the M9 guide §7.
