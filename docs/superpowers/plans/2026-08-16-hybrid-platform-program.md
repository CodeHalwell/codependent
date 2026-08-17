# Hybrid Codypendent Platform Program Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development for one milestone at a time. Do not start a later milestone until the preceding exit gate passes. Mark steps complete in this file as evidence lands.

**Goal:** Turn Codypendent into a complete local-first product and optional hybrid team platform, with real clients, governed automation and sharing, and one secure remote-runner protocol.

**Architecture:** Preserve the local daemon as authority for source, private history, artifacts, secrets, and local effects. Add domain capabilities vertically to the existing protocol/daemon/clients, then introduce a separate network protocol and a single PostgreSQL/S3 control-plane implementation for both managed and self-hosted modes. Remote execution reuses the existing workflow, runtime, sandbox, routing, and evaluation engines.

**Tech stack:** Rust 2021 workspace; Tokio; SQLx with SQLite locally and PostgreSQL in the control plane; Axum; S3-compatible object storage; React/TypeScript/Vite; Tauri desktop shell; VS Code API; Docker/Kubernetes; Ed25519; JSON Schema and golden protocol vectors.

**Spec:** `docs/superpowers/specs/2026-08-16-hybrid-platform-program-design.md`

**Implementation guides:** `docs/superpowers/implementation/` — read
[`00-conventions-and-traps.md`](../implementation/00-conventions-and-traps.md) **before your first
milestone**, then the guide for the milestone you are starting. The guides carry the column-level
data models, the reserved-contract → implementation mapping, and the repository-specific traps that
this plan does not.

## Status — re-verified against the tree on 2026-08-17

An M2–M9 implementation wave has landed a very large amount of code. The table below is the result
of grepping for every symbol before ticking anything, and it separates two things the previous
version of this table conflated:

- **Built** — the code exists and its own tests pass.
- **Reachable** — a client, CLI, or background task in a shipped binary can actually cause it to
  run.

**The dominant failure mode in this wave was not missing code. It was finished, tested code with no
caller.** Three whole crates (`codypendent-federation`, `codypendent-marketplace`,
`codypendent-runner`) are workspace members that no other crate depends on; five PostgreSQL
migrations have no Rust reference at all. Every gate is green over all of it.

| Milestone | Built | Reachable | Verdict |
|---|---|---|---|
| **M0** stabilize | ✅ | ✅ | **complete** — `ReadArtifact` role-permitted, `extensions/vscode/src/patch-review.ts`, honest desktop state |
| **M1** contracts + generated SDK | ✅ | ✅ | **complete** — `crates/protocol/src/bin/export_schema.rs`, `sdk/protocol/{schema,scripts/generate.mjs,src/generated}`, `migrations/0040`, `checksums.json` |
| **M2** session library + clients | ✅ | 🟡 mostly | **2.1, 2.3, 2.6 clean; 2.4, 2.5 substantially done.** Library, editor actions, Tauri desktop and the shared TS client are real and driven. **2.7 bundles are daemon-complete with no CLI and no UI caller**; 2.2 never marks council sessions internal; 2.8 cannot pass as written |
| **M3** inbox + analytics | ✅ | 🟡 mostly | **3.1 and 3.3 clean; 3.4 partial.** Inbox and observations have real producers and real readers. **Budget alerts can never fire** (no caller, no way to create a budget); no quality surface; 3.2 raises no OS-native notification |
| **M4** automation | 🟡 | ❌ | **4.5 clean; 4.1 CRUD only; 4.2–4.4 absent.** Bindings persist and compute `next_fire_at`; the template catalogue is complete and compiled in. **Nothing fires a binding**: no scheduler tick, no production `WebhookEventSink`, no receipt/attempt/lease code |
| **M5** secrets + marketplace | ✅ | 🟡 partly | **Broker reachable, backends are fakes; marketplace reachable via a second unaudited implementation.** All three M5 refusals are correctly gone. See the warnings below |
| **M6** federation | ✅ | ❌ | **Fully built, zero handlers.** All 14 commands exist on the wire and fall through `role_permits`' `_ => false` to a role denial. Being wired now — verify before assuming |
| **M7** control plane | ✅ | ❌ | **Builds, tests, and cannot be reached.** Auth is genuinely sound now, but `POST /v1/auth/login` is a `501` and no route can mint a first user, so no daemon can ever pair. Not in the release binaries |
| **M8** self-hosted runners | 🟡 | ❌ | **Library only.** `crates/runner` has no binary and no network client; the control plane has no scheduler; `remote_node_executor.rs` does not exist; no Kubernetes controller, no container images |
| **M9** managed execution + quality | ❌ | ❌ | **Not started, and the promotion pipeline is now bricked.** No `runner-provider`, no quality modules. `ObserveCanary` is refused without its measured replacement, so no candidate can reach `Promoted` |

### Read these before trusting a tick

- **M5 secrets.** The broker is real and wired (`crates/daemon/src/server.rs:328`, `:647`), and the
  three refusals M5 existed to remove are gone. But `with_default_backends`
  (`crates/secrets/src/broker.rs:39-62`) installs hardcoded constant keys, `ManagedBackend`'s
  "envelope encryption" is a **XOR against a repeating 32-byte pad**
  (`crates/secrets/src/backends/managed.rs:46-57`), and Vault and Keychain are in-process
  `HashMap`s with no external client (`vault.rs:14-29`, `keychain.rs:13-27`). Treat M5.2 as an
  interface milestone, not a security one.
- **M5 marketplace.** The commands work, but they reach
  `crates/daemon/src/marketplace.rs` — a second implementation that does **no signature
  verification** (`allow_unsigned` is accepted and ignored at `:418`; publisher keys are written as
  64 zeros at `:447-451`). `crates/marketplace/`, which has the verification, trust, revocation and
  hostile-archive code, is orphaned.
- **M9 canary.** Caller-supplied `CanaryMetrics` are now **refused**
  (`crates/codypendentd/src/promotion.rs:264-270`) without the server-measured replacement being
  built. `MIN_CANARY_SAMPLES = 100` is no longer satisfied by typing `500` — it is no longer
  satisfiable at all, because nothing increments the counter. That is a dead end, not a fix.
- **Schema landed ≠ feature landed.** Every migration the plan claimed now exists —
  `0041`–`0049` (SQLite) and `0001`–`0010` (PostgreSQL). `0006`–`0010` have **no Rust caller
  anywhere in the repository**, and `0047`/`0048` are touched only by the orphaned federation crate.
- **The `role_permits` catch-all struck again.** See §1 of
  [`../implementation/00-conventions-and-traps.md`](../implementation/00-conventions-and-traps.md).
  It has now claimed fourteen commands at once (M6). The guard test that exists to prevent it
  passed, because its list is hand-maintained and nobody added them.

Milestone-by-milestone evidence is in
[`../implementation/README.md`](../implementation/README.md) under "Where the plan contradicts
shipped code", which carries the file:line references and a verdict for each of the original twelve
findings plus eight found by this re-verification.

## Program rules

- Execute milestones 0–9 in order. Parallelize only disjoint tasks inside the current milestone.
- Write a failing test first, run it and observe the expected failure, implement the smallest behavior, rerun focused tests, then refactor.
- Every feature task must name and exercise a production caller. A domain object with unit tests is not feature-complete.
- Root `migrations/` is append-only SQLite. Use the fixed sequence in this plan; never edit, delete, or renumber historical migrations.
- Control-plane PostgreSQL migrations live only in `crates/control-plane/migrations/`.
- Generate TypeScript wire contracts from Rust schemas. Never add another hand-maintained protocol mirror.
- Derive local identity from the authenticated connection principal and remote identity from validated workload/human credentials. Never trust owner, organization, repository, policy, budget, or approval authority from an untrusted payload.
- Unauthorized and absent repository-owned resources must be indistinguishable, including counts, traversal, pagination, and error bodies.
- Unknown usage and quality measurements stay absent; never convert missing measurements to zero.
- Cloud grants can narrow but never broaden organization, repository, runner, or local deny-first policy.
- Keep local-only startup and operation working with no control-plane configuration or network.
- Preserve unrelated staged and unstaged work. Before every commit inspect `git diff`, `git diff --cached`, and `git status --short`.
- Use conventional, reviewable commits after each task. Never push, merge, tag, publish, or deploy without separate explicit approval.

## Dependency and migration map

```text
M0 stabilization
  └─ M1 generated local contracts + 0040_session_library.sql
       └─ M2 real local product + 0041_session_bundles.sql
            ├─ M3 inbox/analytics + 0042_inbox.sql + 0043_execution_observations.sql
            │    └─ M4 automation + 0044_automation.sql
            │         └─ M5 secrets/marketplace + 0045_secret_broker.sql + 0046_marketplace.sql
            │              └─ M6 federation + 0047_graph_publication.sql + 0048_multi_repo_campaigns.sql
            │                   └─ M7 control plane + 0049_control_plane_sync.sql
            │                        └─ M8 self-hosted runners
            │                             └─ M9 managed execution + continuous quality
            └─ Desktop and VS Code remain usable locally throughout
```

Control-plane migrations start independently at `0001` and remain forward-only.

## Baseline and common verification

Before implementation, capture results without changing gates:

```bash
git status --short --branch
git diff --check
git diff --cached --check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --no-run
cargo test --workspace --all-features
python3 .github/scripts/check_doc_test_counts.py --skip-vitest
python3 .github/scripts/check_docs_manifest.py
cargo deny check
```

Record pre-existing failures in the Milestone 0 evidence; do not hide or waive them. Every milestone exit reruns the full applicable set plus its focused commands.

---

## Milestone 0 — Stabilize the current tree

### Task 0.1: Reconcile the v0.9 worktree into reviewable ownership groups

**Files:** Existing staged/unstaged files reported by `git status`; `improvement_plan/{README,checklist,findings-register,scan-verification,ratatui-styling-review}.md`; `docs/docs/adoptions/22-megaplan.md`; root `MEGAPLAN.md`; `Cargo.lock`; `docs/MANIFEST.json`.

- [x] Save `git status --short`, staged/unstaged stats, and both forms of `git diff --check` as baseline evidence.
- [x] Review mixed files one at a time with `git diff --cached -- <file>` and `git diff -- <file>`; classify intent without resetting either side.
- [x] Fix the known staged trailing whitespace in `improvement_plan/ratatui-styling-review.md`; run both diff checks and expect no whitespace errors.
- [x] Compare root `MEGAPLAN.md`, `300726.txt`, and `docs/docs/adoptions/22-megaplan.md`; restore root content only from the attributable complete document and keep unrelated local files untracked.
- [x] Re-run every finding in `improvement_plan/findings-register.md` against current symbols; update status only with command/test evidence.
- [x] Regenerate `Cargo.lock` only after the current dependency edits are understood; do not use it to erase unrelated staged lock changes.
- [x] Commit coherent existing v0.9 groups separately; example: `docs(review): reconcile verified v0.9 findings`.

### Task 0.2: Verify previously reported correctness fixes

**Files:** `crates/runtime/src/tools/edit_match.rs`; `crates/daemon/src/db.rs`; `crates/knowledge/src/lsp/{mod,servers,client,transport}.rs`; their existing unit/integration tests.

- [x] Add or confirm regression tests for Unicode edit matching, private SQLite file permissions on creation/reopen, and one Python LSP owner per workspace.
- [x] Run each focused test before changing code; if it already passes, record verification and make no implementation edit.
- [x] If a test fails, make the smallest source-of-truth fix and rerun it.
- [x] Run `cargo test -p codypendent-runtime edit_match`, `cargo test -p codypendent-daemon db`, and `cargo test -p codypendent-knowledge lsp`.
- [x] Commit only actual fixes: `fix(runtime): preserve unicode edit matching invariants`, etc.

### Task 0.3: Replace simulated desktop behavior with an honest disconnected product state

**Files:** Modify `apps/desktop/src/{App,types}.tsx`; `apps/desktop/src/components/{Navigation,Composer,Transcript}.tsx`; `apps/desktop/package.json`. Add `apps/desktop/test/App.test.tsx` and test setup.

- [x] Add a failing component test proving no fake sessions appear, no timer produces a completion, and the UI says the daemon is disconnected when no transport is configured.
- [x] Run `npm --prefix apps/desktop test -- App.test.tsx`; expect failure because no test script/state exists.
- [x] Add Vitest/testing-library using the repository's React test conventions; remove hard-coded sessions, `setTimeout` completion, and unconditional connected state.
- [x] Disable run controls while disconnected and provide an actionable daemon-discovery message.
- [x] Run `npm --prefix apps/desktop run check`, tests, and build; expect all pass.
- [x] Commit: `fix(desktop): remove simulated daemon state`.

