# M5 — Secret broker, marketplace, and integration pack

**Audience:** the implementer of Milestone 5 of
[`plans/2026-08-16-hybrid-platform-program.md`](../plans/2026-08-16-hybrid-platform-program.md)
(§411–464). Read [`00-conventions-and-traps.md`](./00-conventions-and-traps.md) first.
**Status:** verified against the tree on 2026-08-17 (branch `release/v0.9.0`).

The plan gives the task list, the **Files:** lines and the commit messages; this document does not
repeat them. It supplies what the plan leaves out: what exists, the column-level data model, the
contract→handler→authorization wiring, and the secret- and supply-chain rules that this milestone
lives or dies by.

M5 is the most security-sensitive milestone in the program. Two sentences carry most of it:

- **A secret is brokered, never handed over.** Plugin and guest code receives a *reference*; the
  host resolves material at the final transport injection point and nowhere else.
- **A package is verified before it can execute.** Signature and publisher trust are checked before
  installation, and installation never enables anything.

---

## 1. Status: what exists, what does not

### Missing entirely

- `crates/secrets/` and `crates/marketplace/` — neither directory exists; the workspace members
  list is `Cargo.toml:3-24`.
- `migrations/0045_secret_broker.sql`, `migrations/0046_marketplace.sql` — highest committed
  migration is `0040_session_library.sql`.
- `crates/protocol/src/marketplace.rs` — the module list is `crates/protocol/src/lib.rs:12-39`;
  there is no marketplace or secrets module and, unlike M4's automation, **nothing is reserved**.
- Any `marketplace` or `secrets` bit in `ClientCapabilities`
  (`crates/protocol/src/capabilities.rs:18-53`).
- `examples/integration-pack/` — `examples/` currently holds only `plugins/` and `remote-ui-tsx/`.

### The secret seam, precisely

Three pieces exist and one is a deliberate stub:

1. **The request vocabulary.** `HostRequest::ReadSecret { name }`
   (`crates/sandbox/src/gate.rs:113-116`), with `class()` = `"secret-read"` (line 128),
   `describe()` (line 144) and a digest arm (line 176) already written. Its doc comment says
   plainly: *"No shipped gate grants this: the brokered-secrets daemon does not exist."*
2. **The declaration ceiling.** `SandboxProfile.brokered_secrets`
   (`crates/sandbox/src/profile.rs:40`), populated from `Capability::Secret`
   (`profile.rs:72`), and enforced by `SandboxProfile::permits`
   (`crates/sandbox/src/gate.rs:314`). The ceiling half already works.
3. **The run-policy gate — the stub.** `RunPolicyAdapter::lower`
   (`crates/daemon/src/policy_gate.rs:94-121`) returns, unconditionally:

   ```rust
   HostRequest::ReadSecret { .. } => Err(GateDenied::new(
       "policy.no-secret-broker",
       "brokered secrets are not implemented",
   )),
   ```

   (`policy_gate.rs:116-119`.) **This is the exact seam M5's broker fills.** Replacing that arm
   with a brokered lowering — and doing so without weakening `WriteFile`'s neighbouring
   `policy.unsupported-action` refusal — is the load-bearing change of Task 5.2. The refusal is
   pinned by `unsupported_requests_refuse_with_their_own_codes` (`policy_gate.rs:302`), which you
   will need to rewrite rather than delete.

Two things about that seam the module docs get wrong or leave unsaid:

- `policy_gate.rs:22-29` claims *"Nothing in the workspace constructs a `SkillRunner::enforcing(...)`
  yet, so no guest currently runs under it."* `RunPolicyAdapter` **is** constructed today, at
  `crates/runtime/src/agent.rs:3622` — but only as a `PolicyReentry` for hook rewrites, never as a
  `RunPolicyGate` for a sandboxed guest. The claim is stale for construction and still true for the
  gate path. Verify before you rely on either statement.
- **Nothing raises `HostRequest::ReadSecret`.** The WASM host builds only
  `HostRequest::ReadFile` (`crates/sandbox/src/wasm.rs:462` and `:465`). There is no
  guest-callable `read_secret` host function anywhere. Implementing the gate arm alone leaves the
  path dead end-to-end; M5 must add the host function too, and it must go through
  `CapabilityBroker::request` so the `GateGrant` (`gate.rs:195-200`) is minted, not bypassed.

**And one hard blocker:** `crates/knowledge/src/manifest.rs:452-460` *refuses to load* any skill
manifest that declares `permissions.secrets`, with
`ManifestError::UnenforceableCapability { capability: "secrets", detail: "brokered secrets require
the secrets daemon, which does not exist yet; no executor reads `brokered_secrets`" }`. Until that
refusal is relaxed, no skill package can declare a secret at all. (The *sandbox* manifest parser is
more permissive — `CapabilitiesSpec.secrets` at `crates/sandbox/src/manifest.rs:438-440` is accepted
for every kind except `ThemePack` and `UiComponent`, which forbid all capabilities at
`manifest.rs:724` and `:757`.)

### Redaction discipline already in the tree — extend it, do not restart it

