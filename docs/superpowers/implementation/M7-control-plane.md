# M7 — Hybrid control plane

**Audience:** the implementer(s) of Milestone 7 of
[`plans/2026-08-16-hybrid-platform-program.md`](../plans/2026-08-16-hybrid-platform-program.md) (Tasks 7.1–7.10).
**Read first:** [`00-conventions-and-traps.md`](./00-conventions-and-traps.md) and
[`M6-federation.md`](./M6-federation.md) (M7 consumes M6's publication classes and tombstones).
**Design authority:** [`specs/2026-08-16-hybrid-platform-program-design.md`](../specs/2026-08-16-hybrid-platform-program-design.md)
§4.1, §5, §6.1, §6.2, §6.4, §7.2, §7.3, §7.5, §10, §12.

M7 is the largest milestone in the program: two new Rust crates, two new TypeScript SDKs, a new web
application, a PostgreSQL service, an object store, two identity protocols, and the first network
trust boundary this codebase has ever had. **It is not one pass.** §6 breaks it into independently
shippable checkpoints; read that before writing any code.

---

## 1. Status — verified against the tree on 2026-08-17

| Path | State |
|---|---|
| `crates/control-plane/` | **absent** |
| `crates/control-plane-protocol/` | **absent** |
| `sdk/control-plane/` | **absent** |
| `sdk/control-plane-react/` | **absent** |
| `apps/web/` | **absent** |
| `crates/federation/` | absent (M6 prerequisite) |
| `migrations/0049_control_plane_sync.sql` | absent (highest on disk is `0040_session_library.sql`) |
| `crates/daemon/src/control_plane_sync/` | absent |
| `protocol-vectors/control-plane/` | absent |

Existing workspace members are listed at `Cargo.toml:3-22`; `sdk/` contains `protocol`, `remote-ui`,
`ui`, `wasm`; `apps/` contains only `desktop`.

**Nothing needed for the server exists as a dependency yet.** The workspace has no `axum`, no
`tower`/`tower-http`, no `jsonwebtoken`/`josekit`, no `oauth2`, no S3 client, and `sqlx`
(`Cargo.toml:132`) is declared `default-features = false` with `["runtime-tokio", "sqlite", "migrate",
"macros", "chrono", "uuid"]` — **no `postgres`, and no TLS feature at all**. See §8.1.

What M7 builds on that *does* exist:

| Reused | Where |
|---|---|
| Append-only durable record | `crates/daemon/src/ledger.rs:202` `append_next_event` — claims its sequence inside the INSERT |
| Owner-scoped store + principal-bound keyset cursor | `crates/daemon/src/session_library.rs:111`, cursor hashing at `:121` |
| Same-transaction outbox | `crates/knowledge/src/outbox.rs:55` |
| Principal derivation | `crates/daemon/src/principal.rs:27` (`SO_PEERCRED`) |
| Repository ownership gate | `crates/daemon/src/server.rs:4965` `principal_owns_repository` |
| Schema export → TS generation | `crates/protocol/src/bin/export_schema.rs:163`, `sdk/protocol/scripts/generate.mjs:11`, gated by `.github/scripts/check_generated_protocol.sh` |
| Publication classes and tombstones | M6, `migrations/0047_graph_publication.sql` |

---

## 2. Two migration sequences, two engines, two rule sets

This is the distinction most likely to be got wrong, so state it in full.

| | Root `migrations/` | `crates/control-plane/migrations/` |
|---|---|---|
| Engine | SQLite | PostgreSQL |
| Runner | `sqlx::migrate!("../../migrations")` at `crates/daemon/src/db.rs:48` | a **second** `sqlx::migrate!("./migrations")` inside the control-plane crate |
| Numbering | continues the global sequence (`0049_control_plane_sync.sql`) | restarts independently at `0001_identity.sql` |
| Repo gate | `migrations/checksums.json` + `.github/scripts/check_migration_immutability.py` (SHA-384, enforced in the `lint` CI job at `.github/workflows/ci.yml:26`) | **not covered by that gate** — the script only scans `migrations/` |
| Rule | append-only, immutable, checksum-gated | forward-only |

Three things the "forward-only" label does **not** mean:

1. **Not editable.** `sqlx::migrate` records a checksum of each applied migration in the target
   database's `_sqlx_migrations` table, on PostgreSQL exactly as on SQLite. Editing a deployed
   control-plane migration makes every existing deployment refuse to boot with *"migration N was
   previously applied but has been modified"*. The repo-level gate is absent; the runtime one is not.
   Treat control-plane migrations as immutable-once-deployed and put the comment right the first
   time.
2. **Not exempt from the docs/manifest gate.** Files under `docs/` are; SQL is not — but adding
   `crates/control-plane/migrations/` does mean the migration-immutability script now sees a second
   directory of numbered `.sql` files it does not track. Decide explicitly whether to extend
   `.github/scripts/check_migration_immutability.py` with a second root (recommended: yes, with the
   PostgreSQL set gated on "recorded checksums never change" but allowed to be added out of order)
   or to leave it uncovered and say so in the crate README.
3. **Not a place for down-migrations.** There are none anywhere in this repo. Rollback is
   restore-from-backup, which is why Task 7.9 requires a tested backup/restore document.

`0049_control_plane_sync.sql` is a **root SQLite** migration and obeys the root rules: append-only,
`--update` the checksums, commit the SQL and `checksums.json` together.

---

## 3. Data model

### 3.1 `migrations/0049_control_plane_sync.sql` — local SQLite (daemon side)

The daemon remains authoritative. Everything here is *bookkeeping about what was synchronized*, not
a second copy of authority.

```sql
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
```

### 3.2 `crates/control-plane/migrations/` — PostgreSQL (server side)

Five files, per plan Task 7.2. PostgreSQL types throughout: `uuid`, `text`, `timestamptz`, `bytea`,
`jsonb`, `bigint`. Every table has `created_at timestamptz NOT NULL DEFAULT now()`.

**`0001_identity.sql`**

```sql
CREATE TABLE users (
    id uuid PRIMARY KEY,
    -- Display only. NOT an identity key: email collisions must never link two
    -- humans (design §5.1 requires proof of both identities to link).
    display_name text NOT NULL,
    primary_email text,
    state text NOT NULL CHECK (state IN ('active', 'suspended', 'deleted')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- One row per external identity. A human may have several (design §5.1).
CREATE TABLE user_identities (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id),
    -- 'github' | 'oidc'
    provider text NOT NULL,
    -- The OIDC issuer, or 'https://github.com'. Part of the identity key
    -- because subject values are only unique WITHIN an issuer — two OIDC
    -- tenants can both mint sub='1'.
    issuer text NOT NULL,
    subject text NOT NULL,
    -- Verified email at link time, retained for audit. Never used to match.
    email_at_link text,
    linked_at timestamptz NOT NULL DEFAULT now(),
    -- The audit record id proving both identities were authenticated when the
    -- link was made. NOT NULL: an unlinked-provenance link is not permitted.
    link_audit_id uuid NOT NULL,
    UNIQUE (provider, issuer, subject)
);

-- Refresh credentials for browser sessions. Only the hash is stored, so a
-- database compromise does not yield usable tokens.
CREATE TABLE user_refresh_tokens (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id),
    token_hash bytea NOT NULL UNIQUE,
    -- Rotation chain: a replayed (already-rotated) refresh token means theft,
    -- and must revoke the whole chain rather than just failing.
    rotated_from uuid REFERENCES user_refresh_tokens(id),
    issued_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    user_agent_digest bytea
);
CREATE INDEX ON user_refresh_tokens (user_id) WHERE revoked_at IS NULL;

-- OAuth/OIDC in-flight state. Rows are single-use and short-lived; the PKCE
-- verifier hash and nonce are what make an authorization-code interception
-- useless.
CREATE TABLE auth_flows (
    state text PRIMARY KEY,
    provider text NOT NULL,
    issuer text NOT NULL,
    pkce_verifier_hash bytea NOT NULL,
    nonce text NOT NULL,
    redirect_uri text NOT NULL,
    -- Set when the flow is a LINK rather than a login; the already-authenticated
    -- user whose account the new identity attaches to.
    linking_user_id uuid REFERENCES users(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz
);
```

**`0002_organizations.sql`**

```sql
CREATE TABLE organizations (
    id uuid PRIMARY KEY,
    slug text NOT NULL,
    display_name text NOT NULL,
    -- Organization-wide publication ceiling (design §12.3). Narrowing input to
    -- every repository under it.
    max_publication_class text NOT NULL DEFAULT 'metadata-shared',
    max_classification text NOT NULL DEFAULT 'internal',
    data_residency text,
    retention_days integer CHECK (retention_days IS NULL OR retention_days > 0),
    policy_version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now()
);
-- Case-insensitive uniqueness without the citext extension (self-hosters may
-- not be able to CREATE EXTENSION).
CREATE UNIQUE INDEX ON organizations (lower(slug));

CREATE TABLE teams (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    slug text NOT NULL,
    display_name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX ON teams (organization_id, lower(slug));

CREATE TABLE memberships (
    organization_id uuid NOT NULL REFERENCES organizations(id),
    user_id uuid NOT NULL REFERENCES users(id),
    state text NOT NULL CHECK (state IN ('invited', 'active', 'suspended')),
    joined_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id, user_id)
);

CREATE TABLE team_members (
    team_id uuid NOT NULL REFERENCES teams(id),
    user_id uuid NOT NULL REFERENCES users(id),
    PRIMARY KEY (team_id, user_id)
);

CREATE TABLE repositories (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    -- M6's cross-machine identity (SHA-256 hex). The control plane never sees a
    -- local path-derived RepositoryId.
    federated_id text NOT NULL CHECK (char_length(federated_id) = 64),
    display_name text NOT NULL,
    -- Repository ceiling. Intersected with the organization's, never widening
    -- it — enforce in code AND with the CHECK-able invariant test in §5.
    max_publication_class text NOT NULL DEFAULT 'metadata-shared',
    max_classification text NOT NULL DEFAULT 'internal',
    policy_version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- Scoped to the organization on purpose: a global UNIQUE on federated_id
    -- turns registration into a cross-tenant existence oracle (a 409 proves
    -- another organization registered that repository). See §5.4.
    UNIQUE (organization_id, federated_id)
);

CREATE TABLE role_grants (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    -- Exactly one of user_id / team_id is set: a grant is to a principal or to
    -- a group, never both, never neither.
    user_id uuid REFERENCES users(id),
    team_id uuid REFERENCES teams(id),
    -- NULL = organization-wide scope.
    repository_id uuid REFERENCES repositories(id),
    role text NOT NULL CHECK (role IN (
        'observer', 'contributor', 'approver', 'maintainer', 'organization-admin'
    )),
    -- For 'approver': the action scope the grant is limited to (design §5.3
    -- requires an EXPLICIT repository/action scope). NULL for other roles.
    action_scope jsonb,
    granted_by uuid NOT NULL REFERENCES users(id),
    granted_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz,
    revoked_at timestamptz,
    CHECK ((user_id IS NULL) <> (team_id IS NULL)),
    CHECK (role <> 'approver' OR action_scope IS NOT NULL)
);
CREATE INDEX ON role_grants (organization_id, user_id, repository_id) WHERE revoked_at IS NULL;
CREATE INDEX ON role_grants (organization_id, team_id, repository_id) WHERE revoked_at IS NULL;
```

**`0003_workloads.sql`**

```sql
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
```

**`0004_sync.sql`**

```sql
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
```

**`0005_audit.sql`**

```sql
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
```

The role split is created by the deployment, not by a migration that assumes role names. Ship it as
documented SQL in `docs/self-hosting/control-plane.md` and verify it with an integration test that
connects as the runtime role and asserts `UPDATE audit_records` fails.

---

## 4. Authority boundaries, concretely

**The rule:** the local daemon stays authoritative for source, private history, artifacts, secrets
and local effects. The control plane must never become an authority that can broaden local policy.
Cloud grants may narrow, never broaden.

Per table and per endpoint:

| Control-plane object | May it decide? | The boundary |
|---|---|---|
| `organizations.max_publication_class` | Yes, downward only | Sent to the daemon, stored in `control_plane_policy_snapshot`, and read only as `local.strictest(remote)`. There is no code path that writes it into `graph_publication_policy`. |
| `repositories.max_publication_class` | Yes, downward only | Must additionally be `<=` its organization's. Enforce on write **and** on read (a direct DB edit must not widen anything). |
| `role_grants.role` | Yes, remotely | Grants access to *control-plane* resources. It never becomes a local `ClientRole` (`crates/daemon/src/commands.rs:3001`) — the two enums are unrelated despite sharing the words "observer"/"contributor"/"approver". A remote `maintainer` gets no local capability whatsoever. |
| `daemons.accepts_remote_approvals` | Narrowing switch | `false` means the daemon ignores remote approvals. `true` does **not** mean a remote approval is sufficient: the local approval broker still applies local policy and the action digest still has to match. |
| `daemons.accepts_runner_dispatch` | Narrowing switch | Same shape. |
| `published_objects` | Yes | The control plane is authoritative for what it stores. It is **not** authoritative for whether the daemon should have sent it. |
| `shared_sessions`, `sync_receipts` | Yes | Projections. A conflict is resolved in the daemon's favour: the daemon re-sends, the control plane overwrites. Never the reverse. |
| `audit_records` | Yes | The only control-plane store that is authoritative in the strong sense. |
| Anything about source, worktrees, secrets, unpublished artifacts | **No representation at all** | If a control-plane table would need one of these to answer a query, the query is out of scope. |

**Identity derivation, per side:**

- **Local:** `PeerPrincipal::from_stream` (`crates/daemon/src/principal.rs:44`). No wire field ever
  becomes an owner. This does not change in M7 — a synchronized delta is attributed to the
  `owner_uid` recorded locally, not to a user id the control plane sent.
- **Remote, human:** the `user_id` resolved from a validated session/refresh token, then from
  `user_identities` by `(provider, issuer, subject)`. Never from an `X-User-Id` header, a JWT claim
  the caller could mint, or a body field.
- **Remote, workload:** the `daemon_id` resolved from `workload_credentials.token_hash`, with
  `audience` and `purpose` checked against the endpoint. A daemon may never assert its
  `organization_id`, `repository_id` or `max_publication_class` in a request body — all three are
  read from the `daemons` row.

**The concrete anti-pattern to test for:** a sync delta whose payload names a repository the daemon
is not paired for. The correct behaviour is refusal keyed on the daemons row, and — because
unauthorized and absent are indistinguishable — the same refusal whether the repository exists in
another organization or does not exist at all.

---

## 5. Unauthorized vs absent in a multi-tenant PostgreSQL service

Locally this reduces to a uid comparison (`crates/daemon/src/principal.rs:97`). Remotely it is the
hardest requirement in M7, because SQL leaks existence through channels that have no local analogue.

### 5.1 One authorized-scope CTE, used by every query

Build the authorized set once and **join** to it. Never filter after loading.

```sql
WITH authorized_repositories AS (
    SELECT r.id
    FROM repositories r
    JOIN role_grants g ON g.organization_id = r.organization_id
                      AND (g.repository_id IS NULL OR g.repository_id = r.id)
    LEFT JOIN team_members tm ON tm.team_id = g.team_id
    WHERE g.revoked_at IS NULL
      AND (g.expires_at IS NULL OR g.expires_at > now())
      AND (g.user_id = $1 OR tm.user_id = $1)
      AND g.role = ANY($2)          -- the roles sufficient for THIS action
)
SELECT ... FROM shared_sessions s JOIN authorized_repositories a ON a.id = s.repository_id ...
```

A helper that returns this CTE, and a lint/review rule that no handler may reference
`repositories`, `shared_sessions` or `published_objects` without it, is worth more than any number
of individual authorization tests.

### 5.2 Counts, aggregates and pagination

- `COUNT(*)`, `SUM`, `MAX` all run **inside** the CTE. An accurate total over the unfiltered set is
  a disclosure even when no row is returned.
- No "N results hidden" affordance, anywhere, at any layer including the web client.
- Cursors are keyset, not offset, and are **bound to the principal**. Copy the shipped shape at
  `crates/daemon/src/session_library.rs:95-103`: the cursor carries a `query_hash` that folds in the
  principal id, and decoding rejects a mismatch (`:121`). Sign it (HMAC over a server key) as well,
  because unlike the local socket the value crosses an untrusted boundary — an unsigned cursor is a
  writable query parameter.
- Page size limits are constant, not data-dependent. `has_more` must be computed inside the
  authorized set (fetch `limit + 1`, as `session_library` does).

### 5.3 Error bodies and status codes

One shape for both cases:

```json
{ "type": "not_found", "resource": "repository", "message": "no such repository" }
```

- Same HTTP status (`404`) for unauthorized-and-existing and absent. No `403` on repository-scoped
  resources at all. `401` is reserved for "no valid credential presented" — a state that carries no
  information about any resource.
- The same body for a repository in another organization, a deleted repository, and a typo.
- No `WWW-Authenticate` variation, no differing `Retry-After`, no error `detail` that names the
  organization, no correlation id that embeds a resolved internal id.
- Validation order matters: validate syntax → authenticate → **authorize** → load. A `400 invalid
  uuid` before authorization is fine (it depends on nothing); a `400 repository is archived` after
  loading is a disclosure.

### 5.4 The PostgreSQL-specific leaks

These have no local equivalent and are the reason this section exists:

1. **Unique-constraint violations are oracles.** A global `UNIQUE (federated_id)` on `repositories`
   means registering a repository another organization already registered returns `409`, proving its
   existence. The constraint is `UNIQUE (organization_id, federated_id)` for exactly this reason.
   Audit every unique index in `0002`/`0004` for the same property.
2. **Foreign-key violation messages name the constraint and therefore the table.** Map every
   `sqlx::Error::Database` with an FK/unique code to the generic `not_found`; never let a driver
   error string reach a response body. Do this in one place in `src/error.rs`, not per handler.
3. **Idempotency keys must be principal-scoped.** `PRIMARY KEY (principal_kind, principal_id, key)`
   in `0004_sync.sql`. A global key space lets one tenant enumerate another's keys by observing
   whether a mutation replays or conflicts.
4. **Row-level security is defence in depth, not the gate.** If you enable RLS with
   `SET LOCAL app.principal_id`, it protects against a handler that forgot the CTE — but an RLS
   denial surfaces as zero rows *or* as a constraint failure depending on the statement, and the
   difference is observable. Keep the query-builder gate authoritative and treat an RLS-only denial
   as a bug that failed loudly in tests.
5. **Serial/bigserial ids leak volume.** `stream_events.id` is a resume cursor and is fine (it is
   organization-scoped in every read). Do not expose a `bigserial` as a public resource id anywhere
   else — the delta between two ids measures another tenant's write rate. Every externally visible
   id is a `uuid`.
6. **`EXPLAIN`-visible timing.** Aim for the same work class, as locally: authorize first, so an
   unauthorized query does an index probe on `role_grants` and returns, whether or not the target
   row exists. Do not add artificial delays; do add a test that a query for a non-existent uuid and
   one for an inaccessible real uuid take the same code path (assert on a counter, not a clock).
7. **Streams leak on subscribe.** `stream_events` rows carry `repository_id`; a subscription must
   filter by the authorized CTE at *delivery* time, re-evaluated as grants change. A revoked grant
   must stop delivery on the existing connection, not only on the next connect.

---

## 6. Sub-phasing — M7 is too large for one pass

Plan Tasks 7.1–7.9 are not equal in size and are not all on the critical path. Ship in these
checkpoints; each is independently reviewable, and each leaves the tree green and the local product
untouched.

| # | Checkpoint | Plan tasks | Ships | Gate |
|---|---|---|---|---|
| **7A** | Network protocol crate + generated SDK | 7.1 | `crates/control-plane-protocol`, `protocol-vectors/control-plane/v1/`, `sdk/control-plane` | Golden vectors round-trip; generated TS is byte-identical on re-run |
| **7B** | Service skeleton: config, PG migrations, health, error mapping | 7.2 | `crates/control-plane` boots against a real PG, migrates, `/healthz`, `/readyz` | Migration test from an empty DB and from each schema fixture |
| **7C** | Identity | 7.3 | GitHub OAuth + OIDC discovery, PKCE/state/nonce, refresh rotation, identity linking, workload tokens, pairing challenges | Replayed code/state/nonce rejected; email collision does not link |
| **7D** | RBAC + audit + non-disclosure | 7.4 | Authorized-scope CTE, role matrix, hash-chained audit, unified error shape | Full role × action × scope matrix; absent/inaccessible byte-equality |
| **7E** | First vertical slice end to end | 7.6 (minimal) + 7.7 (two endpoints) | `migrations/0049`, daemon pairing, one delta kind, one read endpoint, one stream | See below |
| **7F** | Object storage | 7.5 | Content-addressed upload, range reads, presign, tombstones; memory + MinIO adapters | Wrong-hash and partial-upload tests |
| **7G** | API breadth | 7.7 | Remaining REST resources and streams | Idempotency: receipt + effect in one transaction |
| **7H** | Browser client | 7.8 | `sdk/control-plane-react`, `apps/web` | Auth callback, stream resume, a11y, deep links |
| **7I** | Deployment + observability | 7.9 | Dockerfile/Compose/Helm, self-host docs, OTEL | Non-root image, backup/restore, upgrade/rollback |

Ordering constraints that are real: 7A before everything (the SDK is the contract); 7B before 7C;
7D before 7E (do not build endpoints and retrofit authorization — that is how per-arm ownership
leaks happened four times locally); 7E before 7F/7G (it validates the whole shape cheaply); 7H after
7G for the resources it renders.

### The suggested first vertical slice (7E)

Narrowest path that crosses **every** authority boundary exactly once:

1. A human logs in with GitHub (7C), creates an organization, and registers one repository.
2. They start a pairing challenge; the daemon displays the consent manifest locally and the human
   confirms at the machine (`control_plane_pairings`, `state='active'`, class `metadata-shared`).
3. The daemon enqueues **one** delta kind — `session-summary` — into `control_plane_outbox`, in the
   same transaction as the local session write.
4. The daemon syncs outbound; the control plane writes `sync_receipts` + `shared_sessions` + an
   `audit_records` row in one transaction.
5. A second user *without* a grant calls `GET /organizations/{id}/sessions` and receives a response
   byte-identical to the one for a non-existent organization.
6. The human revokes the daemon; the next sync attempt fails with a specific revocation error and
   the daemon continues local work unaffected.

That slice exercises: identity, RBAC, non-disclosure, idempotency, outbound-only transport,
narrowing-only policy, audit, and offline tolerance. Everything after it is breadth, not depth.

### `sdk/control-plane` must be generated

The program rule (`plan:20`) admits no exception. Follow the shipped pipeline exactly:

1. Add `crates/control-plane-protocol/src/bin/export_schema.rs`, modelled on
   `crates/protocol/src/bin/export_schema.rs:163` — same `SchemaSettings::draft07()`, same
   `canonicalize` key sort, same "write one catalog struct per domain" shape.
2. Emit into `sdk/control-plane/schema/`.
3. Add `sdk/control-plane/scripts/generate.mjs`, copying the `targets` array shape at
   `sdk/protocol/scripts/generate.mjs:11`.
4. Add a `check:generated` script and a CI step alongside the existing
   `.github/scripts/check_generated_protocol.sh` (which is a `generated-protocol` job of its own at
   `.github/workflows/ci.yml:50`) — the determinism double-export check is the valuable half; keep it.

Do **not** hand-write `sdk/control-plane/src/*.ts`. There are already ~14 hand-mirrored modules in
`sdk/protocol/src/` and a whole hand-written mirror in `extensions/vscode/src/protocol/`; the
program rule exists to stop that list growing.

---

## 7. Acceptance criteria

1. `control_plane_migrations_apply_to_an_empty_database` — `0001`–`0005` apply cleanly, and
   re-running is a no-op.
2. `control_plane_migrations_apply_to_each_schema_fixture` — one fixture per released schema
   version; each upgrades without data loss.
3. `runtime_role_cannot_update_or_delete_audit_records` — connecting as the runtime role, `UPDATE`
   and `DELETE` on `audit_records` both fail.
4. `audit_hash_chain_detects_a_removed_row` — deleting a middle row as the owner breaks
   verification.
5. `oauth_replayed_state_is_rejected` / `oidc_nonce_mismatch_is_rejected` /
   `pkce_verifier_mismatch_is_rejected` — each single-use artefact is consumed exactly once.
6. `identity_link_requires_authenticated_proof_of_both` — linking without an active session for the
   existing identity fails and writes no `user_identities` row.
7. `matching_email_does_not_link_identities` — two providers reporting the same email produce two
   users.
8. `refresh_token_replay_revokes_the_chain` — presenting a rotated token revokes every descendant.
9. `workload_token_is_audience_and_purpose_bound` — a `sync` credential is refused at a pairing
   endpoint and vice versa.
10. `daemon_cannot_assert_its_own_organization_or_class` — a sync request whose body names a
    different organization, repository or wider class than the `daemons` row is refused, and the
    stored delta carries the row's values.
11. `rbac_matrix_is_exhaustive` — table-driven over every (role × action × scope) pair, including
    the absence of a grant.
12. `inaccessible_and_absent_responses_are_byte_identical` — for every repository-scoped endpoint:
    same status, same body bytes, same headers except `Date`.
13. `counts_are_computed_inside_the_authorized_set` — a tenant with one visible session out of many
    sees `total = 1`.
14. `cursor_from_another_principal_is_rejected` — and a tampered cursor is rejected, not silently
    reinterpreted.
15. `duplicate_registration_across_organizations_does_not_conflict` — registering the same
    `federated_id` in two organizations succeeds in both; no `409` is observable.
16. `idempotent_mutation_stores_receipt_and_effect_atomically` — kill the process between the two
    and assert neither exists; replay returns the original response body.
17. `idempotency_key_is_scoped_to_the_principal` — the same key from two principals produces two
    independent effects.
18. `stream_resumes_without_gap_or_duplicate_effect` — disconnect mid-stream, resume from the
    cursor, assert exact continuity.
19. `revoking_a_grant_stops_an_open_stream` — delivery ceases on the existing connection.
20. `object_upload_with_wrong_hash_is_refused` / `partial_upload_is_never_served` — an `uploading`
    row is invisible to reads and to counts.
21. `tombstones_are_applied_before_new_deltas_after_reconnect` — the offline-delete scenario ends
    with the object absent; a resurrecting delta is rejected as stale.
22. `remote_policy_narrows_but_never_broadens` — a control plane sending a wider
    `max_publication_class` than local leaves the effective class unchanged; a narrower one applies.
23. `local_operation_survives_control_plane_outage` — with the endpoint unreachable, local session
    create/run/approve all succeed; shared operations report a specific connectivity error and stay
    queued.
24. `daemon_with_no_pairing_never_opens_a_socket` — assert on the connector, not on logs.
25. `previous_protocol_version_still_negotiates` — the current and one prior control-plane protocol
    version both handshake (design §7.5).
26. `generated_control_plane_sdk_is_deterministic_and_current` — the double-export diff, mirroring
    `.github/scripts/check_generated_protocol.sh`.
27. `web_client_never_requests_unpublished_content` — the `apps/web` SDK surface has no method that
    can name a local artifact, worktree or transcript.

---

## 8. Gotchas

### 8.1 Dependencies and the build

1. **`sqlx` has no `postgres` feature today** (`Cargo.toml:132`). Adding it to the shared workspace
   entry enables PostgreSQL for every sqlx consumer through feature unification — the daemon, the
   workflow store, everything — inflating build time and the `cargo deny` graph (`deny.toml` sets
   `[graph] all-features = true`). Prefer a second workspace entry
   (`sqlx-postgres = { package = "sqlx", version = "0.8", default-features = false, features = [...] }`)
   or accept unification deliberately and say why in the Cargo.toml comment.
2. **`default-features = false` means no TLS.** A control plane talking to a managed PostgreSQL needs
   `tls-rustls` (and `rustls` is already in the tree via `reqwest` at `Cargo.toml:94`, so pick the
   same backend or you will ship two TLS stacks).
3. **No `axum`, `tower`, `tower-http`, `jsonwebtoken`, `oauth2` or S3 client exists.** Every one is a
   new entry in `[workspace.dependencies]` and a new subtree in the `deny` job
   (`.github/workflows/ci.yml:212`). The licence allowlist in `deny.toml` is exactly the set present
   today; check new transitive licences before the PR, not after CI goes red.
4. **`cargo test --workspace --all-features` runs everything** (`.github/workflows/ci.yml:61`), and
   CI has **no PostgreSQL service container**. Control-plane integration tests must either be feature-
   gated behind something not in `--all-features` (a `DATABASE_URL` env probe that skips cleanly is
   the low-friction option) or CI must gain a `services: postgres` block. Decide in 7B; retrofitting
   after 40 integration tests exist is painful.
5. **MinIO for 7F** is a second service container. Keep the object-store trait's in-memory adapter
   the default so only a dedicated job needs MinIO.

### 8.2 Contracts and CI

6. **The vector inventory tests are not recursive.** `sdk/protocol/test/protocol-vectors.test.ts:2369`
   and `extensions/vscode/test/protocol-vectors.test.ts:1371` both do
   `readdirSync(dir).filter(name => name.endsWith(".json"))`. Putting control-plane vectors in
   `protocol-vectors/control-plane/v1/` makes them **invisible to both guards** — you get a directory
   of golden vectors with no drift protection and a green CI. Add an explicit suite in
   `sdk/control-plane/test/` that walks its own directory, and note the non-recursion in the vectors
   README.
7. **`apps/web` must join the `sdk-desktop` CI job** (`.github/workflows/ci.yml:281`), including its
   `cache-dependency-path` list, or it is never built, typechecked or tested. A new package directory
   also needs its own `.gitignore` — three packages have shipped `node_modules/`/`dist/` into the
   repo.
8. **Adding vitest tests drifts the `doc-count:vitest` markers**, which only the `extension` job
   settles (`--skip-vitest` hides it locally). Adding `apps/web` tests means deciding whether its
   vitest run also feeds `check_doc_test_counts.py`.
9. **`docs/self-hosting/*.md` (Task 7.9) fails the docs manifest gate** until
   `python3 .github/scripts/check_docs_manifest.py --fix` is run and the regenerated
   `docs/MANIFEST.json` is committed.
10. **`PROTOCOL_V1` is `1.6`** (`crates/protocol/src/version.rs:38`) and belongs to the **local**
    protocol. The control-plane protocol needs its own independent version constant. Do not reuse
    `ProtocolVersion` in a way that couples the two release cadences; design §4.2 explicitly says the
    local `Envelope` stays out of `crates/control-plane-protocol`.

### 8.3 Semantics

11. **Role name collision.** Local `ClientRole` is `Observer | Contributor | Controller | Approver`
    (`crates/daemon/src/commands.rs:3001`); control-plane RBAC is
    `observer | contributor | approver | maintainer | organization-admin` (design §5.3). They overlap
    in three names and mean different things, and `Controller` (the most privileged local role) has no
    remote counterpart. Never map one to the other; a remote role must not be able to produce a local
    `ClientRole` at all.
12. **`is_reserved_unsupported_command`** (`crates/daemon/src/commands.rs:2971`) still reserves inbox,
    analytics, automation and bundle payloads. If M3–M5 have not landed, the control plane has nothing
    real to synchronize for those domains — do not build endpoints against contracts the daemon
    answers with `protocol.unsupported-payload`.
13. **`Unknown` is the strictest value, on both class enums.** Follow `DataClassification`
    (`crates/protocol/src/artifact.rs:41-74`) and M6's `PublicationClass`: a class string from a newer
    peer deserializes to `Unknown` and publishes nothing. The control plane receiving `Unknown` must
    store the delta as unreadable rather than defaulting it to `metadata-shared`.
14. **Redact at enqueue, not at send.** `control_plane_outbox.payload` stores the already-redacted
    bytes. A sender that re-reads local authority at transmission time will happily send content a
    policy narrowing has since forbidden, and the narrowing→tombstone path assumes the outbox row is
    immutable.
15. **`at-least-once` plus "never duplicate an external effect"** means the receipt and the effect
    must be in **one** transaction on both sides. The shipped local pattern is
    `received → effect → applied` in `crates/daemon/src/commands.rs`; mirror it, and mirror
    `resume_received`'s re-drive rules for the crash-in-the-middle case.
16. **Outbound only.** The daemon opens the connection (design §7.3); nothing in M7 may bind a
    listening port on the workstation. Assert it in a test that inspects the transport, because a
    WebSocket library's "reconnect server" helper makes it very easy to do accidentally.
17. **Do not use `stable_repository_id` anywhere in the control plane.** It is path-derived
    (`crates/knowledge/src/codegraph.rs:174`) — see M6 §8.1. The control plane only ever sees M6's
    `federated_id`.
18. **Green is not done.** Every recent release here passed a fully green gate and still had real
    defects found by adversarial review afterwards. M7 adds the first network trust boundary in the
    product; budget a dedicated adversarial pass against design §15's mandatory scenario list
    (replayed tokens, webhooks, approvals; oversized payloads; reconnect after offline deletion;
    partition at every state).
