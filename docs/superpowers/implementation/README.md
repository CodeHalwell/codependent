# Milestone Implementation Guides

Supporting documentation for
[`plans/2026-08-16-hybrid-platform-program.md`](../plans/2026-08-16-hybrid-platform-program.md).

## What lives where

Three documents describe this programme, and they do different jobs. Read them in this order:

| Document | Answers |
|---|---|
| [`specs/…-design.md`](../specs/2026-08-16-hybrid-platform-program-design.md) | **Why** — architecture, authority boundaries, identity/RBAC, publication classes |
| [`plans/…-program.md`](../plans/2026-08-16-hybrid-platform-program.md) | **What** — milestone/task decomposition, the files each task touches, commit messages, exit gates |
| **These guides** | **How** — column-level data models, reserved-contract → implementation mapping, the authorization work each command needs, acceptance criteria, and the traps |

The guides deliberately do **not** restate the plan or the spec. If a guide and the plan disagree,
the guide is usually right and the disagreement is called out in its own text — see
"Where the plan contradicts shipped code" below.

## Read first

**[`00-conventions-and-traps.md`](00-conventions-and-traps.md)** — before your first milestone.
It covers the failure modes that have actually cost this project working features and CI time: the
`role_permits` catch-all that has shipped three unreachable commands, the two CI gates
`cargo test` never runs, migration checksum immutability, and the reference implementations worth
copying instead of designing.

## The guides

| Milestone | Guide | Adds |
|---|---|---|
| M2 (remaining) | [`M2-session-library-and-clients.md`](M2-session-library-and-clients.md) | `0041_session_bundles` DDL, the shared-TS-client gap, bundle import/export identity mapping |
| M3 | [`M3-inbox-and-analytics.md`](M3-inbox-and-analytics.md) | `0042_inbox` + `0043_execution_observations` DDL, `dedup_key` derivation per entry kind, the producer seams |
| M4 | [`M4-automation.md`](M4-automation.md) | `0044_automation` DDL, webhook signature/replay rules, what must change in the existing ingest path |
| M5 | [`M5-secrets-and-marketplace.md`](M5-secrets-and-marketplace.md) | `0045_secret_broker` + `0046_marketplace` DDL, brokered-secret discipline, signed-package verification reuse |
| M6 | [`M6-federation.md`](M6-federation.md) | `0047_graph_publication` + `0048_multi_repo_campaigns` DDL, publication-class algebra and edge inheritance |
| M7 | [`M7-control-plane.md`](M7-control-plane.md) | `0049_control_plane_sync` (SQLite) **and** PostgreSQL `0001–0005`, multi-tenant non-disclosure, 9-checkpoint sub-phasing |
| M8 | [`M8-self-hosted-runners.md`](M8-self-hosted-runners.md) | runner protocol, workload identity, lease/heartbeat/cancellation, confinement posture |
| M9 | [`M9-managed-execution-and-quality.md`](M9-managed-execution-and-quality.md) | PostgreSQL `0009_runner_pools` + `0010_quality_observations`, measured shadow/canary/drift/rollback extending the existing routing + eval gate |

### Migration numbering

Two sequences run in parallel, and they **reuse the same low numbers**. Read the path, not the
number.

| | Root `migrations/` (SQLite) | `crates/control-plane/migrations/` (PostgreSQL) |
|---|---|---|
| Highest today | `0049_control_plane_sync.sql` | `0010_quality_observations.sql` |
| Claimed by the guides | `0041–0043` (M2/M3), `0044–0046` (M4/M5), `0047–0048` (M6), `0049` (M7) | `0001–0005` (M7), `0006–0008` (M8), `0009–0010` (M9) |
| Landed | **all of them** — every number the guides claimed now exists on disk | **all of them** — `0001–0010` |
| Rule | append-only **and checksum-gated** | forward-only |

Every migration the guides claimed has landed. **Schema landing is not feature landing**, and this
sequence is the clearest example in the repository: `0006`–`0010` on the PostgreSQL side have no
Rust caller at all (finding 16), and `0047`/`0048` on the SQLite side are written and read only by
the orphaned federation crate (findings 13–14). Root `migrations/` also has a genuine gap at
`0020`/`0021`, which never existed and are correctly absent from `migrations/checksums.json`
(47 entries, 47 `.sql` files).

So `0010_quality_observations.sql` (new, PostgreSQL, M9) and `0010_workflow_runs.sql` (shipped,
SQLite) coexist deliberately; likewise `0004_sync.sql` and `0004_codegraph_source_path.sql`. Within
each sequence the numbers are non-overlapping — verified across all eight guides, which were
written concurrently by different authors.