### Task 0.4: Add real bounded artifact retrieval and VS Code patch review

**Files:** Modify `crates/protocol/src/{command,envelope,version}.rs`; `crates/daemon/src/{artifacts,server}.rs`; `crates/protocol/tests/golden_vectors.rs`; `crates/daemon/tests/server_it.rs`; `protocol-vectors/*.json`; `extensions/vscode/src/{client,extension}.ts`; `extensions/vscode/test/{client,patch-review}.test.ts`.

- [x] Add failing Rust round-trip and server tests for `ReadArtifact { artifact_id, offset, limit, expected_sha256 }` and bounded `ArtifactChunk` replies: first/middle/final chunk, limit clamp, unknown/unauthorized ID, and hash mismatch.
- [x] Run focused protocol/server tests; expect compile failures for missing variants.
- [x] Add the additive protocol variants, bump `PROTOCOL_V1.minor`, and range-read through `ArtifactStore::open`, stored ownership/classification, and a limit safely below `MAX_FRAME_BYTES`.
- [x] Add failing VS Code tests for chunk assembly, hash verification, malformed patch, and multi-file selection.
- [x] Implement request/reply correlation in the existing client, assemble verified bytes, parse unified patches, create read-only before/after URI documents, and call `vscode.diff`; retain `MAX_DIFF_ENTRIES = 64`.
- [x] Remove the metadata-only placeholder. Unauthorized and absent artifacts must show the same public error.
- [x] Run `cargo test -p codypendent-protocol`, `cargo test -p codypendent-daemon --test server_it artifact`, and the extension typecheck/test/build gates.
- [x] Commit: `feat(vscode): review verified patch artifacts`.

### Task 0.5: Finish provider transport and credential wiring already advertised by the catalog

**Files:** Modify `crates/providers/src/{model,credential,lib}.rs`; `crates/runtime/src/models.rs` and its provider-feature tests; `crates/providers/builtin_catalog.toml`. Add `crates/providers/tests/native_transport_it.rs` if the mock-server cases cannot remain beside the current runtime model tests.

- [x] Add failing mock-server tests for native Anthropic Messages and Gemini `generateContent` request/stream/error normalization; verify required headers and bounded error snippets.
- [x] Implement native transports through the existing model-driver seam rather than mapping them to OpenAI-compatible requests.
- [x] Add failing credential tests for supported IAM/OAuth flows, token expiry/refresh, redacted debug output, and explicit unsupported configuration.
- [x] Implement `CloudIamCredential` and `OAuthCredential` through injected token-provider traits; do not introduce an interactive browser flow into the daemon.
- [x] Ensure catalog entries are marked runnable only when transport and credential methods are executable.
- [x] Run provider/runtime tests and no-secret log scans.
- [x] Commit: `feat(providers): wire native transports and delegated credentials`.

### Task 0.6: Close internal council sessions

**Files:** Modify `crates/council/src/service.rs`; `crates/codypendentd/src/{executor,workflow_exec}.rs`; add the minimal additive `CloseSession` contract/handler in `crates/protocol/src/{command,envelope}.rs` and `crates/daemon/src/server.rs`; council, protocol, and daemon tests.

- [x] Add a failing integration test proving member sessions become closed after the parent council reaches a terminal state while events and attribution remain readable. Milestones 1–2 add the explicit internal/parent metadata and automatic archival.
- [x] Add a principal-owned, idempotent `CloseSession` command and lifecycle reply without pre-empting the broader lifecycle/search contract in Milestone 1.
- [x] Add an explicit close callback to the council service seam; invoke it on success, failure, and cancellation.
- [x] Remove the `TODO(protocol)` only after the production path drives the operation.
- [x] Run `cargo test -p codypendent-council` and focused codypendentd recovery tests.
- [x] Commit: `fix(council): close internal member sessions`.

### Task 0.7: Milestone 0 exit gate

- [x] Run all common verification commands plus every npm check/audit for `sdk/protocol`, `sdk/remote-ui`, `sdk/ui`, `extensions/vscode`, and `apps/desktop`.
- [x] Confirm `.github/workflows/ci.yml` executes desktop and SDK gates; add jobs if absent rather than relying on local-only evidence.
- [x] Run `evals/ci/run_gate.sh` and sandbox adversarial tests on supported platforms.
- [x] Confirm root megaplan is complete, docs manifest passes, and no product documentation calls simulated behavior complete.
- [x] Commit gate/config fixes: `ci: gate desktop and generated sdk surfaces`.

---

## Milestone 1 — Shared local contracts and generated SDK

### Task 1.1: Export authoritative Rust protocol schemas

**Files:** Modify schema-reachable types in `crates/protocol/src/{artifact,catchup,command,envelope,events,handshake,ide,input,run,version}.rs` and `crates/protocol/Cargo.toml`. Add `crates/protocol/src/bin/export_schema.rs`; `sdk/protocol/schema/*.schema.json`; `.github/scripts/check_generated_protocol.sh`.

- [x] Add failing schema tests requiring roots for `Command`, `Envelope`, `Payload`, `SessionEvent`, catch-up, artifact, and IDE types.
- [x] Add `JsonSchema` derives and explicit schema roots without changing serialized forms.
- [x] Implement deterministic schema export with sorted/stable output.
- [x] Run exporter twice and assert byte-identical output.
- [x] Commit: `build(protocol): export authoritative json schemas`.

### Task 1.2: Generate the TypeScript SDK and preserve golden compatibility

**Files:** Replace handwritten generated portions of `sdk/protocol/src/{commands,events,ids}.ts`; add generated envelope/payload modules, `sdk/protocol/scripts/generate.mjs`, tests, and exports; modify protocol vector tests and CI.

- [x] Add a failing drift check that regenerates to a temp directory and compares committed output.
- [x] Select one deterministic JSON-Schema-to-TypeScript generator, pin it in `sdk/protocol/package-lock.json`, and wrap only naming/order fixes in `generate.mjs`.
- [x] Keep handwritten framing/client helpers separate from generated files.
- [x] Reconstruct all Rust golden vectors in TypeScript and unknown additive fields safely.
- [x] Run Rust golden tests, `npm --prefix sdk/protocol run check`, and the drift script.
- [x] Commit: `build(protocol): generate typescript sdk from rust schemas`.

### Task 1.3: Define session lifecycle, search, history, editor, inbox, analytics, automation, and bundle contracts

**Files:** Add `crates/protocol/src/{session,inbox,analytics,automation,bundle}.rs`; modify `lib.rs`, `command.rs`, `envelope.rs`, `capabilities.rs`, and vectors.

- [x] Write serialization/golden tests for opaque cursor pages; search filters/results/deep links; rename/pin/archive/restore/delete/export; editor actions with context and idempotency; inbox page/mutation; usage query/export; trigger/schedule CRUD; bundle export/import.
- [x] Make all new fields additive/defaulted and all mutations idempotency-keyed.
- [x] Extend `SessionSummary` with internal, pin/archive, repository/workspace, activity, and run-state fields without breaking old vectors.
- [x] Reserve payloads and capability bits even when implementations land in later milestones; return explicit unsupported errors until then.
- [x] Regenerate Rust schemas and TypeScript SDK; run cross-language vectors.
- [x] Commit: `feat(protocol): define local platform capability contracts`.

### Task 1.4: Add append-only session-library storage

**Files:** Add `migrations/0040_session_library.sql`; `.github/scripts/check_migration_immutability.py`; `migrations/checksums.json`; migration tests in `crates/daemon/src/db.rs` or `crates/daemon/tests/persistence.rs`.

- [x] Add a failing upgrade test from a v0.9 fixture and a failing immutability test for changed/deleted historical SQL.
- [x] Add lifecycle/internal/parent metadata and stable search-index source bookkeeping. Use FTS5 only after a runtime capability test; otherwise use the existing Tantivy dependency in Milestone 2.
- [x] Generate checksums for all historical migrations and teach CI to reject drift while allowing appended files.
- [x] Verify migrate-up, reopen, and rollback policy (forward repair, no destructive down migration).
- [x] Commit: `feat(daemon): add session library persistence`.

### Task 1.5: Milestone 1 compatibility gate

- [x] Run protocol all-features tests, schema fixtures, golden vectors, generator drift, TypeScript SDK checks, migration immutability, and upgrade from previous release.
- [x] Launch an old supported local client fixture against the new daemon and the new client against a previous-release daemon; assert negotiated degradation rather than disconnect for additive features.
- [x] Commit compatibility fixtures: `test(protocol): gate previous local client compatibility`.

---

## Milestone 2 — Session Library, bundles, and real local clients

### Task 2.1: Build the ranked Session Library index and query service

**Files:** Add `crates/daemon/src/session_library.rs`; `crates/daemon/tests/session_library_it.rs`; modify `crates/daemon/src/{lib,server,ledger,projections,commands}.rs`.

- [x] Add failing tests for title/transcript/tool/patch/artifact/path/symbol hits; repository/model/date/status filters; ranking; stable cursor paging; owner isolation; source/scope/provenance/deep-link fields.
- [x] Index durable events incrementally after ledger append and rebuild deterministically after interruption.
- [x] Implement lifecycle writes through the existing durable command/idempotency path, including retention-aware delete/tombstone behavior.
- [x] Wire command handlers using transport-derived principal filters before search/query.
- [x] Run focused session library, replay, persistence, and multi-user tests.
- [x] Commit: `feat(daemon): add searchable session library`.

**Verified 2026-08-17 — done and reachable.** `crates/daemon/src/session_library.rs` (1189 lines):
`search_sessions:111`, incremental indexing `:204`/`:231`/`:246`, deterministic rebuild
`rebuild_search_sources:266` called at startup (`crates/daemon/src/server.rs:570`). `SearchSessions`
is handled at `server.rs:3071`; all six lifecycle actions are implemented in `apply_session_lifecycle`
(`crates/daemon/src/commands.rs:869-1037`) with a fail-closed default, including a 30-day tombstone
purge (`:1001-1020`). **Only VS Code drives it** (`extensions/vscode/src/extension.ts:310,333,362`);
CLI, TUI, and desktop expose neither search nor lifecycle.

### Task 2.2: Archive internal sessions after parent completion

**Files:** Modify `crates/council/src/service.rs`; `crates/codypendentd/src/{workflow_exec,executor}.rs`; session creation/projection code and tests.

- [x] Extend the Milestone 0 terminal-state test to success/failure/cancel/recovery and parent-child attribution.
- [ ] Mark internal sessions at creation and archive only after parent terminal persistence succeeds.
- [x] Ensure active-history queries omit archived internal sessions by default while explicit search can include them.
- [ ] Commit: `feat(session): archive completed internal work`.

**Verified 2026-08-17 — half done. The council path is missing entirely.** The workflow path is
real: `crates/codypendentd/src/workflow_exec.rs:1216` sets `internal = 1` with `parent_run_id` and
`parent_session_id`, and `archive_internal_sessions`
(`crates/codypendentd/src/workflows.rs:402-420`) guards on the parent workflow run being terminal
before archiving, called from five sites. The default query filter is in place
(`session_library.rs:581`, `s.internal = 0 AND s.archived_at IS NULL`). **But
`crates/council/src/service.rs` passes `internal: false` at all four `CreateSession` sites
(`:1311`, `:2327`, `:2424`, `:2456`) and contains no archival at all** — which is the exact case
Task 0.6 opened and this task was written to close. Council member sessions still pollute the
library. `crates/codypendentd/src/executor.rs` has no internal/archive handling either.

### Task 2.3: Build a shared daemon TypeScript transport

**Files:** Add `sdk/protocol/src/{framing,client,session-store}.ts`; tests. Refactor `extensions/vscode/src/{client,protocol/frame}.ts` to consume it.

- [x] Add failing tests for fragmented frames, handshake/attach/catch-up/live ordering, request correlation, ping/pong, resume token, sequence dedup, bounded offline queue, reconnect backoff, and cancellation.
- [x] Move behavior from VS Code into `@codypendent/protocol` without changing wire semantics.
- [x] Keep host discovery/socket bridging injected so Node, Tauri, and tests can supply transports.
- [x] Run shared SDK and extension tests before deleting duplicate client code.
- [x] Commit: `refactor(protocol): share daemon client transport`.