| Mechanism | Where |
|---|---|
| `detect_secret(text) -> Option<String>` — credential shapes (AWS, Slack, GitHub, PEM, `key=value`) plus a Shannon-entropy heuristic | `crates/knowledge/src/memory.rs:943-973` |
| Applied on the memory-curation path | `crates/knowledge/src/memory.rs:484-489`, `crates/daemon/src/commands.rs:2117` |
| Applied on the learning ledger | `crates/knowledge/src/learning.rs:801`, `:818` |
| Opaque token with `expose()` documented for one caller and a redacting `Debug` | `crates/integrations/src/github/secret.rs:18-73` |
| `ResolvedCredential` with hand-written redacting `Debug`, resolved at call time and never stored | `crates/providers/src/credential.rs:16-74` |
| Redacting `Debug` on a secret-bearing config | `crates/integrations/src/webhook/config.rs:31-41` |

### Plaintext discovery M5 replaces with references

- `GitHubToken::discover` → `gh auth token`, falling back to `GITHUB_TOKEN`
  (`crates/integrations/src/github/secret.rs:38`, `:45`, `:60-66`).
- `TAVILY_API_KEY` read directly (`crates/integrations/src/search/key.rs:79`).
- Provider API key read from an env var name (`crates/providers/src/credential.rs:176`).
- MCP server env passthrough (`crates/integrations/src/mcp/config.rs:178`) — a literal
  `env = [["GITHUB_TOKEN", "secret"]]` pair placed into a child process environment.

Task 5.2's compatibility rule is that each becomes an `env:<NAME>` **reference** resolved by the
broker at final injection, so the discovery site no longer holds material.

### Marketplace machinery already shipped — reuse, do not reinvent

| Capability | Where |
|---|---|
| `sha256:<hex>` artifact checksum | `crates/sandbox/src/verify.rs:67` (`checksum_of`) |
| Whole-manifest signing digest, domain-separated `codypendent-plugin-signature-v1`, length-committed, signature field blanked | `crates/sandbox/src/verify.rs:98-110` (`signing_digest`) |
| Verify checksum → signature → unsigned policy, in that order, with typed `VerifyError` | `crates/sandbox/src/verify.rs:125-180` (`verify_artifact`), `UnsignedPolicy` at `:26-32` (**default `Deny`**) |
| Trusted publisher key store (ed25519, add/remove/lookup/list, load/save) | `crates/sandbox/src/trust_store.rs:99-243` |
| Manifest schema + validation, `PluginKind` (`native-process`, `wasm-component`, `mcp-remote`, `ui-component`, `theme-pack`) | `crates/sandbox/src/manifest.rs:127-153`, `parse_manifest` at `:773` |
| Permission diff + "does this update expand permissions?" | `crates/sandbox/src/permission.rs:346` (`diff_manifests`), `:383` (`expands_permissions`), `:305` (`diff_resources`) |
| Lifecycle state machine — install disabled → smoke test → enable → revoke; pending update approval receipts | `crates/sandbox/src/lifecycle.rs:127-260` (`InstalledPlugin`), `PendingUpdateApproval` at `:98` |
| A working end-to-end governed store over all of the above | `crates/daemon/src/remote_ui_plugins.rs:188-800` (`RemoteUiPluginStore`) |
| Hostile-archive-safe extraction | `crates/daemon/src/remote_ui_plugins.rs:1853-1945` (`extract_package`) |
| Sandboxed subprocess with typed verdict + audit | `crates/daemon/src/hook_engine.rs`, `crates/daemon/src/hook_exec.rs` (`profile_for_hook` at `:150`, `HookRunner::run_hook` at `:204`, `DispatchAudit` at `:176`) |

`extract_package` already refuses: absolute and non-normalized paths (`normalized_path`, lines
1832-1850), duplicate normalized paths (line 1878), any entry that is not a regular file or
directory — which is how symlinks and hardlinks are rejected (line 1895), per-file size
(`MAX_PACKAGE_FILE_BYTES`, line 1902), entry/file/directory counts (lines 1868, 1887, 1902),
uncompressed total (`MAX_PACKAGE_BYTES`, line 1910), path length and depth (line 1832), and an
empty package (line 1939). It does **not** check a compression ratio, which plan Task 5.3
explicitly requires — add that, and add it in the shared location, not in a copy.

---

## 2. Data model

Conventions from `migrations/0040_session_library.sql` and `migrations/0027_hooks.sql`: TEXT UUID
keys, TEXT ISO-8601 timestamps, INTEGER 0/1 booleans with `CHECK (x IN (0,1))`, TEXT enums with
`CHECK`, partial indexes for sparse columns.

> Migrations are append-only and checksum-gated. Assign `0045`/`0046` centrally per release, then
> run `python3 .github/scripts/check_migration_immutability.py --update` and commit
> `migrations/checksums.json` in the same commit as the SQL.

### 2.1 `migrations/0045_secret_broker.sql`

```sql
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
```

### 2.2 `migrations/0046_marketplace.sql`