## Where the plan contradicts shipped code

Writing these guides was an audit as much as a documentation exercise. The findings below change
the work, not just its description — each was verified against the code.

**Re-verified against the tree on 2026-08-17**, after the M2–M9 implementation wave. Each finding
now carries its current verdict. A finding is only **resolved** when the fix is on a path a client
can actually reach; "resolved in a crate nothing depends on" is recorded as **moved**, not fixed.

1. ~~**`stable_repository_id` cannot serve as a federated identity (M6).**~~ — **MOVED, not
   resolved.** The correct derivation now exists: `derive_federated_id` =
   `SHA-256(root_commit || '\n' || normalized_remote)` with a URL normalizer
   (`crates/federation/src/identity.rs:1-6`, `normalize_remote` at `:25`). But
   `crates/knowledge/src/codegraph.rs:174` is **unchanged** — still the SHA-256 of the canonical
   local path — and nothing in the daemon calls the federation version, because nothing depends on
   `codypendent-federation` at all (see finding 13). Two identities now exist and the wrong one is
   the only one in use.
2. ~~**`MutateInbox` has a vacuous ownership gate by construction (M3).**~~ — **RESOLVED.**
   `NamedResource::InboxEntry` exists (`crates/protocol/src/command.rs:1079`) and `MutateInbox`
   returns it (`:1172-1177`); the `authorize_command` arm resolves `owner_uid` from `inbox_entries`
   and answers a generic `inbox.not-found` (`crates/daemon/src/server.rs:6033-6052`). The
   `ExportBundle` half is covered defensively rather than by the gate: an empty
   `source_session_ids` still yields an empty `Vec` (`command.rs:1208-1213`), but the handler
   rejects the empty list outright (`crates/daemon/src/bundles.rs:354`) and filters every source by
   `owner_uid` (`:374`).
3. **New golden vectors in a subdirectory would be unguarded (M7).** — **PARTIALLY RESOLVED, and
   moot in fact.** The VS Code partition suite now recurses, with a comment naming this exact gap
   and a meta-guard asserting it walks nested directories
   (`extensions/vscode/test/protocol-vectors.test.ts:97-115`, `:1400-1419`). The SDK suite is
   **still flat** (`sdk/protocol/test/protocol-vectors.test.ts:2369-2371`), as is
   `extensions/vscode/test/editor-actions.test.ts:437`. Practically moot only because
   `protocol-vectors/control-plane/v1/` was never created — **M7.1's golden vectors do not exist**,
   nor do M6's. Fix the SDK suite before the first nested vector lands, not after.
4. **`is_reserved_unsupported_command` is M4's done-signal but not M5's.** — **BOTH REFUSALS GONE;
   read how before calling it resolved.** `crates/knowledge/src/manifest.rs` no longer raises
   `UnenforceableCapability` for `permissions.secrets` — only `permissions.network` (`:444`) and
   `trust.signature_required` (`:459`) remain — and `secrets` now flows through to
   `CapabilityRequest::Secret` (`:576-581`). That half is a genuine fix. **But
   `policy.no-secret-broker` was not replaced with a broker decision; it was replaced with an
   unconditional allow.** `eval_read_secret` (`crates/daemon/src/policy/mod.rs:541-552`) returns
   `Decision::Allow` with reason `policy.secret-brokered` for *any* secret name, with no capability
   grant and no approval, lowered at `crates/daemon/src/policy_gate.rs:112-114`. A refusal that
   correctly failed closed became a gate that always passes. Two stale comments in `manifest.rs`
   (`:456`, `:868`) still describe the removed `secrets` check. Note also that
   `is_reserved_unsupported_command`
   (`crates/daemon/src/commands.rs:3361-3369`) is now down to a single arm —
   `MutateSessionLifecycle { action: Unknown }` — so §3 of
   [`00-conventions-and-traps.md`](00-conventions-and-traps.md) no longer describes an inventory of
   remaining work. **`role_permits`' `_ => false` catch-all is the live version of that trap now**
   (finding 13).
5. **The `ReadSecret` path is dead at both ends (M5).** — **HALF RESOLVED. The host function now
   exists and still returns no secret.** `host_read_secret` is at
   `crates/sandbox/src/wasm.rs:517`, raises `HostRequest::ReadSecret` at `:541`, is authorized
   through `CapabilityBroker` at `:545-548`, and is linked as the guest-callable
   `codypendent.read_secret` at `:702` — the missing piece this finding named. But at `:562` it
   writes **the secret's name** back into guest memory (`let bytes = requested.as_bytes()`), never
   calling `crates/secrets/` at all. It is an echo behind an authorization check. It is also
   unreachable: `WasmHost` (`wasm.rs:608`) has one caller outside its crate,
   `crates/knowledge/src/skill_exec.rs:583`, which nothing in `crates/daemon/src` or
   `crates/codypendentd/src` invokes — and the only guest profile the daemon builds hardcodes
   `brokered_secrets: Vec::new()` (`crates/daemon/src/commands.rs:3682`,
   `crates/daemon/src/hook_exec.rs:165`), so the broker would deny every request regardless
   (`crates/sandbox/src/gate.rs:308`).