**Verified 2026-08-17 — done, with one duplicate left.** All three modules exist:
`sdk/protocol/src/framing.ts`, `client.ts` (1091 lines), `session-store.ts`, with
`test/{framing,client,session-store}.test.ts`. `extensions/vscode/src/protocol/frame.ts` **is
deleted**; `extensions/vscode/src/client.ts:14-46` now extends `BaseDaemonClient` from
`@codypendent/protocol` and re-exports the codec from it. Desktop consumes the same package
(`apps/desktop/src/{transport,daemonState,useDaemon}.ts`). **Remaining drift:**
`extensions/vscode/src/protocol/types.ts` is still a hand-written 809-line mirror of the Rust wire
types, imported by `extension.ts`, `editor-actions.ts`, and `inbox.ts` alongside the generated
package — two sources of truth, which is the thing M1.2 was meant to end.

### Task 2.4: Implement the Tauri host and real desktop projection

**Files:** Add `apps/desktop/src-tauri/{Cargo.toml,tauri.conf.json,src/main.rs}`; `apps/desktop/src/daemon/{transport,projection,commands}.ts`; `apps/desktop/src/hooks/useDaemonSession.ts`; tests. Modify desktop package, app, types, and components.

- [x] Add failing transport/projection tests for discovery, connect, create, attach, full paginated catch-up, live overlap dedup, start/cancel, approvals, questions, and artifact reads.
- [x] Use Tauri only for native daemon discovery/socket and notifications; keep protocol state in the shared TypeScript client.
- [x] Upgrade desktop to the React version required by `@codypendent/ui` and remove manually mirrored protocol types.
- [x] Add disconnected/reconnecting/offline states and bounded retry controls.
- [ ] Run desktop check/test/build and `tauri build` on macOS CI.
- [x] Commit: `feat(desktop): connect to the local daemon`.

**Verified 2026-08-17 — real, not a stub; notifications are the exception.**
`apps/desktop/src-tauri/src/daemon.rs` (968 lines) opens a genuine Unix socket through
`codypendent_council::connection::Connection` with real `read_envelope`/`write_envelope` and a 30s
command timeout; `bridge.rs:22-31` pushes daemon frames into a Tauri `Channel`. No synthesized
transcript content remains. `transport.ts:100` returns `null` when the Tauri shell is absent, so a
browser-run desktop build reports honest disconnection rather than fake data. Tauri capabilities are
locked to `core:default` only — no fs, shell, or net. **"and notifications" is not done:** there is
no `tauri-plugin-notification` dependency and no notification capability grant, so the desktop
cannot raise an OS notification (see 3.2). `tauri build` on macOS CI is unverified here.

### Task 2.5: Extract one semantic Remote UI React renderer

**Files:** Add `sdk/ui/src/host-react/{renderer,store,capabilities,theme,slot-registry,mediated,index}.tsx` as appropriate and tests. Modify VS Code webview Remote UI files and `apps/desktop/src/components/RemoteUiRenderer.tsx`.

- [x] Port existing VS Code renderer tests to the shared package first; add capability denial, unknown node, slot, event validation, and theme tests.
- [x] Extract host-neutral state/rendering; keep VS Code and desktop messaging as thin adapters.
- [x] Replace desktop metadata display with shared semantic rendering and validated daemon events.
- [x] Run shared UI, VS Code, and desktop suites.
- [x] Commit: `refactor(ui): share semantic remote ui renderer`.

**Verified 2026-08-17 — extracted, but shared by exactly one host.**
`sdk/ui/src/host-react/` exists (`renderer.tsx` 1172 lines, `store.ts`, `capabilities.ts`,
`theme.ts`, `slot-registry.ts`, `mediated.ts`) and is exported as `@codypendent/ui/host-react`.
Desktop consumes it (`apps/desktop/src/components/RemoteUiRenderer.tsx:8`). **VS Code does not** —
`extensions/vscode/src/webview/remote-ui/` still carries its own near-identical and now-diverging
`renderer.tsx` (1085), `store.ts` (396), `capabilities.ts`, `theme.ts`, `slot-registry.ts`,
`mediated.ts`. The duplication this task existed to remove is still there; it just moved hosts.

### Task 2.6: Complete VS Code history and editor-native actions

**Files:** Add `extensions/vscode/src/editor-actions.ts`; tests. Modify `extensions/vscode/{package.json,src/extension.ts}` and panel/client tests.

- [x] Add failing tests that attach loads every paginated event before live projection and deduplicates catch-up overlap.
- [x] Add command/menu tests for Fix selection, Explain selection, Review current file, Generate tests, and Fix diagnostic.
- [x] Register contributions with correct editor/context/diagnostic enablement; send current `IdeContextUpdate`, source identity, and idempotency key into an ordinary daemon run.
- [x] Prove no extension-only model/tool loop exists and all runs are attributable.
- [x] Run extension typecheck/lint/test/build/prepublish.
- [x] Commit: `feat(vscode): add full history and editor actions`.

**Verified 2026-08-17 — done and wired at both ends.** `extensions/vscode/src/editor-actions.ts`
(367 lines) implements all five actions (`:191`, `:207`, `:223`, `:246`, `:262`), each building an
`EditorNativeAction`; registrations at `:296-358` from `extension.ts:28,733`; `package.json`
contributes commands (`:80-104`), an editor context menu (`:113-133`), and palette entries
(`:140-156`). Daemon side is a real run, not an extension-side loop: protocol enum
`crates/protocol/src/session.rs:265-278`, dispatch `crates/daemon/src/server.rs:1054-1081`, write
path `apply_run_editor_action` (`crates/daemon/src/commands.rs:2349-2385`), role floor at `:3402`.
The searchable-history surface is `extensions/vscode/src/webview/session-library-view.tsx`.

### Task 2.7: Add versioned redacted session/support bundles

**Files:** Add `migrations/0041_session_bundles.sql`; `crates/daemon/src/bundles.rs`; `crates/protocol/src/bundle.rs` implementation fields; `crates/daemon/tests/bundles_it.rs`; CLI commands/tests in `crates/cli/src/{main,commands,client}.rs` and `crates/cli/tests/bundle_it.rs`.

- [x] Add failing export tests for explicit inclusion policy, redaction, manifest hashes, transcript/event selection, routing/approval metadata, patch/artifact manifests, and diagnostics.
- [x] Add hostile import tests for hash mismatch, oversized entries, path/symlink escape, duplicate paths, identity collision, credentials, and unsupported versions.
- [x] Implement a deterministic content-addressed archive; imports receive new local IDs plus imported provenance and never restore credentials or approvals.
- [ ] Drive export/import through CLI and one desktop action; verify round trip into a fresh database.
- [ ] Commit: `feat(session): export and import redacted bundles`.

**Verified 2026-08-17 — daemon-complete and unreachable from any client.**
`migrations/0041_session_bundles.sql` and `crates/daemon/src/bundles.rs` (1395 lines) exist and are
fully wired on the daemon side: handler at `crates/daemon/src/server.rs:3451` with a role gate at
`:3440-3446`, idempotent replay responses at `:4286`/`:4324`, write paths at
`crates/daemon/src/commands.rs:2428` (export) and `:2517` (import), owner filtering at
`bundles.rs:374`. **No production caller exists.** There is no `bundle` subcommand in
`crates/cli/src/main.rs`, no desktop action, and no VS Code command; `exportBundle`/`importBundle`
appear in the TypeScript SDK only as generated type literals. This task's fourth bullet — "name and
exercise a production caller" per the program rules — is the one that is not done, and it is the
one that makes the feature exist.

### Task 2.8: Milestone 2 end-to-end gate

- [ ] Start a real daemon; create/run/search/rename/pin/archive/restore/export a session from desktop and attach/read/review its patch from VS Code.
- [ ] Verify CLI/TUI still observe the same durable events and local-only operation works with network disabled.
- [ ] Run all Rust, SDK, UI, desktop, extension, docs, deny, and audit gates.
- [ ] Commit e2e harness: `test(clients): gate shared local session lifecycle`.

**Blocked as written 2026-08-17.** The first bullet cannot pass: search and lifecycle are not
exposed by the desktop client (only VS Code drives them), and export has no client caller at all.
Either close 2.7's caller gap and add the desktop surfaces, or re-scope this gate to the surfaces
that exist and say which client drives each step.

---

## Milestone 3 — Durable inbox and measured analytics

### Task 3.1: Persist the owner-scoped inbox

**Files:** Add `migrations/0042_inbox.sql`; `crates/daemon/src/inbox.rs`; `crates/daemon/tests/inbox_it.rs`; implement `crates/protocol/src/inbox.rs`; modify daemon server/executor and protocol exports.

- [x] Add failing tests for deduplicated upsert, unread/read/dismissed/resolved transitions, cursor paging, repository filters, deep links, owner isolation, and idempotent mutation.
- [x] Persist `inbox_entries` with unique `(owner_uid,dedup_key)` and separate adapter-delivery attempts.
- [ ] Produce entries beside durable approval, question, run terminal, budget, workflow block, plugin permission, and runner failure records; derive owner from source rows.
- [x] Wire list/acknowledge/dismiss handlers and capability negotiation.
- [x] Commit: `feat(inbox): persist pending human work`.

**Verified 2026-08-17 — reachable, with three of seven producers.**
`migrations/0042_inbox.sql` and `crates/daemon/src/inbox.rs` (1064 lines) exist. Handlers are live:
`ListInbox` at `crates/daemon/src/server.rs:3109`, `MutateInbox` at
`crates/daemon/src/commands.rs:3322` with owner scoping at `:3444` and idempotent replay at
`server.rs:4267`. Ownership is gated non-vacuously — see finding 2 in the implementation guides.
**Producers that exist:** run terminal (`crates/daemon/src/ledger.rs:578,593`), agent question
(`crates/daemon/src/questions.rs:182`), approval request
(`crates/daemon/src/approvals.rs:427`). **Producers that do not:** budget (see 3.4), workflow block,
plugin permission, runner failure. `inbox::produce_budget_warning` (`inbox.rs:761`) has **zero
callers anywhere**.

### Task 3.2: Deliver deduplicated native notifications

**Files:** Modify `sdk/ui/src/first-party/system.tsx`; VS Code client/extension tests; desktop app/native wrapper; TUI terminal notifier as applicable.

- [ ] Add failing tests proving each unread inbox ID notifies once across reconnect and acknowledgement never resolves an approval/question.
- [x] Render the daemon-owned inbox in shared UI with deep links and repository context.
- [ ] Add VS Code and Tauri native adapters keyed by durable entry ID; keep email/chat behind disabled policy adapters.
- [ ] Run client suites and a reconnect e2e test.
- [ ] Commit: `feat(clients): notify from the durable inbox`.

**Verified 2026-08-17 — the inbox is rendered; nothing notifies.** `extensions/vscode/src/inbox.ts`
(226 lines) provides a tree view (`extension.ts:125`, contributions `package.json:37,50-65`) and
`apps/desktop/src/components/InboxView.tsx` renders it from `transport.ts:118-119`. Both are
**in-app chrome only**: VS Code uses `showWarningMessage`/`showInformationMessage`
(`inbox.ts:147,164`) and a status-bar item (`:120`); the desktop has no
`tauri-plugin-notification` dependency and no notification capability grant, so it cannot raise an
OS notification at all. `NotificationCenter` in `sdk/ui/src/first-party/system.tsx:175-207` is a
Remote UI document component, not a native adapter. The once-per-entry-across-reconnect property is
therefore untested because there is nothing to test it against.

### Task 3.3: Persist normalized execution observations

**Files:** Add `migrations/0043_execution_observations.sql`; `crates/daemon/src/analytics.rs`; `crates/daemon/tests/analytics_it.rs`; implement `crates/protocol/src/analytics.rs`; modify ledger, executor, workflow execution, routing, and model profiles.

- [x] Add failing tests for nullable input/output/cached/reasoning tokens, cost, latency, provider/model, repository/workflow/task class, route/retry/escalation, completion, and grader score.
- [x] Record measured observations keyed by logical run/attempt while preserving current run-usage compatibility columns.
- [x] Backfill only values present in durable existing records; assert missing fields remain `NULL`.
- [x] Commit: `feat(analytics): record measured execution observations`.