```sql
-- Milestone 5: durable marketplace distribution, trust and lifecycle.
--
-- The sandbox crate remains the final execution authority: this schema records
-- WHAT was distributed and WHETHER it was approved. Whether a package may RUN
-- is still decided by InstalledPlugin's lifecycle state
-- (crates/sandbox/src/lifecycle.rs:127) and by the two gates in
-- crates/sandbox/src/gate.rs. Never let a row here become an execution grant.

-- Publisher trust is DISTINCT from registry trust (plan Task 5.4): trusting the
-- registry that lists a package says nothing about who signed it.
CREATE TABLE marketplace_publishers (
    id TEXT PRIMARY KEY,                    -- the manifest's `publisher` string
    display_name TEXT NOT NULL,
    -- Raw ed25519 public key, hex. Public by definition; verify_artifact takes
    -- exactly 32 bytes (crates/sandbox/src/verify.rs:160-164).
    public_key_hex TEXT NOT NULL CHECK (length(public_key_hex) = 64),
    trust_tier TEXT NOT NULL DEFAULT 'untrusted'
        CHECK (trust_tier IN ('untrusted', 'trusted', 'first_party')),
    trusted_at TEXT,
    trusted_by TEXT,
    -- Revocation is a state, not a delete: revoking must be able to disable
    -- already-installed packages, which requires the key to remain resolvable.
    revoked_at TEXT,
    revoked_reason TEXT,
    created_at TEXT NOT NULL,
    CHECK (trust_tier = 'untrusted' OR trusted_at IS NOT NULL),
    UNIQUE (id, public_key_hex)
);

CREATE INDEX idx_marketplace_publishers_trust
    ON marketplace_publishers (trust_tier, revoked_at);

-- Stable package identity. Metadata only; nothing here is executable.
CREATE TABLE marketplace_packages (
    id TEXT PRIMARY KEY,                    -- the manifest's `id` slug
    publisher_id TEXT NOT NULL REFERENCES marketplace_publishers(id),
    kind TEXT NOT NULL CHECK (kind IN
        ('native-process', 'wasm-component', 'mcp-remote', 'ui-component', 'theme-pack')),
    display_name TEXT NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    -- A package the org policy hides must be non-disclosing: a query for it
    -- answers exactly as a query for a package that does not exist (plan Task
    -- 5.4, "hidden-package non-disclosure").
    hidden INTEGER NOT NULL DEFAULT 0 CHECK (hidden IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_marketplace_packages_publisher ON marketplace_packages (publisher_id);
CREATE INDEX idx_marketplace_packages_visible ON marketplace_packages (hidden, id);

-- IMMUTABLE once written. A version row is the durable statement "these bytes,
-- under this manifest, from this source". Updating any column here would let a
-- published version be swapped after review; publish a new version instead.
CREATE TABLE marketplace_versions (
    id TEXT PRIMARY KEY,
    package_id TEXT NOT NULL REFERENCES marketplace_packages(id),
    version TEXT NOT NULL,
    -- `sha256:<hex>` in the canonical form checksum_of produces
    -- (crates/sandbox/src/verify.rs:67). Also the content-addressed store key.
    content_hash TEXT NOT NULL CHECK (content_hash LIKE 'sha256:%'),
    -- The manifest TOML as published. Stored verbatim because signing_digest
    -- covers the WHOLE manifest (crates/sandbox/src/verify.rs:98): a
    -- re-serialized copy would not re-verify.
    manifest_toml TEXT NOT NULL,
    signature_b64 TEXT,
    signed INTEGER NOT NULL CHECK (signed IN (0, 1)),
    -- The source the artifact was fetched from, retained for audit and for the
    -- allowlist check on re-download. Redirects are followed only within the
    -- allowlist and the final URL is what is recorded.
    source_url TEXT NOT NULL,
    artifact_bytes INTEGER NOT NULL CHECK (artifact_bytes > 0),
    -- Host-computed, never publisher-asserted: a package cannot declare itself
    -- compatible (plan Task 5.3, "host computes compatibility").
    min_daemon_version TEXT,
    max_daemon_version TEXT,
    published_at TEXT NOT NULL,
    yanked_at TEXT,
    CHECK (signed = 0 OR signature_b64 IS NOT NULL),
    UNIQUE (package_id, version),
    UNIQUE (content_hash)
);

CREATE INDEX idx_marketplace_versions_package
    ON marketplace_versions (package_id, published_at DESC);

-- One row per local installation. Owner-scoped so the desktop can show "my
-- installs", but note the ownership gate treats the store as daemon-wide (§3).
CREATE TABLE marketplace_installs (
    id TEXT PRIMARY KEY,
    package_id TEXT NOT NULL REFERENCES marketplace_packages(id),
    version_id TEXT NOT NULL REFERENCES marketplace_versions(id),
    owner_uid INTEGER NOT NULL,
    -- Mirrors LifecycleState (crates/sandbox/src/lifecycle.rs:42). The sandbox
    -- value is authoritative at execution; this column exists so the daemon can
    -- answer list/inspect without loading every package.
    lifecycle TEXT NOT NULL CHECK (lifecycle IN
        ('installed_disabled', 'smoke_tested', 'enabled', 'disabled', 'revoked')),
    -- Pinning is exact-version and explicit; an update check must not move a
    -- pinned install even when a newer compatible version exists.
    pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    pinned_version TEXT CHECK (pinned = 1 OR pinned_version IS NULL),
    -- The optional session/scope an enable is limited to, mirroring
    -- InstalledPlugin::enabled_scope (crates/sandbox/src/lifecycle.rs:185).
    enabled_scope TEXT CHECK (lifecycle = 'enabled' OR enabled_scope IS NULL),
    -- Set when a publisher key or version is revoked. A revoked install must be
    -- refused at launch even if `lifecycle` was 'enabled' a moment earlier.
    revoked_at TEXT,
    revoked_reason TEXT,
    installed_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    -- One install per package per principal; a second install is an update.
    UNIQUE (package_id, owner_uid)
);

CREATE INDEX idx_marketplace_installs_owner
    ON marketplace_installs (owner_uid, lifecycle, updated_at DESC, id);
CREATE INDEX idx_marketplace_installs_version ON marketplace_installs (version_id);

-- The human decision on a permission change. Bound to the exact manifest hash
-- that was reviewed, so approve-then-substitute fails closed — the same binding
-- hooks.approved_content_hash uses (migrations/0027_hooks.sql:31-35).
CREATE TABLE marketplace_permission_receipts (
    id TEXT PRIMARY KEY,
    install_id TEXT NOT NULL REFERENCES marketplace_installs(id) ON DELETE CASCADE,
    from_version_id TEXT REFERENCES marketplace_versions(id),
    to_version_id TEXT NOT NULL REFERENCES marketplace_versions(id),
    -- The rendered PermissionDiff (crates/sandbox/src/permission.rs:413) — the
    -- text the human was actually shown, kept so an audit can reconstruct it.
    diff_rendered TEXT NOT NULL,
    -- diff.expands_permissions() (permission.rs:383). An expansion may never be
    -- auto-approved, regardless of publisher trust tier.
    expands_permissions INTEGER NOT NULL CHECK (expands_permissions IN (0, 1)),
    approved_manifest_hash TEXT NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('pending', 'approved', 'rejected')),
    decided_by TEXT,
    decided_at TEXT,
    -- Revocation invalidates PENDING receipts (plan Task 5.4), so a receipt
    -- approved after its publisher was revoked cannot be spent.
    invalidated_at TEXT,
    created_at TEXT NOT NULL,
    CHECK (decision = 'pending' OR decided_at IS NOT NULL)
);

CREATE INDEX idx_marketplace_permission_receipts_install
    ON marketplace_permission_receipts (install_id, created_at DESC);
CREATE INDEX idx_marketplace_permission_receipts_pending
    ON marketplace_permission_receipts (decision) WHERE decision = 'pending';

-- Append-only revocation feed. Kept separate from the publisher/version rows so
-- a revocation is a durable event with provenance, not a mutable flag, and so a
-- replay can reconstruct what was known when.
CREATE TABLE marketplace_revocations (
    id TEXT PRIMARY KEY,
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('publisher', 'package', 'version')),
    subject_id TEXT NOT NULL,
    reason TEXT NOT NULL,
    -- Where the revocation came from: a local operator, or a signed registry
    -- feed. A registry-sourced revocation is only honoured from an allowlisted
    -- source, and never grants anything — revocation is one-way.
    source TEXT NOT NULL CHECK (source IN ('operator', 'registry')),
    recorded_at TEXT NOT NULL,
    UNIQUE (subject_kind, subject_id, recorded_at)
);

CREATE INDEX idx_marketplace_revocations_subject
    ON marketplace_revocations (subject_kind, subject_id);
```