6. **`ModelUsage` cannot express the analytics contract (M3).** — **STILL STANDS at the source;
   worked around in the store.** `ModelUsage` (`crates/runtime/src/agent.rs:655-668`) still carries
   only `prompt_tokens`/`completion_tokens`/`cost_micros`. `0043_execution_observations.sql` added
   nullable `cached_tokens` and `reasoning_tokens` (`:58-60`) and its own comment cites
   `agent.rs:655-668` as the reason they cannot be filled (`:54`). The columns are therefore
   correct and permanently `NULL` — which is the honest outcome under §8, but means the analytics
   contract is not measurable until the runtime type grows the fields.
7. **CI cannot run M7's tests as specified.** — **STILL STANDS.** `.github/workflows/ci.yml` has no
   Postgres service, no object store, and no `DATABASE_URL`. `crates/control-plane/tests/
   migrations_it.rs:28-32` **skips itself** when `DATABASE_URL` is unset, and every other
   control-plane test runs against `MemoryStore`. So all ten PostgreSQL migrations and the whole of
   `crates/control-plane/src/store/postgres.rs` are never executed by CI. The sqlx half changed
   shape: the workspace dependency is still SQLite-only (`Cargo.toml:147`, no `postgres`), and
   `crates/control-plane/Cargo.toml:36-45` declares its own non-workspace `sqlx` with
   `postgres` + `tls-rustls`. Cargo still unifies features across the graph, so
   `--workspace --all-features` builds every SQLite consumer against a Postgres-enabled sqlx.
8. ~~**`RepositoryId` cannot address a checkout (M4).**~~ — **RESOLVED pragmatically.**
   `0044_automation.sql` carries `repository_id TEXT` **and** `repository_path TEXT` side by side
   (`:45`, `:51`), with the id derived through `stable_repository_id` and the derivation site named
   in a comment (`:47`). No local `repositories` table was added, and none is needed for this.
9. **Canary evidence is caller-supplied (M9).** — **CHANGED, NOT FIXED — and the change bricked the
   pipeline.** The wire contract is untouched: `PromotionAction::ObserveCanary { metrics }`
   (`crates/protocol/src/command.rs:1382`) still carries five client-supplied numbers, and the CLI
   still makes an operator type all five (`crates/cli/src/main.rs:1090-1104`, assembled at
   `:1143-1160`). What is new is that the daemon now **refuses** it —
   `promotion.caller-supplied-canary-evidence` at `crates/codypendentd/src/promotion.rs:264-270` —
   without building the server-measured replacement M9.5 was supposed to deliver. The result is a
   dead end, not a fix: `StartCanary` succeeds, `ObserveCanary` is hard-rejected, and
   `FinishCanary` therefore always fails `CanaryInsufficientEvidence { observed: 0, required: 100 }`
   (`crates/eval/src/promote.rs:432-438`), because `PromotionStore::observe_canary_samples`
   (`crates/eval/src/store.rs:199`) has no non-test caller. **No candidate can reach `Promoted` on
   any shipped path.** Design §16 criterion 15 is still false; it is now false in the other
   direction.
10. **A `SandboxSpec` with a network allowlist is not translatable (M8).** — **STILL STANDS,
    unchanged.** `validate_enforceable_profile` still rejects any non-empty `network_allowlist`
    (`crates/sandbox/src/executor.rs:1284-1289`) from all four entry points (`:1030`, `:1082`,
    `:1206`, `:1251`), and `bwrap_argv` still emits `--unshare-net` unconditionally (`:647`). Two
    doc comments in the same file (`:25`, `:604`) still say "unless the allowlist is non-empty" and
    are now wrong — harmless, because both paths fail closed, but they should be corrected.
11. **`RouteArm` has never had a driver (M9).** — **STILL STANDS, unchanged.**
    `crates/routing/src/arms.rs:15-25` still records that the exit criterion "is not evaluable by
    any shipped path", and it is still accurate: `codypendent eval` still has exactly one
    subcommand, `Run` (`crates/cli/src/main.rs:931-960`). The only non-test consumer,
    `crates/eval/src/experiment.rs`, has no production caller of its own.