**Verified 2026-08-17 — done, and honest about what it cannot measure.**
`migrations/0043_execution_observations.sql` creates `execution_observations` (`:13`) and
`analytics_budgets` (`:106`); `crates/daemon/src/analytics.rs` (833 lines) records through
`record_observation_in_tx:228`. Real writers on the live path: `crates/daemon/src/ledger.rs:537`
(completion, latency) and `:715` (tokens, cost joined to `model_task_outcomes`), both gated on a
non-null `sessions.owner_uid`. **`cached_tokens` and `reasoning_tokens` are correct and permanently
`NULL`**: `ModelUsage` (`crates/runtime/src/agent.rs:655-668`) has no such fields, which the
migration's own comment records at `:54`. That is §8 measurement honesty working as intended — but
the analytics contract is not fully measurable until the runtime type grows the fields. Dead
helpers to be aware of: `AnalyticsStore::new` and `analytics::backfill` have no production caller;
the daemon calls the free functions directly.

### Task 3.4: Query, budget, alert, and export usage/quality

**Files:** Add `crates/daemon/src/analytics/export.rs`; modify protocol/daemon server; `sdk/ui/src/first-party/intelligence.tsx`; shared/desktop views and tests.

- [x] Add failing aggregate tests by model/provider/repository/workflow/task class/time, including known/unknown sample counts and cost per successful task.
- [x] Add owner/repository-authorized bounded JSON/CSV export tests and formula/escaping snapshots.
- [ ] Implement configurable budget thresholds that create deduplicated inbox alerts from measured values only.
- [x] Drive reports and export from desktop; retain explicit unavailable markers for missing data.
- [ ] Commit: `feat(analytics): add usage and quality center`.

**Verified 2026-08-17 — usage yes, budgets no, quality absent.** `QueryAnalytics`
(`crates/daemon/src/server.rs:3168`) and `ExportAnalytics`
(`crates/daemon/src/commands.rs:2454`, replay at `server.rs:4305`) are both wired, with explicit
`InvalidCursor`/`UnsupportedGrouping`/`UnsupportedFormat` refusals rather than silent coercion.
`apps/desktop/src/components/AnalyticsDashboard.tsx` drives both and marks missing measurements
unavailable rather than zero. **Budget alerts can never fire.** Every piece exists —
`BudgetAlert` (`analytics.rs:83`), `evaluate_budgets` (`:738-820`) with a dedup key (`:816`),
`InboxEntryKind::BudgetWarning` (`inbox.rs:119`), `derive_budget_dedup_key` (`:566`) — and none is
connected: `evaluate_budgets` is called only from `crates/daemon/tests/analytics_it.rs`,
`produce_budget_warning` is called by nothing, and `analytics_budgets` rows are written **only by
tests** (`analytics_it.rs:859,935`) because no command, CLI, or UI can create a budget. There is
also **no quality surface anywhere** — no grader-score view in desktop, VS Code, or shared UI — so
"usage and quality center" is half a title.

### Task 3.5: Milestone 3 exit gate

- [ ] E2E: run measured and unmeasured providers, observe inbox alerts, reconnect clients, query/export analytics, and prove unknowns do not become zero.
- [ ] Run multi-user, replay, migration, client, accessibility, and full common gates.

---

## Milestone 4 — Durable scheduled and event-driven automation

### Task 4.1: Persist trigger bindings, receipts, firings, and attempts

**Files:** Add `migrations/0044_automation.sql`; `crates/daemon/src/automation.rs`; `crates/daemon/tests/automation_it.rs`; implement `crates/protocol/src/automation.rs`.

- [x] Add failing CRUD/role/owner/repository tests for bindings with source config/filter, workflow/version, dedup, concurrency, trigger retry, misfire, budget, approval mode, and enabled state.
- [ ] Add atomic receipt and attempt transitions; keep invocation policy out of `WorkflowDefinition`.
- [x] Wire controller-gated mutation and observer-gated query commands.
- [ ] Commit: `feat(automation): persist trigger bindings and receipts`.

**Verified 2026-08-17 — "bindings" done, "receipts" not started.**
`migrations/0044_automation.sql` (231 lines, five tables), `crates/protocol/src/automation.rs` (381),
`crates/daemon/src/automation.rs` (941), `crates/daemon/tests/automation_it.rs` (849). CRUD is
reachable: `ManageAutomationBinding` is dispatched at `crates/daemon/src/server.rs:2155` with
Controller-gated mutations and open reads (`crates/daemon/src/commands.rs:3419-3425`), owner stamped
from the peer uid, ownership resolved at `server.rs:6055`. **The dispatch authority has no writer:**
`automation_receipts`, `automation_attempts`, and `automation_leases` are referenced by **no code in
`crates/`**, so the at-least-once machinery documented at `0044_automation.sql:149-231` is schema and
prose only. `next_fire_at` is computed and stored (`automation.rs:119,639,867`) and never read back,
leaving `idx_automation_bindings_due` (`0044:107`) serving no query. `crates/daemon/src/automation.rs`
exposes exactly `create/get/list/update/delete_binding` — **there is no fire, receipt, attempt, or
dispatch function in the module at all.**

### Task 4.2: Dispatch verified GitHub webhooks into workflows

**Files:** Add `crates/codypendentd/src/webhook_dispatch.rs`; `crates/codypendentd/tests/webhook_workflow_it.rs`; modify integrations webhook ingest/server and codypendentd assembly/workflows.

- [ ] Add failing e2e tests: valid enabled binding starts once; duplicate GUID returns prior receipt; invalid signature/filter/repository starts none; crash after receipt retries without duplicate effect.
- [ ] Inject a `WebhookEventSink` after verify → normalize → atomic delivery reservation.
- [ ] Resolve target/owner/repository/policy from enabled server-side binding; derive workflow idempotency from binding ID + delivery GUID.
- [ ] Persist dispatch failure/retry state and remove the current `allow_triggers=false` dead end.
- [ ] Commit: `feat(automation): dispatch github webhooks to workflows`.

**Verified 2026-08-17 — the seam exists; nothing is plugged into it.**
`crates/codypendentd/src/webhook_dispatch.rs` does not exist. The `WebhookEventSink` **trait** does
(`crates/integrations/src/webhook/ingest.rs:31-41`, held at `:114`, injected at `:142`), and the
`allow_triggers` boolean is indeed gone from `crates/` — but **no production code constructs a
sink.** The one production construction site is `crates/codypendentd/src/lib.rs:409`
(`WebhookIngestor::new(store, secret, false)`), still carrying the stale comment "Deliveries never
trigger workflows in this phase" at `:408`. Every other caller passes `None` or a test double.
Because a verified delivery reaches no sink, none of the four e2e properties in the first bullet has
anything to hold. **The last bullet's wording is now stale** — the dead end is no longer named
`allow_triggers`; it is the absent sink implementation.

### Task 4.3: Implement schedules and generic trigger semantics

**Files:** Add `crates/codypendentd/src/automation_scheduler.rs`; scheduler tests; modify codypendentd startup/workflows and workspace dependencies for a pinned timezone-aware cron library.

- [ ] Add paused-time tests for cron/one-time occurrences, DST, restart recovery, skip/fire-once/bounded-catch-up misfires, retry, and deterministic next-fire preview.
- [ ] Add concurrent claim tests for allow, skip-while-active, queue, and approved replace behavior.
- [ ] Atomically claim due rows and persist occurrence timestamps before dispatch; use durable binding-level leases, not `DriveLockRegistry`.
- [ ] Keep trigger retry separate from workflow node retry.
- [ ] Commit: `feat(automation): schedule durable workflow invocations`.

**Verified 2026-08-17 — not started.** `crates/codypendentd/src/automation_scheduler.rs` does not
exist and there is no scheduler module anywhere. The pinned timezone-aware cron library was chosen
and added — `croner = "2.1"` (`Cargo.toml:136`, `crates/daemon/Cargo.toml:46`,
`crates/codypendentd/Cargo.toml:45`) — but it is used **only to validate an expression and preview
one next occurrence at binding-create time** (`crates/daemon/src/automation.rs:19,112,116`).
`crates/codypendentd` declares the dependency and never imports it. There is no tick loop, no
due-row claim, and no lease acquisition.

### Task 4.4: Add generic signed webhook and internal event adapters

**Files:** Add `crates/integrations/src/webhook/generic.rs`; tests; modify webhook module/server and dispatch. Add internal event adapter modules under daemon/codypendentd.

- [ ] Add failing signature/replay/body-limit/secret-namespace tests and hostile payload tests proving payload cannot select owner/repository/workflow/budget/approval.
- [ ] Normalize generic webhooks, CI failures, repository/codegraph changes, dependency alerts, and manual/API events into one `TriggerEvent` path.
- [ ] Enforce endpoint-specific secrets, verify-before-parse, bounded errors, and common filter/dedup handling.
- [ ] Commit: `feat(automation): normalize signed and internal triggers`.

**Verified 2026-08-17 — not started.** `crates/integrations/src/webhook/generic.rs` does not exist,
and **`TriggerEvent` does not exist anywhere in `crates/`** (the only similarly-named type,
`ScheduleTriggerEvent` in `crates/control-plane-protocol/src/events.rs:72`, is unrelated).
`crates/integrations/src/webhook/normalize.rs` still exposes only the GitHub-shaped
`NormalizedEvent` (`:15`) and `normalize()` (`:43`); there are no internal event adapters under
either the daemon or codypendentd. The verify-before-parse discipline this task would extend
already exists for GitHub in `webhook/verify.rs` and is worth copying rather than redesigning.

### Task 4.5: Ship first-party workflow templates

**Files:** Add `docs/specs/workflows/{failing-ci-repair,dependency-update,stale-document-refresh,flaky-test-investigation,repository-health-report,release-preparation}.yaml`; modify `crates/workflow/src/source.rs`; `crates/workflow/tests/spec_it.rs`; compatibly retain `docs/specs/workflow.yaml`.

- [x] Add catalogue tests for immutable ID/version, compiler/reference validity, source precedence, approval/budget defaults, and executable-tool allowlist.
- [x] Add each manifest and only production-supported node types.
- [ ] E2E one safe fixture for every template through the trigger service.
- [x] Commit: `feat(workflow): add automation template catalogue`.

**Verified 2026-08-17 — catalogue complete; the e2e bullet is blocked by 4.2/4.3.** All six named
manifests exist under `docs/specs/workflows/` (`failing-ci-repair`, `dependency-update`,
`stale-document-refresh`, `flaky-test-investigation`, `repository-health-report`,
`release-preparation`), plus `pr-review` and `security-scan`. Catalogue wiring is real:
`crates/workflow/src/source.rs:53-71` `include_str!`s each, aggregates into
`BUILTIN_WORKFLOW_MANIFESTS` (`:72-83`), and `WorkflowSources::load` parses them under
`WorkflowScope::BuiltIn` (`:165-173`), with a parse test at `:376-378`. **"Through the trigger
service" cannot be exercised** — there is no trigger service to route them through until 4.2 and
4.3 land.

### Task 4.6: Milestone 4 exit gate

- [ ] Partition/restart/replay tests prove at-least-once delivery never duplicates external effects.
- [ ] UI/desktop can create, inspect, pause, resume, preview, and audit bindings/schedules.
- [ ] Run webhook, workflow, scheduler, migration, authorization, and common gates.

---

## Milestone 5 — Secret broker, marketplace, and integration pack

### Task 5.1: Add the secret-reference and lease domain

**Files:** Add `crates/secrets/{Cargo.toml,src/{lib,reference,lease,backend,audit}.rs,tests/broker_it.rs}`; `migrations/0045_secret_broker.sql`; modify root workspace and provider dependencies.

- [x] Add failing tests for principal/org/repository/job/capability binding, accepted-reference digest, expiry, revocation, idempotent issue, and no secret in Debug/Serialize/audit.
- [x] Implement `SecretReference`, `LeaseContext`, non-clone/non-serialize leased material, `SecretBackend`, and `SecretBroker` traits.
- [x] Persist opaque reference metadata, lease state, and append-only non-secret audit.
- [x] Commit: `feat(secrets): add context-bound secret leases`.

**Verified 2026-08-17 — the domain is real and reachable.** `crates/secrets/src/lib.rs:7-18`
declares `audit, backend, backends, broker, lease, reference`; `migrations/0045_secret_broker.sql`
(115 lines) creates `secret_references` (`:10`), `secret_leases` (`:56`) and `secret_audit` (`:90`).
The broker is instantiated by the daemon (`crates/daemon/src/server.rs:328`, `:647`) and driven by
four wire commands through `handle_secret_command` (`server.rs:4909`), Controller-gated for
mutations. Leases are issued with a 300s TTL (`server.rs:4989-4999`) and **`SecretBind` returns only
`lease_id` and `expires_at` — no key material crosses the wire.** The backends behind this
interface are a separate matter; see 5.2.