---

## 3. Contract → implementation mapping

M5 introduces new protocol surface rather than implementing a reservation. Everything below is
additive and must go through the full new-command checklist (conventions §1).

| New protocol type | Daemon module / function | Handler site | `role_permits` |
|---|---|---|---|
| `marketplace::MarketplaceRequest::{Discover, Inspect}` | `crates/marketplace/src/catalog.rs` via `crates/daemon` marketplace ops | `crates/daemon/src/server.rs`, new `CommandBody::ManageMarketplacePackage` arm — connection-level like `InstallUiPlugin` | any attached role (read) |
| `::Install` (always disabled) | `crates/marketplace/src/{distribution,store}.rs` → `InstalledPlugin::install_disabled` (`crates/sandbox/src/lifecycle.rs:201`) | same arm | `Controller` |
| `::SmokeTest` | reuse the `RemoteUiPluginStore::smoke_test` shape (`crates/daemon/src/remote_ui_plugins.rs:312`) | same arm | `Controller` |
| `::Enable` / `::Disable` / `::Remove` | `crates/sandbox/src/lifecycle.rs:252` / `:507`-equivalents | same arm | `Controller` |
| `::Pin` / `::CheckUpdates` | `crates/marketplace/src/{compatibility,update}.rs` | same arm | `Controller` (pin), read (check) |
| `::ApproveUpdate` / `::RejectUpdate` | permission receipts + `PendingUpdateApproval` (`crates/sandbox/src/lifecycle.rs:98-118`) | same arm | `Approver \| Controller` |
| `::TrustPublisher` / `::RemoveTrustedPublisher` / `::Revoke` | `crates/marketplace/src/trust.rs` over `TrustedPublishers` (`crates/sandbox/src/trust_store.rs:192`, `:210`) | same arm | `Controller` |
| `::Unknown` | — | same arm | `false` |
| `secrets::SecretReferenceRequest::{Create, List, Rotate, Revoke}` | `crates/daemon/src/secret_gate.rs` + `crates/codypendentd/src/secrets.rs` | new `CommandBody::ManageSecretReference` arm | `Controller` (mutate), any role (list — **metadata only**) |
| `HostRequest::ReadSecret` (already typed, `crates/sandbox/src/gate.rs:113`) | replace the stub arm in `RunPolicyAdapter::lower` (`crates/daemon/src/policy_gate.rs:116-119`) with a brokered lowering | `CapabilityBroker::request`, from a new guest host function beside `wasm.rs:462` | n/a — not a client command |