12. **Crash recovery clears the field a remote executor would key on (M8).** — **STILL STANDS,
    unchanged, and now un-actioned.** `reset_interrupted_node`
    (`crates/workflow/src/store.rs:1005`) still sets `agent_run_id = NULL, cost_json = NULL` while
    preserving `attempt` (`:1013-1015`). No remote idempotency key derived from
    `(workflow_run_id, node_id, attempt)` exists anywhere. `0006_runner_jobs.sql` anticipates the
    shape — `idempotency_key TEXT NOT NULL` described as "Caller-chosen" (`:50-53`), unique per org
    (`:94`), one attempt per job (`:124`) — and **nothing populates any of it.**

### Found by this re-verification

13. **`role_permits`' `_ => false` catch-all has claimed a whole milestone (M6).** All fourteen
    federation commands exist on the wire (`crates/protocol/src/command.rs:996-1064`) with reply
    variants (`crates/protocol/src/envelope.rs:460-490`), and **not one has a handler or a
    `role_permits` arm** — they fall to `_ => false` (`crates/daemon/src/commands.rs:3452`) and are
    role-denied for every client. This is §1 of
    [`00-conventions-and-traps.md`](00-conventions-and-traps.md) happening a fourth time, at
    fourteen commands instead of one. The guard test written to prevent exactly this
    (`commands.rs:6081-6094`) passes, because its `client_issued` list is hand-maintained and no
    federation command was added to it. **The guard only protects what it enumerates — that
    sentence in §1 is load-bearing.**
14. **Three finished crates are orphaned from the binary.** `codypendent-federation`,
    `codypendent-marketplace`, and `codypendent-runner` are workspace members
    (`Cargo.toml:19,25-27`) that **no other crate depends on** — neither `crates/daemon/Cargo.toml`
    nor `crates/codypendentd/Cargo.toml` lists any of them. They compile and their tests pass, so
    every gate is green while none of the code can execute in the product.
15. **The reachable marketplace is not the audited one (M5).** `crates/marketplace/` (~2,900 lines:
    `verify.rs`, `trust.rs`, `distribution.rs`, hostile-archive handling) is orphaned per finding
    14. The path actually on the wire is a second, independent implementation —
    `crates/daemon/src/marketplace.rs` (511 lines) — reached from
    `crates/daemon/src/server.rs:4800`. It performs **no signature verification**: `allow_unsigned`
    is accepted and then ignored (`marketplace.rs:418`, the parameter is `_allow_unsigned`),
    the publisher is taken from the untrusted manifest, its public key is written as 64 zeros
    (`:447-451`), and every version row is stored `signed = 0` (`:494-497`). M5.3's hostile-archive
    tests exercise code no client can call.
16. **The M8/M9 control-plane schema has zero Rust callers.** `crates/control-plane/migrations/`
    `0006_runner_jobs`, `0007_runner_leases`, `0008_runner_attestations`, `0009_runner_pools`, and
    `0010_quality_observations` — 581 lines of SQL — are applied at startup
    (`crates/control-plane/src/store/postgres.rs:26`) and referenced by **no `.rs` file in the
    repository**. There is no scheduler module, and `crates/control-plane/Cargo.toml` does not
    depend on `codypendent-control-plane-protocol`, so the server cannot speak the runner protocol
    it stores tables for.
17. **`crates/runner` has no binary and no transport.** It has no `[[bin]]` and no `src/main.rs`, so
    the "runner agent daemon" cannot be started; and no `reqwest`/`axum`/`tokio-tungstenite`, so
    the only `ControlPlaneClient` implementation is `InMemoryControlPlane`
    (`crates/runner/src/client.rs:57`). The crate doc's claim of "WebSocket/long-polling dispatch"
    (`crates/runner/src/lib.rs:5`) is false. It also defines its own duplicate job types
    (`crates/runner/src/types.rs`) rather than the control-plane protocol's.
18. **The control plane cannot mint its first user, so nobody can ever pair (M7).**
    `POST /v1/auth/login` (`crates/control-plane/src/http.rs:35`) returns `501 not_implemented`
    unconditionally and touches no store (`crates/control-plane/src/routes/auth.rs:32-43`).
    `create_user_token` is reachable only from `refresh` (`routes/auth.rs:90`), which needs a
    refresh-token row that no HTTP path can create; `Store::create_user` exists
    (`store/mod.rs:230`) with no route behind it. Since `start_pairing_challenge` requires
    `Principal::User` (`routes/auth.rs:133-140`), a freshly deployed control plane can never issue
    a pairing code. The rest of the auth work is genuinely sound — see finding 19 — this is the one
    missing link that makes all of it unreachable.