### Task 5.2: Implement initial secret backends and policy gate

**Files:** Add `crates/secrets/src/backends/{mod,environment,keychain,managed,vault,workload_identity}.rs`; tests; `crates/daemon/src/secret_gate.rs`; `crates/codypendentd/src/secrets.rs`; modify policy gate, sandbox gate, providers credential, GitHub/search secret discovery.

- [ ] Add adapter contract tests for resolve-at-use environment refs, platform keychain/unsupported behavior, envelope-encrypted managed values, Vault lease TTL/revoke, and audience-bound workload tokens.
- [ ] Authorize `HostRequest::ReadSecret` only after manifest ceiling and run policy; resolve material at final transport injection.
- [ ] Replace integration-specific plaintext discovery with compatibility references such as `env:GITHUB_TOKEN`.
- [ ] Add compromised-job tests proving no post-acceptance scope widening.
- [ ] Commit: `feat(secrets): broker local and managed credential backends`.

**Verified 2026-08-17 — the three refusals M5 existed to remove are gone, and none of the five
backends is a real backend. Do not tick this on the strength of the refusals disappearing.**

What genuinely changed: `policy.no-secret-broker` no longer exists in `crates/`;
`ManifestError::UnenforceableCapability` is no longer raised for `permissions.secrets`
(`crates/knowledge/src/manifest.rs` now raises it only for `permissions.network` at `:444` and
`trust.signature_required` at `:459`), so a skill manifest can finally declare a secret and it
lowers to `CapabilityRequest::Secret` (`:576-581`); and the guest-callable host function exists
(`host_read_secret`, `crates/sandbox/src/wasm.rs:517`, linked at `:702`).

What that does **not** mean:

- **The removed policy refusal was replaced by an unconditional allow.** `eval_read_secret`
  (`crates/daemon/src/policy/mod.rs:541-552`) returns `Decision::Allow` with reason
  `policy.secret-brokered` for any secret name, with no capability grant and no approval
  (lowered at `crates/daemon/src/policy_gate.rs:112-114`). A gate that failed closed now always
  passes. **This is the bullet about authorizing "only after manifest ceiling and run policy", and
  it is inverted.**
- **`host_read_secret` returns the secret's name, not its value.** At `wasm.rs:562` it writes
  `requested.as_bytes()` — the name the guest passed in — back into guest memory, and never calls
  `crates/secrets/`. It is an echo behind an authorization check.
- **Nothing can reach it anyway.** `WasmHost` (`wasm.rs:608`) has one caller outside its crate,
  `crates/knowledge/src/skill_exec.rs:583`, which neither the daemon nor codypendentd invokes; and
  the only guest profile the daemon builds hardcodes `brokered_secrets: Vec::new()`
  (`crates/daemon/src/commands.rs:3682`, `crates/daemon/src/hook_exec.rs:165`), so
  `CapabilityBroker` would deny every request regardless (`crates/sandbox/src/gate.rs:308`).
- **The backends are process-lifetime fakes with hardcoded keys.** `with_default_backends`
  (`crates/secrets/src/broker.rs:39-62`) registers `ManagedBackend::new([0x42; 32])` and
  `WorkloadIdentityBackend::new([0x24; 32])`, and that is what `server.rs:647` installs. As
  audited, `ManagedBackend`'s "envelope encryption" was a **XOR against a repeating 32-byte pad**
  (`managed.rs:46-57`; `decrypt` is literally `encrypt`) — deterministic, malleable and
  unauthenticated. `VaultBackend` is an in-process `HashMap` with no HTTP client and an
  `outage: AtomicBool` toggle (`vault.rs:14-29`); `KeychainBackend` is an in-process `HashMap`
  whose `supported` flag is supplied by the caller (`keychain.rs:13-27`). The adapter contract
  tests in this bullet pass against all of it, which is the real finding: **they cannot distinguish
  a backend from a mock of itself.** *A replacement is in flight (ChaCha20-Poly1305 envelope with
  per-record DEKs; fail-closed `NotConfigured` for Vault and Keychain) — re-verify before relying
  on either description.*
- **`crates/daemon/src/secret_gate.rs` and `crates/codypendentd/src/secrets.rs` do not exist**; the
  logic the plan puts there lives inline in `server.rs:4909`. Update the file list or move it.
- Compatibility references such as `env:GITHUB_TOKEN` have not replaced the existing
  integration-specific plaintext discovery.

### Task 5.3: Add durable marketplace distribution and compatibility

**Files:** Add `crates/marketplace/{Cargo.toml,src/{lib,catalog,distribution,store,compatibility}.rs,tests/distribution_it.rs}`; `migrations/0046_marketplace.sql`; workspace wiring.

- [x] Add hostile archive/download tests: size/count/ratio, absolute/parent paths, escaping links, duplicate normalized paths, unexpected executable files, hash/signature mismatch, redirects, and source allowlist.
- [x] Reuse sandbox manifest/signature/lifecycle and generic safe extraction patterns; persist immutable package/version/hash, publisher, artifact, install, pin, lifecycle, and permission receipt records.
- [x] Install content-addressed packages disabled; host computes compatibility.
- [x] Commit: `feat(marketplace): verify and persist package distributions`.

**Verified 2026-08-17 — built in full, and not the code the daemon runs. See 5.4.**
`crates/marketplace/` exists with all nine modules (`catalog, compatibility, distribution, error,
lifecycle, permission, store, trust, verify` — `lib.rs:14-22`, including `TrustManager` and
`PackageVerifier` at `:35-36`), ~2,900 lines, and `migrations/0046_marketplace.sql` (167 lines)
creates all six tables. **The crate is a workspace member that no other crate depends on** — grep
every `Cargo.toml` and the only hit is its own. All of the hostile-archive and signature work in
this task is unreachable.

### Task 5.4: Add trust, revocation, updates, protocol, CLI, and UI

**Files:** Add marketplace trust/revocation/update modules/tests; daemon/codypendentd operations; `crates/protocol/src/marketplace.rs`; vectors; CLI commands/tests; desktop shared UI.

- [ ] Add failing tests for discover/inspect/install/pin/check/approve/disable/enable/remove/trust/revocation, permission expansion receipts, org allowlists, and hidden-package non-disclosure.
- [ ] Make publisher/registry trust distinct; revocation disables installed/cached packages and invalidates pending receipts.
- [ ] Keep sandbox `InstalledPlugin` lifecycle as final execution authority.
- [ ] Drive the complete lifecycle from CLI and desktop.
- [ ] Commit: `feat(marketplace): add governed package lifecycle`.

**Verified 2026-08-17 — a lifecycle is drivable from the CLI, and it is not the governed one.**
Six commands are wired end to end: `crates/cli/src/main.rs:288-360` → dispatch at `:1799-1840` →
`crates/cli/src/commands.rs:3264-3447` → daemon `handle_marketplace_command`
(`crates/daemon/src/server.rs:4783`, Controller-gated at `:4795`). But the handler calls
`crates/daemon/src/marketplace.rs` — a **second, 511-line reimplementation** — not the audited
crate, and it drops every governance property this task lists:

- **No signature verification.** `allow_unsigned` is accepted from the wire and then ignored: the
  parameter is `_allow_unsigned` (`crates/daemon/src/marketplace.rs:418`), every version row is
  written `signed = 0` (`:494-497`), and `PackageVerifier` is never invoked.
- **No trust model.** The publisher is read from the untrusted manifest and inserted with a
  64-zero public key at `trust_tier = 'untrusted'` (`:447-451`). `TrustManager` is never consulted,
  and publisher trust is not distinct from registry trust because neither exists on this path.
- **No permission receipts.** `marketplace_permission_receipts` is written only by the dead crate
  (`crates/marketplace/src/store.rs:946,981,1020,1061`).
- Revocation is a flat `INSERT OR IGNORE` plus a lifecycle `UPDATE` (`daemon/src/marketplace.rs:386`,
  `:397`) which does block a later enable (`:303-306`), but invalidates no pending receipt.
- `crates/protocol/src/marketplace.rs` is **24 lines** — one `MarketplacePackageView` struct is the
  entire wire contract, which is why none of the governance state can be expressed to a client.

There is also no desktop surface. Before ticking anything here, decide which implementation is the
product: keeping both is how the verified one stayed unreachable.

### Task 5.5: Ship the first-party integration pack

**Files:** Add typed modules/tests for `gitlab`, `issues/{linear,jira}`, `chat/{slack,teams}`, generic webhook; manifests under `examples/integration-pack/*/plugin.toml`; modify integration exports and existing GitHub/MCP/ACP/provider constructors.

- [ ] Add mock-server contract tests per service for auth lease injection, bounded responses, retries/rate limits, idempotent external effects, SSRF/redirect controls, and hostile text sanitization.
- [ ] Package GitHub/GitLab, Linear/Jira, Slack/Teams, generic webhooks, OpenAI-compatible providers, MCP, and ACP with least-privilege manifests.
- [ ] Install each through the real marketplace path; smoke-test before explicit enable.
- [ ] Commit service pairs separately, then catalogue: `feat(integrations): ship first-party marketplace pack`.

**Verified 2026-08-17 — not started.** `crates/integrations/src/` is `acp.rs, acp_client.rs,
acp_registry.rs, github/, ide/, mcp/, search/, unsloth/, webhook/`: there is no `gitlab`, no
`issues/` directory (so no Linear or Jira), and no `chat/` directory (so no Slack or Teams).
`examples/integration-pack/` exists but is a **single skill example** — six files, `skill.toml` +
`SKILL.md` + `README.md` + `hooks/pre-tool-audit.toml` — with **no `plugin.toml` at any depth**. Do
not read its name as evidence that the pack landed.

### Task 5.6: Milestone 5 exit gate

- [ ] Scan logs, DB, events, artifacts, support bundles, and test outputs for sentinel secrets.
- [ ] Exercise publisher-key revocation, permission expansion, compromised plugin, Vault outage, expired token, and offline local environment reference.
- [ ] Run secrets, marketplace, sandbox adversarial, integration, client, migration, deny/audit, and common gates.

---

## Milestone 6 — Cross-repository architecture intelligence

> **Verified 2026-08-17 — the whole milestone is built and none of it is reachable.**
> `crates/federation/` exists with all eight modules (`authorization, campaign, error, identity,
> publication, query, store, tombstone`, ~3,800 lines) plus a 1091-line integration suite, and
> `migrations/0047_graph_publication.sql` (7 tables) and `0048_multi_repo_campaigns.sql` (5 tables)
> have landed. **`codypendent-federation` is a workspace member that no crate depends on** —
> neither `crates/daemon/Cargo.toml` nor `crates/codypendentd/Cargo.toml` lists it — so those
> tables have no production writer and the crate cannot execute in the product.
>
> All fourteen federation commands exist on the wire
> (`crates/protocol/src/command.rs:996-1064`, replies at `crates/protocol/src/envelope.rs:460-490`)
> and **not one has a handler or a `role_permits` arm.** They fall through `_ => false`
> (`crates/daemon/src/commands.rs:3452`) and are role-denied for every client — the §1 catch-all
> trap, at fourteen commands at once. The guard test written to prevent exactly this
> (`commands.rs:6081-6094`) passes, because its `client_issued` list is hand-maintained and nobody
> added them. **Add every federation command to that list as part of wiring them.**
>
> Also outstanding: no golden vectors exist for any federation payload, and no CLI, TUI, or desktop
> surface references federation or campaigns.
>
> Handler wiring was in flight at the time of writing — re-verify `role_permits` and the guard-test
> list before acting on this note.

### Task 6.1: Publish policy-approved graph facts

**Files:** Add `crates/federation/{Cargo.toml,src/{lib,identity,publication,authorization,store}.rs,tests/publication_it.rs}`; `migrations/0047_graph_publication.sql`; daemon/codypendentd graph publication modules.

- [x] Add failing tests for metadata-only default, strictest-source classification, stable repository identities, revision/hash/provenance/policy version, idempotent batches, retraction, and tombstones.
- [x] Define `PublicationPolicy` and `SharedGraphStore`; publish stable facts, never local row IDs.
- [x] Keep existing local `CodeGraphQuery` repository-bound and private.
- [ ] Commit: `feat(federation): publish policy-approved graph facts`.