### The "done" signals

M5 removes **nothing** from `is_reserved_unsupported_command`
(`crates/daemon/src/commands.rs:2971-2986`) — every entry there belongs to M2.x, M3.x, M4 or the
bundle work. Its equivalents for this milestone are the two refusals that must disappear:

1. `crates/daemon/src/policy_gate.rs:116-119` — the `HostRequest::ReadSecret` arm returning
   `policy.no-secret-broker`. While it stands, no guest can read any secret, however much broker
   code exists.
2. `crates/knowledge/src/manifest.rs:452-460` — `ManifestError::UnenforceableCapability` for
   `permissions.secrets`. While it stands, no skill package can *declare* a secret, so the ceiling
   at `crates/sandbox/src/gate.rs:314` has nothing to admit.

A third, weaker signal: `crates/sandbox/src/gate.rs:107-112`'s doc comment (*"No shipped gate
grants this"*) and `crates/daemon/src/policy_gate.rs:22-29`'s wiring-status paragraph both become
false and must be updated in the same commit.

### The rest of the checklist, per new command

1. Variant on `CommandBody` (`crates/protocol/src/command.rs`).
2. Arm in `role_permits` (`crates/daemon/src/commands.rs:3001`) with a **decided** floor —
   `_ => false` at line 3038 silently kills the feature.
3. Representative body added to `every_client_issued_command_has_a_decided_role_floor`
   (`crates/daemon/src/commands.rs:5516`).
4. `named_resources()` (`crates/protocol/src/command.rs:985`) returns real resources:
   - **Marketplace:** add `DaemonStore::Marketplace` to `crates/protocol/src/command.rs:946-961`
     and return `vec![NamedResource::DaemonStore(DaemonStore::Marketplace)]`. This mirrors
     `DaemonStore::UiPlugins` (`command.rs:951-953`, gated at `server.rs:5250-5255` on
     `principal.owns(state.daemon_uid)`) and is correct for the same reason: an installed package
     is an arbitrary-code surface for the whole daemon, and the `marketplace_installs.owner_uid`
     column is for attribution, not authorization.
   - **Secrets:** add `NamedResource::SecretReference(SecretReferenceId)` and resolve `owner_uid`
     per row — secrets are principal-bound (design spec §9), so the daemon-wide shape is wrong here.
5. Ownership arm in `authorize_command` (`crates/daemon/src/server.rs:5143`). Copy the
   `NamedResource::Artifact` arm (`server.rs:5202-5213`) exactly in shape: resolve the owner from
   the daemon's own storage, compare with `owner.unwrap_or(state.daemon_uid) == principal.uid()`,
   and refuse with a **generic** not-found so unauthorized and absent are indistinguishable.
   `secret.not-found` must be the answer for both "not yours" and "does not exist", and the same
   goes for a hidden marketplace package.
6. Golden vectors under `protocol-vectors/` (new `marketplace.json`, `secrets.json`), and **each new
   vector added to the `modeled` or `notModeled` list in
   `extensions/vscode/test/protocol-vectors.test.ts`** or `assertPartitionIsComplete` fails and the
   `doc-count:vitest` markers drift (conventions §5).
7. New `ClientCapabilities` bits (`crates/protocol/src/capabilities.rs:18-53`) — additive,
   `#[serde(skip_serializing_if = "is_false")]`, defaulting `false`, and used only to decide what
   the daemon *offers*, never to authorize.

---

## 4. Security requirements

### 4.1 Secrets are brokered — never handed over

- **Never to plugin or guest code.** The guest gets a *name*; the host resolves. The declaration
  ceiling (`SandboxProfile.permits`, `crates/sandbox/src/gate.rs:314`) must match the requested
  name against `brokered_secrets` *before* the run policy is consulted — that ordering is
  deliberate (`gate.rs:52-54`: a hostile package must not be able to probe operator policy with
  undeclared requests).
- **Never via environment.** `SandboxProfile.brokered_secrets` is documented as *"Named secrets
  brokered per call (never placed in env)"* (`crates/sandbox/src/profile.rs:39-40`). The
  `env_allowlist` is a separate, non-secret field (`profile.rs:33`); do not route material through
  it. The MCP config's literal `env = [["GITHUB_TOKEN", "secret"]]`
  (`crates/integrations/src/mcp/config.rs:178`) is exactly the shape being replaced.