19. **The M7 auth defects previously reported are fixed.** Recorded here so nobody re-fixes them:
    the hardcoded default JWT secret is gone and now actively banned by value and by placeholder
    substring (`crates/control-plane/src/config.rs:7-24`, `resolve_jwt_secret` at `:80-105`,
    fail-closed startup at `main.rs:35-39`); daemon JWTs **are** checked against their DB row and
    its `state`/`revoked_at`, with authority read from the row and not the claims
    (`crates/control-plane/src/auth/mod.rs:222-249`, `:281-283`); and `repository_id` from a
    request body **is** validated and org-scoped before use, with cross-tenant reads answered as
    404 (`crates/control-plane/src/routes/sync.rs:60-90`, publication-class intersection at
    `:93-107`).
20. **The daemon's secret backends are process-lifetime fakes, wired in by default (M5).** The
    broker itself is real and reachable — `crates/daemon/src/server.rs:328`, `:647`, handlers at
    `handle_secret_command` (`:4909`), Controller-gated, with a 300s lease TTL and no key material
    crossing the wire (`SecretBind` returns only `lease_id` + `expires_at`). The **backends** are
    the problem. `with_default_backends` (`crates/secrets/src/broker.rs:39-62`) registers
    `ManagedBackend::new([0x42; 32])` and `WorkloadIdentityBackend::new([0x24; 32])` — hardcoded
    constant keys — and that is what `server.rs:647` instantiates. As audited,
    `ManagedBackend`'s "envelope encryption" was a **XOR against a repeating 32-byte pad**
    (`managed.rs:46-57`, where `decrypt` is literally `encrypt`): deterministic, malleable,
    unauthenticated, and broken by any known-plaintext prefix. `VaultBackend` is an in-process
    `HashMap` with no HTTP client, no `VAULT_ADDR`, and an `outage: AtomicBool` that makes "service
    unreachable" a constructor toggle (`vault.rs:14-29`); `KeychainBackend` is an in-process
    `HashMap` whose `supported` flag is **passed in by the caller** (`keychain.rs:13-27`), so
    "platform keychain is unavailable" was an argument rather than a fact. M5.2's adapter contract
    tests pass against all of it — which is the finding: the contract tests cannot tell a backend
    from a mock of itself. *A replacement is in flight at the time of writing (ChaCha20-Poly1305
    envelope with per-record DEKs and fail-closed `NotConfigured` refusals for Vault and Keychain);
    re-verify `managed.rs`, `vault.rs`, and `keychain.rs` before relying on either description.*

21. **M4's at-least-once dispatch authority has no writer (M4).** `0044_automation.sql` defines five
    tables. `automation_bindings` is read and written by `crates/daemon/src/automation.rs`;
    `automation_endpoints` is read by one query (`crates/integrations/src/webhook/store.rs:90`);
    and **`automation_receipts`, `automation_attempts`, and `automation_leases` are referenced by
    no code anywhere in `crates/`** — the exactly-once machinery documented at
    `0044_automation.sql:149-231` is schema and prose only. Relatedly, `next_fire_at` is computed
    and stored (`crates/daemon/src/automation.rs:119`, `:639`, `:867`) and never read back by any
    non-test code, so `idx_automation_bindings_due` (`0044:107`) serves no query. `croner` is a
    declared dependency of `crates/codypendentd` that the crate never imports; it is used only for
    create-time validation in the daemon (`automation.rs:19`, `:112`, `:116`).

Two findings recur across milestones and are worth reading once as a pair: **`RoutingCoordinator::
escalate` and `record_transition` are `#[cfg_attr(not(test), allow(dead_code))]`**
(`crates/codypendentd/src/routing.rs:487`, `:534` — unchanged) because re-driving would emit a
second terminal `RunCompleted`; M8.6 and M9.5 both need that missing runtime seam. And **`sqlx` is
SQLite-only workspace-wide** (`Cargo.toml:147`, no `postgres`) while the control-plane crate opts
into `postgres` locally; Cargo unifies features, so `--all-features` turns it on for
`daemon`/`workflow`/`eval`/`knowledge`/`codypendentd` too. M8 and M9 inherit both.

## Status

The plan's checkboxes were unticked for eight milestones' worth of landed work; they are now
reconciled milestone by milestone, and the plan carries a status table with the reachability
verdict for each. **Treat checkboxes as evidence, not authority: grep for the symbol before you
build it — and then check that something calls it.** The dominant failure mode in this wave was
not missing code. It was finished, tested code with no caller.