**Verified 2026-08-17 — built, unreachable (see the milestone note above).** The federated identity
this milestone needed does exist: `derive_federated_id` =
`SHA-256(root_commit || '\n' || normalized_remote)` with a URL normalizer
(`crates/federation/src/identity.rs:1-6`, `normalize_remote` at `:25`) — which closes finding 1 of
the implementation guides *in the crate*. **It is not adopted**:
`crates/knowledge/src/codegraph.rs:174` still derives `stable_repository_id` from the canonical
local path, and nothing calls the federation version. Two identity schemes now exist and only the
path-based one is in use. Note also that `store.rs`, `campaign.rs`, `query.rs`, `tombstone.rs` and
`authorization.rs` carry **no in-file unit tests** — coverage is concentrated in
`tests/publication_it.rs`.

### Task 6.2: Implement access-safe shared traversal and planning

**Files:** Add federation query/blast-radius/migration-plan/ownership modules/tests; `crates/protocol/src/federated_graph.rs`; daemon/codypendentd operations and vectors.

- [x] Add adversarial tests comparing byte-equivalent absent/inaccessible seed results and proving hidden intermediate nodes cannot affect paths, counts, cursors, or timing class.
- [x] Apply authorized repository grants at seed selection and every recursive traversal step.
- [x] Implement cross-repository blast radius, API/schema migration planning, dependency campaign targets, and ownership-aware reviewer suggestions with evidence.
- [ ] Drive queries from CLI/desktop with grant-limited fixtures.
- [ ] Commit: `feat(federation): query shared architecture safely`.

**Verified 2026-08-17 — built, unreachable.** `crates/federation/src/query.rs` and
`authorization.rs` implement the traversal and the grant checks, and
`crates/protocol/src/federated_graph.rs` (719 lines) carries the wire types, exported at
`crates/protocol/src/lib.rs:27,71` and present in the generated schema. **No CLI, TUI, or desktop
surface exists** — grep for `federation|federated|campaign` across `crates/cli/src`,
`crates/tui/src`, and `apps/desktop/src` returns nothing — and **no golden vectors exist** for any
federated-graph or campaign payload.

### Task 6.3: Coordinate multi-repository campaigns with separate authority

**Files:** Add `migrations/0048_multi_repo_campaigns.sql`; federation campaign/store modules/tests; protocol campaign; daemon/codypendentd campaign operations; workflow store integration; CLI tests.

- [x] Add failing tests for one existing workflow child per repository, separate worktrees/budgets/policy/secret leases/approval digests, partial failures, resume, and idempotent retry.
- [x] Persist campaign/repository/run/approval/effect records and create children through `WorkflowStore::create_run_idempotent_owned`.
- [x] Allow coordinator aggregation only; prohibit blanket approval and shared credentials/worktrees.
- [ ] E2E a two-repository API migration with one denial and selective retry.
- [ ] Commit: `feat(federation): coordinate repository-scoped campaigns`.

**Verified 2026-08-17 — built, unreachable.** `crates/federation/src/campaign.rs` (930 lines) and
`migrations/0048_multi_repo_campaigns.sql` (`campaigns:9`, `campaign_repositories:37`,
`campaign_runs:66`, `campaign_approvals:86`, `campaign_effects:109`) exist. The e2e bullet cannot
pass while `CreateCampaign`/`ExecuteCampaign`/`CancelCampaign` are role-denied — this is the
milestone's blocking dependency, not a separate task.

### Task 6.4: Milestone 6 exit gate

- [ ] Run graph publication/traversal inference tests, campaign recovery/effect dedup, protocol vectors, multi-user authorization, and full gates.
- [ ] Confirm no private source/transcript/path/evidence is published under metadata-only policy.

---

## Milestone 7 — Hybrid control plane

> **Verified 2026-08-17 — a real service that nobody can log into, and it does not ship.**
> `crates/control-plane-protocol/` (20 modules), `crates/control-plane/` (Axum + SQLx + PostgreSQL,
> ten migrations), `sdk/control-plane/`, `sdk/control-plane-react/` and `apps/web/` all exist and
> pass their tests. Three things gate every claim below:
>
> 1. **No route can mint the first user, so no daemon can ever pair.**
>    `POST /v1/auth/login` (`crates/control-plane/src/http.rs:35`) returns `501 not_implemented`
>    unconditionally and touches no store (`routes/auth.rs:32-43`). `create_user_token` is reachable
>    only from `refresh` (`routes/auth.rs:90`), which needs a refresh-token row no HTTP path can
>    create; `Store::create_user` (`store/mod.rs:230`) has no route. `start_pairing_challenge`
>    requires `Principal::User` (`routes/auth.rs:133-140`). The entire HTTP surface is therefore
>    unreachable on a fresh deployment.
> 2. **The control plane is not in the release.** `.github/workflows/release.yml:144-148` builds
>    `--bin codypendent --bin codypendentd --bin codypendent-ui-worker-launcher` and nothing else.
>    There is no `codypendent-control-plane` binary, no container image, and no chart in the
>    release — nor anywhere in the repository.
> 3. **CI never executes any of it against PostgreSQL.** `.github/workflows/ci.yml` has no Postgres
>    service and no `DATABASE_URL`, so `crates/control-plane/tests/migrations_it.rs:28-32` skips
>    itself and every other control-plane test runs on `MemoryStore`. All ten PostgreSQL migrations
>    and the whole of `src/store/postgres.rs` are unexercised.
>
> **The auth work itself is sound** and should not be re-done: the hardcoded default JWT secret is
> gone and now banned by value and by placeholder substring
> (`crates/control-plane/src/config.rs:7-24`, `resolve_jwt_secret` at `:80-105`, fail-closed startup
> at `main.rs:35-39`); daemon JWTs **are** validated against their DB row including `state` and
> `revoked_at`, with authority taken from the row rather than the claims (`auth/mod.rs:222-249`,
> `:281-283`); and `repository_id` from a request body **is** validated and org-scoped, with
> cross-tenant reads answered as 404 and the publication class intersected across org, repo, daemon
> and request (`routes/sync.rs:60-107`). This work was in flight during the audit — re-verify before
> acting on it.

### Task 7.1: Add the independent network protocol and generated SDK

**Files:** Add `crates/control-plane-protocol/` modules for version/IDs/page/error/auth/identity/RBAC/repository/publication/sync/audit/artifact/events/runner and tests; `protocol-vectors/control-plane/v1/`; `sdk/control-plane/` generated client/stream/tests; workspace wiring.

- [ ] Add failing golden/schema/compatibility tests for current plus one previous version, opaque pages, idempotency, publication classes, resumable streams, sync deltas/receipts/tombstones/approvals/schedules/runner events.
- [x] Keep local `Envelope` out of this crate; share only semantically identical value types.
- [x] Generate TypeScript using Milestone 1 infrastructure.
- [x] Commit: `feat(control-plane): define versioned network protocol`.

**Verified 2026-08-17 — types yes, vectors no.** `crates/control-plane-protocol/` has 20 modules
plus `src/bin/export_schema.rs`, and `sdk/control-plane/src/types/` (14 files) is generated from it.
**The crate has no `tests/` directory at all, `protocol-vectors/control-plane/v1/` was never
created, and no generated `*.schema.json` is committed** — so the first bullet, which is the whole
compatibility guarantee, has nothing behind it. Fix
`sdk/protocol/test/protocol-vectors.test.ts:2369-2371` (still a flat `readdirSync`) **before** the
first nested vector lands, or the partition guard will pass while checking nothing; the VS Code
suite already recurses (`extensions/vscode/test/protocol-vectors.test.ts:97-115`).

### Task 7.2: Create the single PostgreSQL control-plane service

**Files:** Add `crates/control-plane/{Cargo.toml,src/{lib,main,config,error,http,health,state}.rs,src/store/{mod,postgres}.rs,migrations/0001_identity.sql..0005_audit.sql,tests/{migrations,health}_it.rs}`; workspace dependencies.

- [ ] Add failing health/config/migration tests against PostgreSQL, including upgrade from each control-plane schema fixture.
- [x] Add Axum/Tower/SQLx PostgreSQL service and configuration-injected identity/object endpoints; no managed/self-hosted domain forks.
- [ ] Separate runtime and migration DB roles; make audit rows append-only by DB permission.
- [x] Commit: `feat(control-plane): add postgres service foundation`.

**Verified 2026-08-17 — the service exists; the PostgreSQL guarantees are untested.** The crate
builds a real Axum service (`http.rs`, `routes/{auth,orgs,repos,sync,objects,audit}.rs`,
`store/postgres.rs`, `main.rs`) and applies its migrations at
`store/postgres.rs:26`. But `tests/migrations_it.rs:28-32` **skips itself when `DATABASE_URL` is
unset**, which is always in CI, and `auth_it`, `rbac_it`, `sync_it`, `objects_it`, `audit_it` and
`health_it` all run against `MemoryStore`. Note the migration count is **`0001`–`0010`, not
`0001`–`0005`** as the file list says: `0006`–`0010` were added ahead of M8/M9 and have no Rust
caller (see the M8 note).

### Task 7.3: Implement GitHub OAuth, generic OIDC, identity linking, and workload tokens

**Files:** Add control-plane identity modules and tests.

- [ ] Add failing PKCE/state/nonce/JWKS/issuer/audience/expiry tests, linking proof tests, refresh-token hash tests, and email-collision non-link tests.
- [ ] Implement GitHub OAuth and OIDC discovery; linking requires authenticated proof of both identities.
- [x] Issue short-lived audience/purpose/org/repository-bound daemon tokens and auditable pairing challenges.
- [ ] Commit: `feat(identity): add github oidc and workload identity`.

**Verified 2026-08-17 — token issuance and validation are real; there is no login.** Daemon and
workload tokens are minted and then validated against their DB row, with `state`/`revoked_at`
checked and authority read from the row rather than the claims
(`crates/control-plane/src/auth/mod.rs:222-249`, `:259-273`, `:281-283`); the JWT secret is
mandatory, length-checked and placeholder-banned (`config.rs:7-24`, `:80-105`). **Neither GitHub
OAuth nor generic OIDC is implemented**: `POST /v1/auth/login` returns `501` and touches no store
(`routes/auth.rs:32-43`). Because pairing requires `Principal::User` (`routes/auth.rs:133-140`) and
no route can create one, the pairing challenge is unreachable in practice. **This one gap makes all
of M7.6–M7.10 undemonstrable.**

### Task 7.4: Enforce RBAC and immutable audit

**Files:** Add `src/authz/*`, `src/audit/*`, matrix/non-disclosure/audit tests.

- [x] Add exhaustive role/action/scope matrix tests and absent/inaccessible response equivalence tests.
- [x] Authorize before resource load/count; intersect organization/repository/local authority.
- [x] Append actor/action/target digest/correlation/hash-chain records transactionally without product secrets/content.
- [ ] Commit: `feat(control-plane): enforce scoped rbac and audit`.

**Verified 2026-08-17 — implemented and tested, but only against `MemoryStore`.**
`src/authz/mod.rs` and `src/audit/mod.rs` exist with `tests/rbac_it.rs` and `tests/audit_it.rs`;
`sdk/control-plane/test/audit-verifier.test.ts` checks the hash chain client-side. Authorization
runs before load and answers cross-tenant reads as 404 (`routes/sync.rs:60-90`). The append-only
guarantee is enforced in code, **not by DB permission** — 7.2's "separate runtime and migration DB
roles" is not done — and no test has ever run these paths against PostgreSQL.

### Task 7.5: Add S3-compatible published-object storage

**Files:** Add control-plane object store trait, S3/memory adapters, manifest, tamper tests.

- [ ] Add failing content-addressed upload, wrong hash/length, bounded range, partial resume, minimum-scope presign, and tombstone tests against memory and MinIO.
- [x] Keep PostgreSQL authoritative for metadata/policy; object storage owns bytes only.
- [x] Commit: `feat(control-plane): store verified published objects`.

**Verified 2026-08-17 — memory adapter only.** `src/storage/mod.rs` and `routes/objects.rs` exist
with `tests/objects_it.rs`, which runs against the in-memory store. **There is no MinIO service in
CI** (no services block in `.github/workflows/ci.yml`), so the S3-compatible half of the first
bullet is unverified.

### Task 7.6: Pair and synchronize outbound local daemons