- **Never logged.** Leased material must be a type that is **not** `Clone`, **not** `Serialize`,
  **not** `Debug`-printable, with a single documented accessor — the `GitHubToken::expose` pattern
  (`crates/integrations/src/github/secret.rs:26-32`) and the hand-written redacting `Debug` on
  `ResolvedCredential` (`crates/providers/src/credential.rs:36-53`). Implement `Drop` to zeroize.
  The `GateGrant` precedent (`crates/sandbox/src/gate.rs:195-200`) shows the shape: private fields,
  no `Default`, no `Deserialize`, no public constructor.
- **Never written to a file.** Not to config, not to a temp file for a subprocess, not to a support
  bundle. The lease is resolved at the final transport injection point — setting one HTTP header,
  or writing one field of one request — and dropped.
- **Never in the database.** §2.1's schema has no value column by construction. The exit gate
  (plan Task 5.6) scans logs, DB, events, artifacts, support bundles and test output for sentinel
  secrets; make that scan a test, not a manual step.
- **Tie into the existing redaction discipline.** Anything a broker or backend produces that could
  reach a durable note, memory, learning record or audit line goes through
  `codypendent_knowledge::detect_secret` (`crates/knowledge/src/memory.rs:943`) first, as
  `crates/daemon/src/commands.rs:2117` and `crates/knowledge/src/learning.rs:801` already do. This
  is defence in depth for the case where the primary discipline fails, not a substitute for it.
- **No post-acceptance widening.** Design spec §9 (`…-design.md:377-379`): *"A runner cannot ask
  for a broader secret after accepting a job; the job must be rejected and resubmitted with a newly
  reviewed specification."* Enforce it structurally: `secret_leases` binds
  `(principal_uid, organization_id, repository_id, job_id, capability)` at issue, and
  `secret_references.accepted_digest` is recomputed and compared on every resolve.
- **Order of authorization for `ReadSecret`** (plan Task 5.2): manifest ceiling → run policy →
  broker. The ceiling is already applied by `CapabilityBroker` before the gate is called
  (`crates/daemon/src/policy_gate.rs:14-16` — *"applying it twice would hide which gate refused"*).
  Do not add a third check inside the broker that duplicates either.
- **An unattended guest cannot answer a prompt.** `RunPolicyAdapter::authorize` turns
  `Decision::RequireApproval` into a refusal, not a wait (`policy_gate.rs:152-155`). A secret whose
  policy demands approval must be refused for a sandboxed guest, never granted on retry.
- **Deny is the default posture on backend failure.** A Vault outage, an expired token or an
  unsupported platform keychain resolves to a refusal with a dotted code and an audit row — never
  to a fallback to an environment variable.

### 4.2 Marketplace packages verify before they execute

- **Signature and publisher trust before anything runs.** Reuse `verify_artifact`
  (`crates/sandbox/src/verify.rs:125`) and resolve the publisher key from the trust store
  (`crates/sandbox/src/trust_store.rs:219`, `key_for`). Do not write a second verifier: the digest
  is domain-separated and versioned (`verify.rs:89-96`) and the crate deliberately refuses any
  weaker legacy form.
- **`UnsignedPolicy::Deny` is the default** (`crates/sandbox/src/verify.rs:26-32`). An "allow
  unsigned" path is an explicit operator opt-in per install, never a config default and never an
  org default.
- **Checksum before signature.** `verify_artifact` checks the checksum first (`verify.rs:131-147`)
  because *"a signature over a checksum means nothing if the checksum does not describe the bytes
  in hand"*. Keep that order in any wrapper.
- **Install never enables.** `InstalledPlugin::install_disabled`
  (`crates/sandbox/src/lifecycle.rs:201`) → smoke test → explicit enable. Design spec §8.6:
  *"Installation never enables executable code automatically."* The `RemoteUiPluginStore` flow
  (`crates/daemon/src/remote_ui_plugins.rs:230`, `:312`, `:457`) is the working precedent.
- **Permission expansion always needs a fresh human receipt.** `diff_manifests` →
  `expands_permissions` (`crates/sandbox/src/permission.rs:346`, `:383`), bound to the exact
  manifest hash reviewed, so approve-then-substitute fails closed. Publisher trust tier does not
  waive this.
- **Revocation is retroactive.** Revoking a publisher key must disable installed and cached
  packages and invalidate pending receipts (plan Task 5.4). Recheck revocation at *launch*, not
  only at install: `marketplace_installs.revoked_at` must be consulted on the execution path.
- **Hostile archives.** Reuse `extract_package`'s checks
  (`crates/daemon/src/remote_ui_plugins.rs:1853-1945`) — but **lift it into a shared module**
  (`crates/sandbox/src/package.rs` is the natural home, since `crates/marketplace` will depend on
  the sandbox crate and the daemon already does) rather than copying it. It is currently a private
  free function in a daemon module and is unreachable from a new crate. Add the compression-ratio
  check plan Task 5.3 requires; it is the one limit `extract_package` lacks.
- **Download controls.** Source allowlist, TLS, bounded redirects that stay inside the allowlist,
  size ceiling checked against `Content-Length` *and* enforced on the stream, and SSRF refusal for
  private/link-local/loopback destinations unless explicitly allowlisted.
- **Hidden means non-disclosing.** A package hidden by org policy answers identically to a package
  that does not exist — in the error body, the result count and the cursor.