**Files:** Add `migrations/0049_control_plane_sync.sql`; `crates/daemon/src/control_plane_sync/*`; tests; `crates/codypendentd/src/control_plane.rs`; startup wiring.

- [x] Add failing tests for explicit pairing display/consent, revocation, durable outbox, duplicate deltas/receipts/approvals, resumable cursor, queued shared operation, and local operation with no config/outage.
- [x] Add offline-delete/reconnect test proving tombstones are consumed before new publication.
- [x] Reuse knowledge outbox patterns but keep dedicated sync tables and encrypted credential references.
- [x] Initiate outbound authenticated sync only; no workstation inbound port.
- [ ] Commit: `feat(daemon): synchronize with an optional control plane`.

**Verified 2026-08-17 — the engine is complete and never started.**
`migrations/0049_control_plane_sync.sql` (168 lines, 7 tables) and
`crates/daemon/src/control_plane_sync/{client,engine,error,inbound,outbox,pairing}.rs` exist,
declared at `crates/daemon/src/lib.rs:26`, with `crates/daemon/tests/control_plane_sync_it.rs`
covering the listed properties. **`SyncEngine::new` is constructed only by that test file.**
`crates/daemon/src/server.rs` and `commands.rs` contain **zero** occurrences of `control_plane`, so
the engine is never spawned and nothing enqueues to the outbox in production; and
`crates/codypendentd/src/control_plane.rs` — the startup wiring this task names — **does not
exist.** The "local operation with no config" property holds trivially, because that is the only
mode there is.

### Task 7.7: Expose idempotent REST resources and resumable streams

**Files:** Add control-plane API modules for organizations/repositories/sessions/inbox/approvals/artifacts/analytics/automation/marketplace/daemons/events and idempotency tests.

- [ ] Add failing tests that every mutation atomically stores receipt + domain effect and duplicate keys return the original response.
- [ ] Add stream persist-before-publish/resume/reconnect/authorization tests.
- [x] Expose only shared classes and bounded queries; browser API cannot request unpublished local content.
- [ ] Commit: `feat(control-plane): expose shared resources and event streams`.

**Verified 2026-08-17 — six of eleven resource families exist.** `routes/` covers `auth`, `orgs`,
`repos`, `sync`, `objects`, `audit`, with `ws.rs` for streaming and
`sdk/control-plane/test/idempotency.test.ts` + `stream.test.ts` on the client side. **Absent:**
sessions, inbox, approvals, analytics, automation, marketplace, daemons, and events as first-class
resources. The publication-class intersection at `routes/sync.rs:93-107` — which refuses
`PrivateLocal` outright — is the third bullet's guarantee and it is genuinely implemented.

### Task 7.8: Build the browser client from shared UI

**Files:** Add `sdk/control-plane-react/`; `apps/web/` routes/features/tests for sessions/inbox/approvals/analytics/marketplace/automation/artifacts; modify shared UI only for genuinely reusable views.

- [ ] Add failing auth callback, stream resume, authorization, deep-link, loading/error/empty, and accessibility tests.
- [x] Consume generated SDK and `@codypendent/ui`; do not import desktop internals.
- [x] Show classification/publication status and never imply local content access.
- [ ] Commit: `feat(web): add shared team workspace client`.

**Verified 2026-08-17 — a partial app for a service nobody can log into.** `apps/web/src/views/`
has `Login`, `Overview`, `Sessions`, `Approvals`, `AuditLogs`, `Users`, `ApiKeys`, `Settings`, with
seven test files, on top of `sdk/control-plane-react/`. **Absent:** the inbox, analytics,
marketplace, automation and artifact routes this task lists — there is no `features/` directory at
all. `LoginView` cannot succeed while `POST /v1/auth/login` returns `501`.

### Task 7.9: Ship self-host deployment and observability

**Files:** Add control-plane Dockerfile/Compose/Helm/Kustomize; `docs/self-hosting/{control-plane,backup-restore}.md`; OTEL setup/health/metrics.

- [ ] Test container image as non-root, migration init, readiness/liveness, config validation, backup/restore, and upgrade/rollback against PostgreSQL + MinIO + test OIDC.
- [ ] Add structured correlation across browser/control-plane/daemon without publishing private telemetry by default.
- [ ] Lint/render all deployment manifests.
- [ ] Commit: `feat(deploy): package self-hosted control plane`.

**Verified 2026-08-17 — nothing exists.** There is **no Dockerfile, no Compose file, no Helm chart
and no Kustomize overlay anywhere in the repository** (the only hits are vendored third-party
projects under `reference-repos/`), and `docs/self-hosting/` does not exist. Config validation is
the one piece present, and it lives in the crate rather than in deployment
(`crates/control-plane/src/config.rs`, fail-closed at `main.rs:35-39`). Self-hosting is currently
described only in this plan, the design spec, and
[`../implementation/M7-control-plane.md`](../implementation/M7-control-plane.md) — **prose, not an
artifact anyone can deploy.**

### Task 7.10: Milestone 7 e2e gate

- [ ] GitHub and OIDC login → repository grant → daemon pair → metadata-only sync → browser session/inbox/approval → revoke daemon.
- [ ] Disconnect control plane and prove local CLI/TUI/desktop/VS Code remain functional.
- [ ] Run PostgreSQL/MinIO, protocol compatibility, RBAC non-disclosure, sync partition, web accessibility, deployment, and common gates.

---

## Milestone 8 — Self-hosted remote runners

> **Verified 2026-08-17 — a well-tested library with no binary, no transport, and no scheduler to
> talk to.** `crates/runner/` exists with a real `RunnerBackend` trait (`backend/mod.rs:25`), a
> container backend that genuinely spawns `docker`/`podman`/`nerdctl`
> (`backend/container.rs:282,320,401`) and refuses fail-closed when none is present (`:311`), a
> process backend, hostile-archive materialization, log streaming, attestation, and six test files
> (1138 lines). None of it can run:
>
> - **No binary.** `crates/runner/Cargo.toml` has no `[[bin]]` and there is no `src/main.rs`, so the
>   "runner agent daemon" cannot be started.
> - **No network client.** No `reqwest`, `axum`, or `tokio-tungstenite` dependency; the only
>   `ControlPlaneClient` implementation is `InMemoryControlPlane` (`src/client.rs:57`) and the only
>   object store is `InMemoryObjectStore` (`:367`). **The crate doc's claim of "WebSocket/long-polling
>   dispatch" (`src/lib.rs:5`) is false and should be corrected.**
> - **No dependent crate.** `codypendent-runner` appears only in `Cargo.toml:63` and its own
>   manifest.
> - **Nothing to claim from.** `crates/control-plane/src` has no scheduler module (grep for
>   "scheduler" returns nothing) and `routes/mod.rs` declares no runner routes. The crate does not
>   even depend on `codypendent-control-plane-protocol`, so the server cannot speak the runner
>   protocol.
> - **581 lines of dead schema.** `crates/control-plane/migrations/0006_runner_jobs.sql`,
>   `0007_runner_leases.sql` and `0008_runner_attestations.sql` are applied at startup and
>   referenced by **no `.rs` file in the repository**.
> - **Duplicate job types.** `crates/runner/src/types.rs` (426 lines) redefines what
>   `control-plane-protocol::runner` already models, and the two are unlinked.

### Task 8.1: Complete deployment-neutral runner contracts

**Files:** Expand control-plane protocol runner/artifact modules, vectors, and generated SDK.

- [ ] Add golden/canonical tests for capabilities/eligibility, job/input/sandbox/resource specs, claim/lease/release/cancel/log/output/attestation.
- [x] Bind canonical attestation bytes to job, attempt, lease, runner/image, input/output hashes, timestamps, and result.
- [x] Commit: `feat(runner): define deployment-neutral execution protocol`.

**Verified 2026-08-17 — contract-only, and untested.** All the types exist in a single module
`crates/control-plane-protocol/src/runner.rs` (574 lines) rather than the three the plan names:
capabilities `:92`, registration `:117`, heartbeat `:160`, claim `:187`, lease `:210`, `JobSpec:232`,
`SandboxSpec:258`, `ResourceSpec:280`, output declaration/registration `:290`/`:300`, and the
attestation family `:418-552` with `ATTESTATION_SCHEME_V1` and its canonical digest documented at
`:447`/`:476`. **There are no golden vectors and no tests**: the crate has no `tests/` directory and
`protocol-vectors/` contains nothing runner- or attestation-related. The only consumer of any of
these types is the schema exporter (`src/bin/export_schema.rs:159-164`).

### Task 8.2: Persist scheduler jobs, attempts, leases, and attestations

**Files:** Add control-plane migrations `0006_runner_jobs.sql`, `0007_runner_leases.sql`, `0008_runner_attestations.sql`; scheduler modules/API/tests.

- [ ] Add concurrent PostgreSQL tests for `SKIP LOCKED` claims, matching-generation renew, expiry/retry, cancel/complete CAS, duplicate submission, and one accepted attempt.
- [ ] Implement eligibility by policy/capability/residency/budget/region and durable terminal state.
- [ ] Commit: `feat(control-plane): schedule leased runner jobs`.

**Verified 2026-08-17 — schema only; there is no scheduler.** The three migrations exist and are
applied (`0006_runner_jobs.sql` 148 lines, `0007_runner_leases.sql` 63, `0008_runner_attestations.sql`
71) and **no Rust code references any of their tables.** `0006` even anticipates the idempotency
shape the plan needs — `idempotency_key TEXT NOT NULL` described as "Caller-chosen" (`:50-53`),
unique per organization (`:94`), one attempt per job (`:124`) — and nothing populates it, which is
also why finding 12 in the implementation guides is still open. No scheduler module, no runner
routes, no eligibility code.

### Task 8.3: Build runner core and hostile input materialization

**Files:** Add `crates/runner/` client/identity/claim/materialize/execute/log/upload/attest/cleanup and tests.

- [x] Add archive tests for absolute/parent path, symlink/hardlink escape, duplicate conflict, expansion/size limits, undeclared/wrong hashes.
- [x] Add claim-loop partition tests and compromised runner tests requesting another job/repository/secret.
- [x] Materialize content-addressed inputs, execute through an injected backend, stream bounded logs, upload complete outputs, sign attestation, and clean every workspace.
- [ ] Commit: `feat(runner): execute leased jobs securely`.

**Verified 2026-08-17 — built against an in-memory control plane only.** The behavior exists, under
different filenames than the plan lists: the claim loop is `agent.rs:55-100` (not `claim.rs`),
execution is `backend/{process,container}.rs` (not `execute.rs`), upload is `agent.rs:211-240` (not
`upload.rs`), cleanup is `workspace.rs`'s `WorkspaceGuard` (not `cleanup.rs`); `identity.rs`,
`materialize.rs`, `log_streamer.rs` and `attestation.rs` match. Six test files (1138 lines) cover
hostile archives, the claim loop, the container backend, sandbox execution and attestation.
**Every one of them runs against `InMemoryControlPlane`** — see the milestone note: there is no
transport, no binary, and no server-side counterpart.

### Task 8.4: Add hardened container execution

**Files:** Add runner backend trait/container/process, runner Docker/Compose, container/network tests.

- [x] Add failing tests for non-root, read-only root, dropped capabilities, workspace-only mounts, CPU/memory/PID/output bounds, cancellation, and deny-by-default network.
- [x] Translate `SandboxSpec` into enforceable container controls; refuse unsupported grants rather than silently weakening them.
- [ ] Run the existing Codypendent runtime inside disposable job environments.
- [ ] Commit: `feat(runner): add hardened container backend`.

**Verified 2026-08-17 — the backend is real; there is no image to run in it.**
`crates/runner/src/backend/container.rs` spawns a real container runtime (`:320`), probes for
`docker`/`podman`/`nerdctl` (`:401`) and refuses fail-closed when none is available (`:311`), with
`tests/container_backend_tests.rs` and `sandbox_execution_tests.rs`. **There is no Dockerfile or
Compose file anywhere in the repository**, so nothing packages the Codypendent runtime into a
disposable job environment. Finding 10 of the implementation guides is unchanged and still applies
here: `validate_enforceable_profile` rejects any non-empty `network_allowlist`
(`crates/sandbox/src/executor.rs:1284-1289`) and `bwrap_argv` emits `--unshare-net` unconditionally
(`:647`) — correct fail-closed behavior, but this bullet's "translate `SandboxSpec`" still has no
answer for a populated allowlist, and the doc comments at `executor.rs:25` and `:604` still claim a
conditional that does not exist.

### Task 8.5: Verify artifacts, partial uploads, and attestations

**Files:** Add scheduler artifact/attestation modules and control-plane/runner tests.

- [ ] Add failing tests for partial upload retry, bad hash/unknown signer/revoked image or key/wrong lease-attempt/malformed attestation, and quarantine.
- [ ] Keep jobs `uploading` until every output hash verifies; only accepted attempt artifacts may continue workflows.
- [ ] Commit: `feat(runner): verify outputs and execution attestations`.

**Verified 2026-08-17 — runner-side only; there is no verifier.** The runner signs
(`crates/runner/src/attestation.rs`, `tests/attestation_tests.rs`) and the protocol models
verification errors and a quarantine reason
(`crates/control-plane-protocol/src/runner.rs:450`, `:552`). **Nothing on the server verifies
anything** — `runner_attestations` has no Rust reader, and no scheduler exists to hold a job in
`uploading`.

### Task 8.6: Dispatch existing workflow nodes remotely

**Files:** Add `crates/codypendentd/src/remote_node_executor.rs`; codypendentd/control-plane dispatch tests.

- [ ] Add failing e2e tests that policy-selected nodes submit jobs and map accepted outputs to `NodeOutcome` while local nodes still use `AgentLoopNodeExecutor`.
- [ ] Preserve node attempts, costs, attribution, retry, observer transitions, and workflow budget; do not alter `WorkflowDriver` semantics.
- [ ] Test daemon disconnect while remote work finishes and result sync on reconnect.
- [ ] Commit: `feat(workflow): dispatch eligible nodes to remote runners`.

**Verified 2026-08-17 — not started.** `crates/codypendentd/src/remote_node_executor.rs` does not
exist, and `RemoteNode` appears nowhere in `crates/`. Every `runner` reference in
`crates/codypendentd/src/workflow_exec.rs` is the unrelated local `RepositoryTestRunner` trait
(`:166`, production impl `ShellRepositoryTestRunner` at `:174-177`, injected at `:759`). When this
task starts, read finding 12 in the implementation guides first: `reset_interrupted_node`
(`crates/workflow/src/store.rs:1005`) still clears `agent_run_id` and `cost_json` while preserving
`attempt` (`:1013-1015`), which is the double-execution trap this task walks into.

### Task 8.7: Add Kubernetes runner controller

**Files:** Add `crates/runner-controller/`; Helm deployment; pod/controller tests.

- [ ] Add pod-spec tests for one hardened Job per attempt, scoped credentials, resource/security context, no broad service account, and terminal/expired cleanup.
- [ ] Keep PostgreSQL scheduler/lease truth authoritative, not Kubernetes annotations.
- [ ] Contract-test container and Kubernetes paths against the same job vectors.
- [ ] Commit: `feat(runner): add kubernetes job controller`.

**Verified 2026-08-17 — not started.** `crates/runner-controller/` does not exist and is not a
workspace member; there is no Kubernetes code and no Helm chart anywhere in the repository. The
third bullet is also blocked upstream: the "same job vectors" it contract-tests against do not
exist (see 8.1).

### Task 8.8: Milestone 8 e2e gate

- [ ] Browser starts shared workflow → container runner claims → verified artifacts continue workflow.
- [ ] Kill Kubernetes runner → lease expires → retry → exactly one output set accepted.
- [ ] Partition at claim/execution/upload/completion and replay cancel/renew/complete messages.
- [ ] Run runner/scheduler/protocol/security/deployment and all common gates.

---

## Milestone 9 — Managed execution and continuous quality

> **Verified 2026-08-17 — not started, and the promotion pipeline it was meant to fix is now a dead
> end.** `crates/runner-provider/` does not exist and is not a workspace member; no Firecracker or
> macOS adapter exists. `crates/control-plane/src` contains **no quality module of any kind** —
> grep for "quality" returns zero files — so none of capture, redaction, experiment, shadow,
> assignment, canary, statistics, drift or promotion is present. The two migrations landed anyway:
> `crates/control-plane/migrations/0009_runner_pools.sql` (100 lines) and
> `0010_quality_observations.sql` (199 lines, with provenance columns and correctly nullable
> measured metrics) are applied at startup and referenced by **no `.rs` file in the repository**.
> `0010`'s own comments cite `crates/routing/src/classify.rs` and `policy.rs:150-153` as the
> intended joins; neither join was ever written.
>
> **The regression to know about:** `PromotionAction::ObserveCanary` is now hard-rejected with
> `promotion.caller-supplied-canary-evidence`
> (`crates/codypendentd/src/promotion.rs:264-270`) — the right instinct, taken without building the
> server-measured replacement. `StartCanary` succeeds (`:259-262`) and `FinishCanary` (`:271-275`)
> now always fails `CanaryInsufficientEvidence { observed: 0, required: 100 }`
> (`crates/eval/src/promote.rs:432-438`), because the only accumulator,
> `PromotionStore::observe_canary_samples` (`crates/eval/src/store.rs:199`), has no non-test caller.
> **No candidate can reach `ComparisonReady` or `Promoted` on any shipped path.** `MIN_CANARY_SAMPLES
> = 100` used to be satisfiable by typing `500`; it is now not satisfiable at all. Design §16
> criterion 15 is still false — it has changed direction, not truth value. Whoever starts 9.5 owns
> re-opening this path with measured evidence, not just deleting the refusal.

### Task 9.1: Add provider-neutral managed runner provisioning

**Files:** Add `crates/runner-provider/{Cargo.toml,src/{lib,provider,model,pool,firecracker,macos}.rs,tests/provider_contract.rs}`; workspace/control-plane wiring.

- [ ] Add provider contract tests for provision/inspect/terminate, idempotency, timeout, image/key revocation, cleanup, and endpoint identity.
- [ ] Implement Firecracker behind a narrow Unix-socket API adapter; the guest runs the unchanged runner/protocol.
- [ ] Keep macOS adapter feature-gated with explicit unsupported behavior until configured.
- [ ] Commit: `feat(runner): add managed execution provider contract`.

### Task 9.2: Add warm pools and capability-aware autoscaling

**Files:** Add control-plane `0009_runner_pools.sql`; scheduler pools/autoscale/providers modules/tests.

- [ ] Add failing tests for queued eligible jobs, capability/image/region pools, min/max, provisioning latency, scale down, revoked image, and concurrent reconciler idempotency.
- [ ] Reset every warm instance to a known snapshot and issue fresh runner identity/job credentials; reject residual writable state.
- [ ] Commit: `feat(runner): autoscale isolated warm pools`.

### Task 9.3: Capture policy-controlled real execution traces

**Files:** Add control-plane `0010_quality_observations.sql`; quality capture/store/redact modules/tests.

- [ ] Add failing classification/redaction tests and missing-measurement tests when translating execution to `codypendent_eval::Trace`.
- [ ] Store large trace payloads as classified artifact refs and measured metadata in PostgreSQL.
- [ ] Never capture beyond publication policy or fabricate token/cost/quality fields.
- [ ] Commit: `feat(quality): capture approved execution observations`.

### Task 9.4: Execute real shadow experiments

**Files:** Add quality experiment/shadow/assignment modules/tests; reuse routing `RouteArm*` and eval candidates.

- [ ] Add deterministic assignment and isolation tests proving same approved inputs, separate budget/credentials, no production effects/approvals, and no output influence.
- [ ] Persist baseline/candidate `RouteArmResult` observations before activating shadow state.
- [ ] Commit: `feat(quality): run isolated shadow candidates`.

### Task 9.5: Add canary comparison, drift, and automatic rollback

**Files:** Add quality canary/statistics/drift modules/tests; modify production promotion command path.

- [ ] Add tests for eligible population, stable assignment, `MIN_CANARY_SAMPLES`, fixed/sequential analysis horizon, quality non-inferiority, cost/latency limits, missing data, safety rollback, and drift.
- [ ] Build `RouteEvalReport` from server-measured samples and drive existing `PromotionStore` transitions.
- [x] Deprecate/reject production caller-supplied `CanaryMetrics`; retain wire compatibility until a major protocol release.
- [ ] Commit: `feat(quality): evaluate and roll back canary routes`.

**Verified 2026-08-17 — the third bullet landed alone, and on its own it breaks promotion.** Wire
compatibility is retained exactly as specified: `PromotionAction::ObserveCanary { metrics }`
(`crates/protocol/src/command.rs:1382`) and its five `CanaryMetrics` fields (`:1394-1400`) are
untouched, and the CLI still accepts all five (`crates/cli/src/main.rs:1090-1104`, assembled at
`:1143-1160`). The daemon now refuses them (`crates/codypendentd/src/promotion.rs:264-270`), which
left `validate_canary_metrics` (`:312`) and `canary_regressed` (`:329`) dead-code-gated. **Bullets
one and two — the measured replacement — are not started**, so the refusal has no successor and the
state machine cannot advance past canary. See the milestone note. `RouteArm` still has no driver
(finding 11): `crates/routing/src/arms.rs:15-25` is unchanged and accurate, and `codypendent eval`
still has exactly one subcommand (`crates/cli/src/main.rs:931-960`).

### Task 9.6: Require scoped human promotion and expose quality UI

**Files:** Add control-plane quality promotion/API modules/tests; `apps/web/src/features/quality/*`; shared observability UI.

- [ ] Add failing tests proving runner/candidate/grader/unscoped org admin cannot self-promote and exact evidence/action digest/expiry is required.
- [ ] Authenticate scoped Approver/Maintainer, append immutable comparison evidence, and call the existing promotion state machine.
- [ ] Add accessible baseline/shadow/canary/drift/rollback evidence UI with unknown measurements explicit.
- [ ] Commit: `feat(quality): gate promotions on measured evidence and approval`.

### Task 9.7: Final program acceptance gate

- [ ] Run the 16 acceptance criteria from the specification as executable end-to-end scenarios and store machine-readable results under the existing eval/test evidence conventions.
- [ ] Run a container and managed microVM against the same runner golden job; compare accepted semantic outputs and attestations.
- [ ] Run replay, compromised runner, artifact tamper, partial upload, publisher revocation, malicious archive, path/symlink, cancellation/lease, oversized payload, offline deletion, and every runner-state partition scenario.
- [ ] Run all Rust/TypeScript/UI/deployment/docs/dependency/security gates on Linux and supported macOS targets.
- [ ] Audit documentation so only production-driven behavior is marked complete; document managed and self-hosted operation, recovery, security boundaries, and compatibility.
- [ ] Final review commit only: `docs: record hybrid platform acceptance evidence`.

---

## Milestone review template

For every milestone, attach:

1. commits and exact file scope;
2. failing-test evidence followed by passing-test evidence;
3. migration upgrade/checksum result;
4. protocol/schema/vector compatibility result;
5. security/adversarial result;
6. production caller and end-to-end scenario;
7. full regression result and any pre-existing unrelated failures;
8. documentation claims changed;
9. rollback/disable path;
10. explicit approval before beginning the next milestone.

## Program acceptance traceability

| Specification acceptance criterion | Primary proof |
|---|---|
| Local operation without hosted service | M2.8, M7.6, M7.10 |
| Shared durable local state across clients | M2.3–M2.8 |
| Provenance-rich global search | M2.1 |
| Lifecycle, bundles, internal archival | M2.2, M2.7 |
| One durable pending-work inbox | M3.1–M3.2 |
| Durable policy-gated automation | M4.1–M4.6 |
| Honest measured usage and quality | M3.3–M3.5, M9.3 |
| Verified/revocable/disabled marketplace | M5.3–M5.6 |
| No cross-repository inference leak | M6.1–M6.4 |
| One managed/self-host control plane | M7.2, M7.9–M7.10 |
| Outbound revocable metadata-only pairing | M7.3, M7.6 |
| One protocol for container and microVM | M8.1, M9.1, M9.7 |
| Exactly one accepted runner output set | M8.2, M8.5, M8.8 |
| End-to-end hash and provenance | M0.4, M7.5, M8.5 |
| Real observations drive gated experiments | M9.3–M9.6 |
| Documentation reflects production reality | M0.3–M0.7, M9.7 |

## Explicitly deferred until separately approved

- Pushing branches, merging, tagging, publishing packages/images, creating releases, or deploying shared infrastructure.
- Windows runner implementation; protocol compatibility remains required.
- A real macOS managed provider when no provider is configured; only the adapter contract is in scope.
- Automatically sharing raw source, transcripts, artifacts, memories, or credentials.