- **The sandbox stays the final authority.** Plan Task 5.4: *"Keep sandbox `InstalledPlugin`
  lifecycle as final execution authority."* A `marketplace_installs.lifecycle` value is a cached
  projection for listings; the execution path loads the real `InstalledPlugin`.

### 4.3 The integration pack

Every first-party integration (GitLab, Linear/Jira, Slack/Teams, generic webhook,
OpenAI-compatible providers, MCP, ACP) ships with a least-privilege `plugin.toml` under
`examples/integration-pack/*/`, installs through the real marketplace path, and:

- injects auth from a **lease**, at the request-construction site only;
- bounds response size and applies redirect/SSRF controls;
- makes external effects idempotent (a retried post does not double-post);
- sanitizes hostile text before it reaches model context or a durable record —
  `crates/sandbox/src/sanitize.rs` exists for this.

---

## 5. Acceptance criteria

1. **The secret gate is no longer a stub.** `crates/daemon/src/policy_gate.rs` contains no
   `policy.no-secret-broker` arm; a declared, policy-allowed `ReadSecret` yields a `GateGrant` whose
   authority names the broker.
   → `policy_gate::a_declared_and_policy_allowed_secret_read_is_granted`
2. **Undeclared secrets are still refused by the ceiling, before policy.** A `ReadSecret` for a name
   absent from `brokered_secrets` is refused with the ceiling's code, not the policy's.
   → `policy_gate::an_undeclared_secret_is_refused_by_the_ceiling`
3. **A guest can actually ask.** A WASM guest calling the new host function reaches
   `CapabilityBroker::request` and receives material only through the grant path.
   → `wasm_adversary_it::guest_secret_read_goes_through_the_broker`
4. **Skill manifests can declare secrets again.** A manifest with `permissions.secrets` loads
   without `ManifestError::UnenforceableCapability`.
   → `knowledge::manifest::secrets_are_enforceable_once_the_broker_exists`
5. **No secret in `Debug`, `Serialize`, or audit.** Leased material has no `Debug`/`Serialize`
   impl (compile-fail test), and every `secret_audit` row for a full issue/use/deny/rotate/revoke
   cycle is free of the sentinel value.
   → `broker_it::leased_material_has_no_debug_or_serialize`,
     `broker_it::audit_records_events_without_values`
6. **Leases are context-bound and idempotent.** The same `(principal, org, repository, job,
   capability)` issued twice returns one lease; a different capability is refused, not narrowed.
   → `broker_it::issue_is_idempotent_per_context`,
     `broker_it::a_job_cannot_widen_its_capability_after_acceptance`
7. **Expiry and revocation are enforced at use, not only at issue.**
   → `broker_it::an_expired_or_revoked_lease_is_refused_at_use`
8. **Each backend honours its contract.** Environment references resolve at use (never at
   registration); an unsupported platform keychain returns a typed refusal, never a fallback;
   managed values are envelope-encrypted; Vault TTL/revoke round-trips; workload tokens are
   audience-bound.
   → `backends_it::{environment,keychain,managed,vault,workload_identity}_contract`
9. **Backend outage fails closed.** A Vault outage produces a refusal plus an audit row and never
   falls back to an env var.
   → `backends_it::vault_outage_denies_rather_than_falling_back`
10. **Compatibility references replace plaintext discovery.** `GitHubToken::discover`,
    `TAVILY_API_KEY` and the provider API-key path all resolve through the broker as
    `env:<NAME>` references; grep proves no direct `std::env::var` credential read remains on those
    paths.
    → `secrets_compat_it::integration_credentials_resolve_through_the_broker`
11. **Hostile archives are refused.** Oversize, excess entry/file/directory counts, excess
    compression ratio, absolute and `..` paths, symlinks and hardlinks, duplicate normalized paths,
    unexpected executable files, checksum mismatch, signature mismatch, off-allowlist redirects.
    → `distribution_it::hostile_archive_matrix_is_refused`
12. **Verification precedes storage.** A checksum-mismatched artifact leaves no package tree and no
    row behind.
    → `distribution_it::a_failed_verification_stores_nothing`
13. **Install never enables.** A freshly installed package is `installed_disabled`, cannot execute,
    and requires a smoke test then an explicit enable.
    → `distribution_it::install_is_inert_until_explicitly_enabled`
14. **Permission expansion demands a fresh receipt bound to the reviewed hash.** Approving a diff
    and then substituting a different manifest fails closed.
    → `trust_it::permission_expansion_receipt_is_hash_bound`
15. **Publisher trust and registry trust are distinct.** A trusted registry listing an untrusted
    publisher's package does not make it installable.
    → `trust_it::registry_trust_does_not_imply_publisher_trust`
16. **Revocation is retroactive and invalidates pending receipts.** Revoking a key disables
    installed and cached packages and blocks a pending update approval from being spent.
    → `trust_it::revocation_disables_installed_and_invalidates_pending`
17. **Hidden packages are non-disclosing.** Inspecting a hidden package and inspecting a
    non-existent one return byte-identical errors, and list counts/cursors do not differ.
    → `catalog_it::hidden_and_absent_packages_are_indistinguishable`
18. **Unauthorized and absent are indistinguishable for secrets.** A `Get` for another uid's secret
    reference and for a random id return the same `secret.not-found`.
    → `secret_gate_it::foreign_and_missing_references_are_indistinguishable`
19. **Every new command has a decided role floor and is enumerated in the guard test.**
    → `commands::every_client_issued_command_has_a_decided_role_floor`
20. **Each integration passes its mock-server contract.** Lease injection, bounded responses,
    retries and rate limits, idempotent external effects, SSRF/redirect controls, hostile text
    sanitization.
    → `integration_pack_it::<service>_contract`
21. **Every integration installs through the real marketplace path and smoke-tests before enable.**
    → `integration_pack_it::first_party_pack_installs_through_the_marketplace`
22. **The exit-gate sentinel scan is clean.** After the full suite, no sentinel secret appears in
    logs, the SQLite file, event bodies, artifacts, support bundles or test output.
    → `secrets_exit_gate_it::no_sentinel_secret_escapes`
23. **The CI gates `cargo test` does not run are green.**
    `check_migration_immutability.py`, `check_doc_test_counts.py --skip-vitest`,
    `check_docs_manifest.py`, and the `extension` job's vector partition.

---

## 6. Gotchas

**The `ReadSecret` path is dead at both ends, not one.** Implementing only the gate arm
(`policy_gate.rs:116`) leaves nothing that can call it: the WASM host builds only
`HostRequest::ReadFile` (`crates/sandbox/src/wasm.rs:462`, `:465`). Budget for a host function, its
ABI, and its adversarial tests.

**`GateGrant` is deliberately unforgeable and that constrains your design.** It is not `Clone`, has
no public constructor, and can only be minted from a `GateSeal` the broker hands out per in-flight
request (`crates/sandbox/src/gate.rs:56-67`, `:184-200`). A grant is bound to the request digest, so
a grant for `ReadSecret { name: "a" }` cannot authorize `"b"`. Do not try to cache or reuse grants.

**`HostRequest::digest` is private to the sandbox crate.** `policy_gate.rs:148-155` notes that the
daemon cannot compute it, which is one reason a `RequireApproval` cannot be turned into a bound
approval there. If the broker needs a stable request identity, add an accessor to the sandbox crate
deliberately — do not re-hash the fields in the daemon and hope they match.

**`extract_package` is private to a daemon module.** It is a free function in
`crates/daemon/src/remote_ui_plugins.rs:1853`, and `crates/marketplace` cannot call it. Move it
(with `normalized_path`, `create_private_dir`, `atomic_write_once`, `freeze_package_tree`,
`sync_directory` — lines 1821-2142) into a shared crate and have `remote_ui_plugins.rs` call the
shared version. Copying it forks a security control.

**`RemoteUiPluginStore` is filesystem-and-HMAC-record based, not SQL.** It stores records under
`<data_dir>/plugins/remote-ui/{records,artifacts,packages,tmp,locks}` with an HMAC record key
(`remote_ui_plugins.rs:198-227`) and file locks. `0046_marketplace.sql` introduces a SQL store
beside it. Decide explicitly whether the marketplace store owns the durable truth and
`RemoteUiPluginStore` becomes a consumer, or the two coexist — and write that decision down. Two
independent lifecycle authorities for the same package is how a revoked plugin stays enabled.

**`signing_digest` covers the *whole* manifest** (`crates/sandbox/src/verify.rs:98-110`), serialized
via `serde_json::to_vec` with the signature field blanked. Storing a re-serialized or normalized
manifest and verifying against that will fail. Persist `manifest_toml` verbatim (§2.2) and re-parse.

**`UnsignedPolicy` defaults to `Deny` — keep it that way.** `#[derive(Default)]` on the enum makes
`Deny` the default (`verify.rs:26-32`). A `#[serde(default)]` config field for "allow unsigned" that
someone later flips to `true` in a fixture will quietly disable supply-chain verification across
every test.

**`ThemePack` and `UiComponent` forbid all capabilities.** `crates/sandbox/src/manifest.rs:721-736`
and `:754-767` reject a manifest of those kinds declaring filesystem, network, secrets or
subprocess. An integration-pack manifest that needs a secret cannot be either kind.

**`RepositoryId` is a one-way hash with no reverse lookup.** `secret_references.repository_id` and
`secret_leases.repository_id` scope a credential to a checkout, but
`crates/codypendentd/src/scan.rs:1130` derives the id from the canonical path and there is no
`repositories` table. Derive the id from the path at authorization time; do not store a path in the
secrets schema you would then have to keep in sync.

**Secret material must not survive a panic.** `Drop`-based zeroization does not run on
`panic = "abort"` and does not run for a value moved into a leaked future. Keep the resolve →
inject → drop window as small as a single function body.

**Adding tests drifts the doc-count markers.** M5 adds a lot of tests across several crates; the
`<!-- doc-count:test … expect=N -->` markers in `ROADMAP.md` must describe the **committed** tree,
and the `doc-count:vitest` markers are deferred locally by `--skip-vitest` so a local run reports
OK while CI fails (conventions §5).

**Clippy runs with `--all-targets`, and the keychain backend is macOS-only.** A
`#[cfg(target_os = "macos")]` helper with no Linux caller is the exact shape that passes locally on
a Mac and fails the Linux lint job as dead code.
