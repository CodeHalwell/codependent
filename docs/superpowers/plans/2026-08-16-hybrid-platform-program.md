# Hybrid Codypendent Platform Program Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development for one milestone at a time. Do not start a later milestone until the preceding exit gate passes. Mark steps complete in this file as evidence lands.

**Goal:** Turn Codypendent into a complete local-first product and optional hybrid team platform, with real clients, governed automation and sharing, and one secure remote-runner protocol.

**Architecture:** Preserve the local daemon as authority for source, private history, artifacts, secrets, and local effects. Add domain capabilities vertically to the existing protocol/daemon/clients, then introduce a separate network protocol and a single PostgreSQL/S3 control-plane implementation for both managed and self-hosted modes. Remote execution reuses the existing workflow, runtime, sandbox, routing, and evaluation engines.

**Tech stack:** Rust 2021 workspace; Tokio; SQLx with SQLite locally and PostgreSQL in the control plane; Axum; S3-compatible object storage; React/TypeScript/Vite; Tauri desktop shell; VS Code API; Docker/Kubernetes; Ed25519; JSON Schema and golden protocol vectors.

**Spec:** `docs/superpowers/specs/2026-08-16-hybrid-platform-program-design.md`

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

- [ ] Save `git status --short`, staged/unstaged stats, and both forms of `git diff --check` as baseline evidence.
- [ ] Review mixed files one at a time with `git diff --cached -- <file>` and `git diff -- <file>`; classify intent without resetting either side.
- [ ] Fix the known staged trailing whitespace in `improvement_plan/ratatui-styling-review.md`; run both diff checks and expect no whitespace errors.
- [ ] Compare root `MEGAPLAN.md`, `300726.txt`, and `docs/docs/adoptions/22-megaplan.md`; restore root content only from the attributable complete document and keep unrelated local files untracked.
- [ ] Re-run every finding in `improvement_plan/findings-register.md` against current symbols; update status only with command/test evidence.
- [ ] Regenerate `Cargo.lock` only after the current dependency edits are understood; do not use it to erase unrelated staged lock changes.
- [ ] Commit coherent existing v0.9 groups separately; example: `docs(review): reconcile verified v0.9 findings`.

### Task 0.2: Verify previously reported correctness fixes

**Files:** `crates/runtime/src/tools/edit_match.rs`; `crates/daemon/src/db.rs`; `crates/knowledge/src/lsp/{mod,servers,client,transport}.rs`; their existing unit/integration tests.

- [ ] Add or confirm regression tests for Unicode edit matching, private SQLite file permissions on creation/reopen, and one Python LSP owner per workspace.
- [ ] Run each focused test before changing code; if it already passes, record verification and make no implementation edit.
- [ ] If a test fails, make the smallest source-of-truth fix and rerun it.
- [ ] Run `cargo test -p codypendent-runtime edit_match`, `cargo test -p codypendent-daemon db`, and `cargo test -p codypendent-knowledge lsp`.
- [ ] Commit only actual fixes: `fix(runtime): preserve unicode edit matching invariants`, etc.

### Task 0.3: Replace simulated desktop behavior with an honest disconnected product state

**Files:** Modify `apps/desktop/src/{App,types}.tsx`; `apps/desktop/src/components/{Navigation,Composer,Transcript}.tsx`; `apps/desktop/package.json`. Add `apps/desktop/test/App.test.tsx` and test setup.

- [ ] Add a failing component test proving no fake sessions appear, no timer produces a completion, and the UI says the daemon is disconnected when no transport is configured.
- [ ] Run `npm --prefix apps/desktop test -- App.test.tsx`; expect failure because no test script/state exists.
- [ ] Add Vitest/testing-library using the repository's React test conventions; remove hard-coded sessions, `setTimeout` completion, and unconditional connected state.
- [ ] Disable run controls while disconnected and provide an actionable daemon-discovery message.
- [ ] Run `npm --prefix apps/desktop run check`, tests, and build; expect all pass.
- [ ] Commit: `fix(desktop): remove simulated daemon state`.

### Task 0.4: Add real bounded artifact retrieval and VS Code patch review

**Files:** Modify `crates/protocol/src/{command,envelope,version}.rs`; `crates/daemon/src/{artifacts,server}.rs`; `crates/protocol/tests/golden_vectors.rs`; `crates/daemon/tests/server_it.rs`; `protocol-vectors/*.json`; `extensions/vscode/src/{client,extension}.ts`; `extensions/vscode/test/{client,patch-review}.test.ts`.

- [ ] Add failing Rust round-trip and server tests for `ReadArtifact { artifact_id, offset, limit, expected_sha256 }` and bounded `ArtifactChunk` replies: first/middle/final chunk, limit clamp, unknown/unauthorized ID, and hash mismatch.
- [ ] Run focused protocol/server tests; expect compile failures for missing variants.
- [ ] Add the additive protocol variants, bump `PROTOCOL_V1.minor`, and range-read through `ArtifactStore::open`, stored ownership/classification, and a limit safely below `MAX_FRAME_BYTES`.
- [ ] Add failing VS Code tests for chunk assembly, hash verification, malformed patch, and multi-file selection.
- [ ] Implement request/reply correlation in the existing client, assemble verified bytes, parse unified patches, create read-only before/after URI documents, and call `vscode.diff`; retain `MAX_DIFF_ENTRIES = 64`.
- [ ] Remove the metadata-only placeholder. Unauthorized and absent artifacts must show the same public error.
- [ ] Run `cargo test -p codypendent-protocol`, `cargo test -p codypendent-daemon --test server_it artifact`, and the extension typecheck/test/build gates.
- [ ] Commit: `feat(vscode): review verified patch artifacts`.

### Task 0.5: Finish provider transport and credential wiring already advertised by the catalog

**Files:** Modify `crates/providers/src/{model,credential,lib}.rs`; `crates/runtime/src/models.rs` and its provider-feature tests; `crates/providers/builtin_catalog.toml`. Add `crates/providers/tests/native_transport_it.rs` if the mock-server cases cannot remain beside the current runtime model tests.

- [ ] Add failing mock-server tests for native Anthropic Messages and Gemini `generateContent` request/stream/error normalization; verify required headers and bounded error snippets.
- [ ] Implement native transports through the existing model-driver seam rather than mapping them to OpenAI-compatible requests.
- [ ] Add failing credential tests for supported IAM/OAuth flows, token expiry/refresh, redacted debug output, and explicit unsupported configuration.
- [ ] Implement `CloudIamCredential` and `OAuthCredential` through injected token-provider traits; do not introduce an interactive browser flow into the daemon.
- [ ] Ensure catalog entries are marked runnable only when transport and credential methods are executable.
- [ ] Run provider/runtime tests and no-secret log scans.
- [ ] Commit: `feat(providers): wire native transports and delegated credentials`.

### Task 0.6: Close internal council sessions

**Files:** Modify `crates/council/src/service.rs`; `crates/codypendentd/src/{executor,workflow_exec}.rs`; add the minimal additive `CloseSession` contract/handler in `crates/protocol/src/{command,envelope}.rs` and `crates/daemon/src/server.rs`; council, protocol, and daemon tests.

- [ ] Add a failing integration test proving member sessions become closed after the parent council reaches a terminal state while events and attribution remain readable. Milestones 1–2 add the explicit internal/parent metadata and automatic archival.
- [ ] Add a principal-owned, idempotent `CloseSession` command and lifecycle reply without pre-empting the broader lifecycle/search contract in Milestone 1.
- [ ] Add an explicit close callback to the council service seam; invoke it on success, failure, and cancellation.
- [ ] Remove the `TODO(protocol)` only after the production path drives the operation.
- [ ] Run `cargo test -p codypendent-council` and focused codypendentd recovery tests.
- [ ] Commit: `fix(council): close internal member sessions`.

### Task 0.7: Milestone 0 exit gate

- [ ] Run all common verification commands plus every npm check/audit for `sdk/protocol`, `sdk/remote-ui`, `sdk/ui`, `extensions/vscode`, and `apps/desktop`.
- [ ] Confirm `.github/workflows/ci.yml` executes desktop and SDK gates; add jobs if absent rather than relying on local-only evidence.
- [ ] Run `evals/ci/run_gate.sh` and sandbox adversarial tests on supported platforms.
- [ ] Confirm root megaplan is complete, docs manifest passes, and no product documentation calls simulated behavior complete.
- [ ] Commit gate/config fixes: `ci: gate desktop and generated sdk surfaces`.

---

## Milestone 1 — Shared local contracts and generated SDK

### Task 1.1: Export authoritative Rust protocol schemas

**Files:** Modify schema-reachable types in `crates/protocol/src/{artifact,catchup,command,envelope,events,handshake,ide,input,run,version}.rs` and `crates/protocol/Cargo.toml`. Add `crates/protocol/src/bin/export_schema.rs`; `sdk/protocol/schema/*.schema.json`; `.github/scripts/check_generated_protocol.sh`.

- [ ] Add failing schema tests requiring roots for `Command`, `Envelope`, `Payload`, `SessionEvent`, catch-up, artifact, and IDE types.
- [ ] Add `JsonSchema` derives and explicit schema roots without changing serialized forms.
- [ ] Implement deterministic schema export with sorted/stable output.
- [ ] Run exporter twice and assert byte-identical output.
- [ ] Commit: `build(protocol): export authoritative json schemas`.

### Task 1.2: Generate the TypeScript SDK and preserve golden compatibility

**Files:** Replace handwritten generated portions of `sdk/protocol/src/{commands,events,ids}.ts`; add generated envelope/payload modules, `sdk/protocol/scripts/generate.mjs`, tests, and exports; modify protocol vector tests and CI.

- [ ] Add a failing drift check that regenerates to a temp directory and compares committed output.
- [ ] Select one deterministic JSON-Schema-to-TypeScript generator, pin it in `sdk/protocol/package-lock.json`, and wrap only naming/order fixes in `generate.mjs`.
- [ ] Keep handwritten framing/client helpers separate from generated files.
- [ ] Reconstruct all Rust golden vectors in TypeScript and unknown additive fields safely.
- [ ] Run Rust golden tests, `npm --prefix sdk/protocol run check`, and the drift script.
- [ ] Commit: `build(protocol): generate typescript sdk from rust schemas`.

### Task 1.3: Define session lifecycle, search, history, editor, inbox, analytics, automation, and bundle contracts

**Files:** Add `crates/protocol/src/{session,inbox,analytics,automation,bundle}.rs`; modify `lib.rs`, `command.rs`, `envelope.rs`, `capabilities.rs`, and vectors.

- [ ] Write serialization/golden tests for opaque cursor pages; search filters/results/deep links; rename/pin/archive/restore/delete/export; editor actions with context and idempotency; inbox page/mutation; usage query/export; trigger/schedule CRUD; bundle export/import.
- [ ] Make all new fields additive/defaulted and all mutations idempotency-keyed.
- [ ] Extend `SessionSummary` with internal, pin/archive, repository/workspace, activity, and run-state fields without breaking old vectors.
- [ ] Reserve payloads and capability bits even when implementations land in later milestones; return explicit unsupported errors until then.
- [ ] Regenerate Rust schemas and TypeScript SDK; run cross-language vectors.
- [ ] Commit: `feat(protocol): define local platform capability contracts`.

### Task 1.4: Add append-only session-library storage

**Files:** Add `migrations/0040_session_library.sql`; `.github/scripts/check_migration_immutability.py`; `migrations/checksums.json`; migration tests in `crates/daemon/src/db.rs` or `crates/daemon/tests/persistence.rs`.

- [ ] Add a failing upgrade test from a v0.9 fixture and a failing immutability test for changed/deleted historical SQL.
- [ ] Add lifecycle/internal/parent metadata and stable search-index source bookkeeping. Use FTS5 only after a runtime capability test; otherwise use the existing Tantivy dependency in Milestone 2.
- [ ] Generate checksums for all historical migrations and teach CI to reject drift while allowing appended files.
- [ ] Verify migrate-up, reopen, and rollback policy (forward repair, no destructive down migration).
- [ ] Commit: `feat(daemon): add session library persistence`.

### Task 1.5: Milestone 1 compatibility gate

- [ ] Run protocol all-features tests, schema fixtures, golden vectors, generator drift, TypeScript SDK checks, migration immutability, and upgrade from previous release.
- [ ] Launch an old supported local client fixture against the new daemon and the new client against a previous-release daemon; assert negotiated degradation rather than disconnect for additive features.
- [ ] Commit compatibility fixtures: `test(protocol): gate previous local client compatibility`.

---

## Milestone 2 — Session Library, bundles, and real local clients

### Task 2.1: Build the ranked Session Library index and query service

**Files:** Add `crates/daemon/src/session_library.rs`; `crates/daemon/tests/session_library_it.rs`; modify `crates/daemon/src/{lib,server,ledger,projections,commands}.rs`.

- [ ] Add failing tests for title/transcript/tool/patch/artifact/path/symbol hits; repository/model/date/status filters; ranking; stable cursor paging; owner isolation; source/scope/provenance/deep-link fields.
- [ ] Index durable events incrementally after ledger append and rebuild deterministically after interruption.
- [ ] Implement lifecycle writes through the existing durable command/idempotency path, including retention-aware delete/tombstone behavior.
- [ ] Wire command handlers using transport-derived principal filters before search/query.
- [ ] Run focused session library, replay, persistence, and multi-user tests.
- [ ] Commit: `feat(daemon): add searchable session library`.

### Task 2.2: Archive internal sessions after parent completion

**Files:** Modify `crates/council/src/service.rs`; `crates/codypendentd/src/{workflow_exec,executor}.rs`; session creation/projection code and tests.

- [ ] Extend the Milestone 0 terminal-state test to success/failure/cancel/recovery and parent-child attribution.
- [ ] Mark internal sessions at creation and archive only after parent terminal persistence succeeds.
- [ ] Ensure active-history queries omit archived internal sessions by default while explicit search can include them.
- [ ] Commit: `feat(session): archive completed internal work`.

### Task 2.3: Build a shared daemon TypeScript transport

**Files:** Add `sdk/protocol/src/{framing,client,session-store}.ts`; tests. Refactor `extensions/vscode/src/{client,protocol/frame}.ts` to consume it.

- [ ] Add failing tests for fragmented frames, handshake/attach/catch-up/live ordering, request correlation, ping/pong, resume token, sequence dedup, bounded offline queue, reconnect backoff, and cancellation.
- [ ] Move behavior from VS Code into `@codypendent/protocol` without changing wire semantics.
- [ ] Keep host discovery/socket bridging injected so Node, Tauri, and tests can supply transports.
- [ ] Run shared SDK and extension tests before deleting duplicate client code.
- [ ] Commit: `refactor(protocol): share daemon client transport`.

### Task 2.4: Implement the Tauri host and real desktop projection

**Files:** Add `apps/desktop/src-tauri/{Cargo.toml,tauri.conf.json,src/main.rs}`; `apps/desktop/src/daemon/{transport,projection,commands}.ts`; `apps/desktop/src/hooks/useDaemonSession.ts`; tests. Modify desktop package, app, types, and components.

- [ ] Add failing transport/projection tests for discovery, connect, create, attach, full paginated catch-up, live overlap dedup, start/cancel, approvals, questions, and artifact reads.
- [ ] Use Tauri only for native daemon discovery/socket and notifications; keep protocol state in the shared TypeScript client.
- [ ] Upgrade desktop to the React version required by `@codypendent/ui` and remove manually mirrored protocol types.
- [ ] Add disconnected/reconnecting/offline states and bounded retry controls.
- [ ] Run desktop check/test/build and `tauri build` on macOS CI.
- [ ] Commit: `feat(desktop): connect to the local daemon`.

### Task 2.5: Extract one semantic Remote UI React renderer

**Files:** Add `sdk/ui/src/host-react/{renderer,store,capabilities,theme,slot-registry,mediated,index}.tsx` as appropriate and tests. Modify VS Code webview Remote UI files and `apps/desktop/src/components/RemoteUiRenderer.tsx`.

- [ ] Port existing VS Code renderer tests to the shared package first; add capability denial, unknown node, slot, event validation, and theme tests.
- [ ] Extract host-neutral state/rendering; keep VS Code and desktop messaging as thin adapters.
- [ ] Replace desktop metadata display with shared semantic rendering and validated daemon events.
- [ ] Run shared UI, VS Code, and desktop suites.
- [ ] Commit: `refactor(ui): share semantic remote ui renderer`.

### Task 2.6: Complete VS Code history and editor-native actions

**Files:** Add `extensions/vscode/src/editor-actions.ts`; tests. Modify `extensions/vscode/{package.json,src/extension.ts}` and panel/client tests.

- [ ] Add failing tests that attach loads every paginated event before live projection and deduplicates catch-up overlap.
- [ ] Add command/menu tests for Fix selection, Explain selection, Review current file, Generate tests, and Fix diagnostic.
- [ ] Register contributions with correct editor/context/diagnostic enablement; send current `IdeContextUpdate`, source identity, and idempotency key into an ordinary daemon run.
- [ ] Prove no extension-only model/tool loop exists and all runs are attributable.
- [ ] Run extension typecheck/lint/test/build/prepublish.
- [ ] Commit: `feat(vscode): add full history and editor actions`.

### Task 2.7: Add versioned redacted session/support bundles

**Files:** Add `migrations/0041_session_bundles.sql`; `crates/daemon/src/bundles.rs`; `crates/protocol/src/bundle.rs` implementation fields; `crates/daemon/tests/bundles_it.rs`; CLI commands/tests in `crates/cli/src/{main,commands,client}.rs` and `crates/cli/tests/bundle_it.rs`.

- [ ] Add failing export tests for explicit inclusion policy, redaction, manifest hashes, transcript/event selection, routing/approval metadata, patch/artifact manifests, and diagnostics.
- [ ] Add hostile import tests for hash mismatch, oversized entries, path/symlink escape, duplicate paths, identity collision, credentials, and unsupported versions.
- [ ] Implement a deterministic content-addressed archive; imports receive new local IDs plus imported provenance and never restore credentials or approvals.
- [ ] Drive export/import through CLI and one desktop action; verify round trip into a fresh database.
- [ ] Commit: `feat(session): export and import redacted bundles`.

### Task 2.8: Milestone 2 end-to-end gate

- [ ] Start a real daemon; create/run/search/rename/pin/archive/restore/export a session from desktop and attach/read/review its patch from VS Code.
- [ ] Verify CLI/TUI still observe the same durable events and local-only operation works with network disabled.
- [ ] Run all Rust, SDK, UI, desktop, extension, docs, deny, and audit gates.
- [ ] Commit e2e harness: `test(clients): gate shared local session lifecycle`.

---

## Milestone 3 — Durable inbox and measured analytics

### Task 3.1: Persist the owner-scoped inbox

**Files:** Add `migrations/0042_inbox.sql`; `crates/daemon/src/inbox.rs`; `crates/daemon/tests/inbox_it.rs`; implement `crates/protocol/src/inbox.rs`; modify daemon server/executor and protocol exports.

- [ ] Add failing tests for deduplicated upsert, unread/read/dismissed/resolved transitions, cursor paging, repository filters, deep links, owner isolation, and idempotent mutation.
- [ ] Persist `inbox_entries` with unique `(owner_uid,dedup_key)` and separate adapter-delivery attempts.
- [ ] Produce entries beside durable approval, question, run terminal, budget, workflow block, plugin permission, and runner failure records; derive owner from source rows.
- [ ] Wire list/acknowledge/dismiss handlers and capability negotiation.
- [ ] Commit: `feat(inbox): persist pending human work`.

### Task 3.2: Deliver deduplicated native notifications

**Files:** Modify `sdk/ui/src/first-party/system.tsx`; VS Code client/extension tests; desktop app/native wrapper; TUI terminal notifier as applicable.

- [ ] Add failing tests proving each unread inbox ID notifies once across reconnect and acknowledgement never resolves an approval/question.
- [ ] Render the daemon-owned inbox in shared UI with deep links and repository context.
- [ ] Add VS Code and Tauri native adapters keyed by durable entry ID; keep email/chat behind disabled policy adapters.
- [ ] Run client suites and a reconnect e2e test.
- [ ] Commit: `feat(clients): notify from the durable inbox`.

### Task 3.3: Persist normalized execution observations

**Files:** Add `migrations/0043_execution_observations.sql`; `crates/daemon/src/analytics.rs`; `crates/daemon/tests/analytics_it.rs`; implement `crates/protocol/src/analytics.rs`; modify ledger, executor, workflow execution, routing, and model profiles.

- [ ] Add failing tests for nullable input/output/cached/reasoning tokens, cost, latency, provider/model, repository/workflow/task class, route/retry/escalation, completion, and grader score.
- [ ] Record measured observations keyed by logical run/attempt while preserving current run-usage compatibility columns.
- [ ] Backfill only values present in durable existing records; assert missing fields remain `NULL`.
- [ ] Commit: `feat(analytics): record measured execution observations`.

### Task 3.4: Query, budget, alert, and export usage/quality

**Files:** Add `crates/daemon/src/analytics/export.rs`; modify protocol/daemon server; `sdk/ui/src/first-party/intelligence.tsx`; shared/desktop views and tests.

- [ ] Add failing aggregate tests by model/provider/repository/workflow/task class/time, including known/unknown sample counts and cost per successful task.
- [ ] Add owner/repository-authorized bounded JSON/CSV export tests and formula/escaping snapshots.
- [ ] Implement configurable budget thresholds that create deduplicated inbox alerts from measured values only.
- [ ] Drive reports and export from desktop; retain explicit unavailable markers for missing data.
- [ ] Commit: `feat(analytics): add usage and quality center`.

### Task 3.5: Milestone 3 exit gate

- [ ] E2E: run measured and unmeasured providers, observe inbox alerts, reconnect clients, query/export analytics, and prove unknowns do not become zero.
- [ ] Run multi-user, replay, migration, client, accessibility, and full common gates.

---

## Milestone 4 — Durable scheduled and event-driven automation

### Task 4.1: Persist trigger bindings, receipts, firings, and attempts

**Files:** Add `migrations/0044_automation.sql`; `crates/daemon/src/automation.rs`; `crates/daemon/tests/automation_it.rs`; implement `crates/protocol/src/automation.rs`.

- [ ] Add failing CRUD/role/owner/repository tests for bindings with source config/filter, workflow/version, dedup, concurrency, trigger retry, misfire, budget, approval mode, and enabled state.
- [ ] Add atomic receipt and attempt transitions; keep invocation policy out of `WorkflowDefinition`.
- [ ] Wire controller-gated mutation and observer-gated query commands.
- [ ] Commit: `feat(automation): persist trigger bindings and receipts`.

### Task 4.2: Dispatch verified GitHub webhooks into workflows

**Files:** Add `crates/codypendentd/src/webhook_dispatch.rs`; `crates/codypendentd/tests/webhook_workflow_it.rs`; modify integrations webhook ingest/server and codypendentd assembly/workflows.

- [ ] Add failing e2e tests: valid enabled binding starts once; duplicate GUID returns prior receipt; invalid signature/filter/repository starts none; crash after receipt retries without duplicate effect.
- [ ] Inject a `WebhookEventSink` after verify → normalize → atomic delivery reservation.
- [ ] Resolve target/owner/repository/policy from enabled server-side binding; derive workflow idempotency from binding ID + delivery GUID.
- [ ] Persist dispatch failure/retry state and remove the current `allow_triggers=false` dead end.
- [ ] Commit: `feat(automation): dispatch github webhooks to workflows`.

### Task 4.3: Implement schedules and generic trigger semantics

**Files:** Add `crates/codypendentd/src/automation_scheduler.rs`; scheduler tests; modify codypendentd startup/workflows and workspace dependencies for a pinned timezone-aware cron library.

- [ ] Add paused-time tests for cron/one-time occurrences, DST, restart recovery, skip/fire-once/bounded-catch-up misfires, retry, and deterministic next-fire preview.
- [ ] Add concurrent claim tests for allow, skip-while-active, queue, and approved replace behavior.
- [ ] Atomically claim due rows and persist occurrence timestamps before dispatch; use durable binding-level leases, not `DriveLockRegistry`.
- [ ] Keep trigger retry separate from workflow node retry.
- [ ] Commit: `feat(automation): schedule durable workflow invocations`.

### Task 4.4: Add generic signed webhook and internal event adapters

**Files:** Add `crates/integrations/src/webhook/generic.rs`; tests; modify webhook module/server and dispatch. Add internal event adapter modules under daemon/codypendentd.

- [ ] Add failing signature/replay/body-limit/secret-namespace tests and hostile payload tests proving payload cannot select owner/repository/workflow/budget/approval.
- [ ] Normalize generic webhooks, CI failures, repository/codegraph changes, dependency alerts, and manual/API events into one `TriggerEvent` path.
- [ ] Enforce endpoint-specific secrets, verify-before-parse, bounded errors, and common filter/dedup handling.
- [ ] Commit: `feat(automation): normalize signed and internal triggers`.

### Task 4.5: Ship first-party workflow templates

**Files:** Add `docs/specs/workflows/{failing-ci-repair,dependency-update,stale-document-refresh,flaky-test-investigation,repository-health-report,release-preparation}.yaml`; modify `crates/workflow/src/source.rs`; `crates/workflow/tests/spec_it.rs`; compatibly retain `docs/specs/workflow.yaml`.

- [ ] Add catalogue tests for immutable ID/version, compiler/reference validity, source precedence, approval/budget defaults, and executable-tool allowlist.
- [ ] Add each manifest and only production-supported node types.
- [ ] E2E one safe fixture for every template through the trigger service.
- [ ] Commit: `feat(workflow): add automation template catalogue`.

### Task 4.6: Milestone 4 exit gate

- [ ] Partition/restart/replay tests prove at-least-once delivery never duplicates external effects.
- [ ] UI/desktop can create, inspect, pause, resume, preview, and audit bindings/schedules.
- [ ] Run webhook, workflow, scheduler, migration, authorization, and common gates.

---

## Milestone 5 — Secret broker, marketplace, and integration pack

### Task 5.1: Add the secret-reference and lease domain

**Files:** Add `crates/secrets/{Cargo.toml,src/{lib,reference,lease,backend,audit}.rs,tests/broker_it.rs}`; `migrations/0045_secret_broker.sql`; modify root workspace and provider dependencies.

- [ ] Add failing tests for principal/org/repository/job/capability binding, accepted-reference digest, expiry, revocation, idempotent issue, and no secret in Debug/Serialize/audit.
- [ ] Implement `SecretReference`, `LeaseContext`, non-clone/non-serialize leased material, `SecretBackend`, and `SecretBroker` traits.
- [ ] Persist opaque reference metadata, lease state, and append-only non-secret audit.
- [ ] Commit: `feat(secrets): add context-bound secret leases`.

### Task 5.2: Implement initial secret backends and policy gate

**Files:** Add `crates/secrets/src/backends/{mod,environment,keychain,managed,vault,workload_identity}.rs`; tests; `crates/daemon/src/secret_gate.rs`; `crates/codypendentd/src/secrets.rs`; modify policy gate, sandbox gate, providers credential, GitHub/search secret discovery.

- [ ] Add adapter contract tests for resolve-at-use environment refs, platform keychain/unsupported behavior, envelope-encrypted managed values, Vault lease TTL/revoke, and audience-bound workload tokens.
- [ ] Authorize `HostRequest::ReadSecret` only after manifest ceiling and run policy; resolve material at final transport injection.
- [ ] Replace integration-specific plaintext discovery with compatibility references such as `env:GITHUB_TOKEN`.
- [ ] Add compromised-job tests proving no post-acceptance scope widening.
- [ ] Commit: `feat(secrets): broker local and managed credential backends`.

### Task 5.3: Add durable marketplace distribution and compatibility

**Files:** Add `crates/marketplace/{Cargo.toml,src/{lib,catalog,distribution,store,compatibility}.rs,tests/distribution_it.rs}`; `migrations/0046_marketplace.sql`; workspace wiring.

- [ ] Add hostile archive/download tests: size/count/ratio, absolute/parent paths, escaping links, duplicate normalized paths, unexpected executable files, hash/signature mismatch, redirects, and source allowlist.
- [ ] Reuse sandbox manifest/signature/lifecycle and generic safe extraction patterns; persist immutable package/version/hash, publisher, artifact, install, pin, lifecycle, and permission receipt records.
- [ ] Install content-addressed packages disabled; host computes compatibility.
- [ ] Commit: `feat(marketplace): verify and persist package distributions`.

### Task 5.4: Add trust, revocation, updates, protocol, CLI, and UI

**Files:** Add marketplace trust/revocation/update modules/tests; daemon/codypendentd operations; `crates/protocol/src/marketplace.rs`; vectors; CLI commands/tests; desktop shared UI.

- [ ] Add failing tests for discover/inspect/install/pin/check/approve/disable/enable/remove/trust/revocation, permission expansion receipts, org allowlists, and hidden-package non-disclosure.
- [ ] Make publisher/registry trust distinct; revocation disables installed/cached packages and invalidates pending receipts.
- [ ] Keep sandbox `InstalledPlugin` lifecycle as final execution authority.
- [ ] Drive the complete lifecycle from CLI and desktop.
- [ ] Commit: `feat(marketplace): add governed package lifecycle`.

### Task 5.5: Ship the first-party integration pack

**Files:** Add typed modules/tests for `gitlab`, `issues/{linear,jira}`, `chat/{slack,teams}`, generic webhook; manifests under `examples/integration-pack/*/plugin.toml`; modify integration exports and existing GitHub/MCP/ACP/provider constructors.

- [ ] Add mock-server contract tests per service for auth lease injection, bounded responses, retries/rate limits, idempotent external effects, SSRF/redirect controls, and hostile text sanitization.
- [ ] Package GitHub/GitLab, Linear/Jira, Slack/Teams, generic webhooks, OpenAI-compatible providers, MCP, and ACP with least-privilege manifests.
- [ ] Install each through the real marketplace path; smoke-test before explicit enable.
- [ ] Commit service pairs separately, then catalogue: `feat(integrations): ship first-party marketplace pack`.

### Task 5.6: Milestone 5 exit gate

- [ ] Scan logs, DB, events, artifacts, support bundles, and test outputs for sentinel secrets.
- [ ] Exercise publisher-key revocation, permission expansion, compromised plugin, Vault outage, expired token, and offline local environment reference.
- [ ] Run secrets, marketplace, sandbox adversarial, integration, client, migration, deny/audit, and common gates.

---

## Milestone 6 — Cross-repository architecture intelligence

### Task 6.1: Publish policy-approved graph facts

**Files:** Add `crates/federation/{Cargo.toml,src/{lib,identity,publication,authorization,store}.rs,tests/publication_it.rs}`; `migrations/0047_graph_publication.sql`; daemon/codypendentd graph publication modules.

- [ ] Add failing tests for metadata-only default, strictest-source classification, stable repository identities, revision/hash/provenance/policy version, idempotent batches, retraction, and tombstones.
- [ ] Define `PublicationPolicy` and `SharedGraphStore`; publish stable facts, never local row IDs.
- [ ] Keep existing local `CodeGraphQuery` repository-bound and private.
- [ ] Commit: `feat(federation): publish policy-approved graph facts`.

### Task 6.2: Implement access-safe shared traversal and planning

**Files:** Add federation query/blast-radius/migration-plan/ownership modules/tests; `crates/protocol/src/federated_graph.rs`; daemon/codypendentd operations and vectors.

- [ ] Add adversarial tests comparing byte-equivalent absent/inaccessible seed results and proving hidden intermediate nodes cannot affect paths, counts, cursors, or timing class.
- [ ] Apply authorized repository grants at seed selection and every recursive traversal step.
- [ ] Implement cross-repository blast radius, API/schema migration planning, dependency campaign targets, and ownership-aware reviewer suggestions with evidence.
- [ ] Drive queries from CLI/desktop with grant-limited fixtures.
- [ ] Commit: `feat(federation): query shared architecture safely`.

### Task 6.3: Coordinate multi-repository campaigns with separate authority

**Files:** Add `migrations/0048_multi_repo_campaigns.sql`; federation campaign/store modules/tests; protocol campaign; daemon/codypendentd campaign operations; workflow store integration; CLI tests.

- [ ] Add failing tests for one existing workflow child per repository, separate worktrees/budgets/policy/secret leases/approval digests, partial failures, resume, and idempotent retry.
- [ ] Persist campaign/repository/run/approval/effect records and create children through `WorkflowStore::create_run_idempotent_owned`.
- [ ] Allow coordinator aggregation only; prohibit blanket approval and shared credentials/worktrees.
- [ ] E2E a two-repository API migration with one denial and selective retry.
- [ ] Commit: `feat(federation): coordinate repository-scoped campaigns`.

### Task 6.4: Milestone 6 exit gate

- [ ] Run graph publication/traversal inference tests, campaign recovery/effect dedup, protocol vectors, multi-user authorization, and full gates.
- [ ] Confirm no private source/transcript/path/evidence is published under metadata-only policy.

---

## Milestone 7 — Hybrid control plane

### Task 7.1: Add the independent network protocol and generated SDK

**Files:** Add `crates/control-plane-protocol/` modules for version/IDs/page/error/auth/identity/RBAC/repository/publication/sync/audit/artifact/events/runner and tests; `protocol-vectors/control-plane/v1/`; `sdk/control-plane/` generated client/stream/tests; workspace wiring.

- [ ] Add failing golden/schema/compatibility tests for current plus one previous version, opaque pages, idempotency, publication classes, resumable streams, sync deltas/receipts/tombstones/approvals/schedules/runner events.
- [ ] Keep local `Envelope` out of this crate; share only semantically identical value types.
- [ ] Generate TypeScript using Milestone 1 infrastructure.
- [ ] Commit: `feat(control-plane): define versioned network protocol`.

### Task 7.2: Create the single PostgreSQL control-plane service

**Files:** Add `crates/control-plane/{Cargo.toml,src/{lib,main,config,error,http,health,state}.rs,src/store/{mod,postgres}.rs,migrations/0001_identity.sql..0005_audit.sql,tests/{migrations,health}_it.rs}`; workspace dependencies.

- [ ] Add failing health/config/migration tests against PostgreSQL, including upgrade from each control-plane schema fixture.
- [ ] Add Axum/Tower/SQLx PostgreSQL service and configuration-injected identity/object endpoints; no managed/self-hosted domain forks.
- [ ] Separate runtime and migration DB roles; make audit rows append-only by DB permission.
- [ ] Commit: `feat(control-plane): add postgres service foundation`.

### Task 7.3: Implement GitHub OAuth, generic OIDC, identity linking, and workload tokens

**Files:** Add control-plane identity modules and tests.

- [ ] Add failing PKCE/state/nonce/JWKS/issuer/audience/expiry tests, linking proof tests, refresh-token hash tests, and email-collision non-link tests.
- [ ] Implement GitHub OAuth and OIDC discovery; linking requires authenticated proof of both identities.
- [ ] Issue short-lived audience/purpose/org/repository-bound daemon tokens and auditable pairing challenges.
- [ ] Commit: `feat(identity): add github oidc and workload identity`.

### Task 7.4: Enforce RBAC and immutable audit

**Files:** Add `src/authz/*`, `src/audit/*`, matrix/non-disclosure/audit tests.

- [ ] Add exhaustive role/action/scope matrix tests and absent/inaccessible response equivalence tests.
- [ ] Authorize before resource load/count; intersect organization/repository/local authority.
- [ ] Append actor/action/target digest/correlation/hash-chain records transactionally without product secrets/content.
- [ ] Commit: `feat(control-plane): enforce scoped rbac and audit`.

### Task 7.5: Add S3-compatible published-object storage

**Files:** Add control-plane object store trait, S3/memory adapters, manifest, tamper tests.

- [ ] Add failing content-addressed upload, wrong hash/length, bounded range, partial resume, minimum-scope presign, and tombstone tests against memory and MinIO.
- [ ] Keep PostgreSQL authoritative for metadata/policy; object storage owns bytes only.
- [ ] Commit: `feat(control-plane): store verified published objects`.

### Task 7.6: Pair and synchronize outbound local daemons

**Files:** Add `migrations/0049_control_plane_sync.sql`; `crates/daemon/src/control_plane_sync/*`; tests; `crates/codypendentd/src/control_plane.rs`; startup wiring.

- [ ] Add failing tests for explicit pairing display/consent, revocation, durable outbox, duplicate deltas/receipts/approvals, resumable cursor, queued shared operation, and local operation with no config/outage.
- [ ] Add offline-delete/reconnect test proving tombstones are consumed before new publication.
- [ ] Reuse knowledge outbox patterns but keep dedicated sync tables and encrypted credential references.
- [ ] Initiate outbound authenticated sync only; no workstation inbound port.
- [ ] Commit: `feat(daemon): synchronize with an optional control plane`.

### Task 7.7: Expose idempotent REST resources and resumable streams

**Files:** Add control-plane API modules for organizations/repositories/sessions/inbox/approvals/artifacts/analytics/automation/marketplace/daemons/events and idempotency tests.

- [ ] Add failing tests that every mutation atomically stores receipt + domain effect and duplicate keys return the original response.
- [ ] Add stream persist-before-publish/resume/reconnect/authorization tests.
- [ ] Expose only shared classes and bounded queries; browser API cannot request unpublished local content.
- [ ] Commit: `feat(control-plane): expose shared resources and event streams`.

### Task 7.8: Build the browser client from shared UI

**Files:** Add `sdk/control-plane-react/`; `apps/web/` routes/features/tests for sessions/inbox/approvals/analytics/marketplace/automation/artifacts; modify shared UI only for genuinely reusable views.

- [ ] Add failing auth callback, stream resume, authorization, deep-link, loading/error/empty, and accessibility tests.
- [ ] Consume generated SDK and `@codypendent/ui`; do not import desktop internals.
- [ ] Show classification/publication status and never imply local content access.
- [ ] Commit: `feat(web): add shared team workspace client`.

### Task 7.9: Ship self-host deployment and observability

**Files:** Add control-plane Dockerfile/Compose/Helm/Kustomize; `docs/self-hosting/{control-plane,backup-restore}.md`; OTEL setup/health/metrics.

- [ ] Test container image as non-root, migration init, readiness/liveness, config validation, backup/restore, and upgrade/rollback against PostgreSQL + MinIO + test OIDC.
- [ ] Add structured correlation across browser/control-plane/daemon without publishing private telemetry by default.
- [ ] Lint/render all deployment manifests.
- [ ] Commit: `feat(deploy): package self-hosted control plane`.

### Task 7.10: Milestone 7 e2e gate

- [ ] GitHub and OIDC login → repository grant → daemon pair → metadata-only sync → browser session/inbox/approval → revoke daemon.
- [ ] Disconnect control plane and prove local CLI/TUI/desktop/VS Code remain functional.
- [ ] Run PostgreSQL/MinIO, protocol compatibility, RBAC non-disclosure, sync partition, web accessibility, deployment, and common gates.

---

## Milestone 8 — Self-hosted remote runners

### Task 8.1: Complete deployment-neutral runner contracts

**Files:** Expand control-plane protocol runner/artifact modules, vectors, and generated SDK.

- [ ] Add golden/canonical tests for capabilities/eligibility, job/input/sandbox/resource specs, claim/lease/release/cancel/log/output/attestation.
- [ ] Bind canonical attestation bytes to job, attempt, lease, runner/image, input/output hashes, timestamps, and result.
- [ ] Commit: `feat(runner): define deployment-neutral execution protocol`.

### Task 8.2: Persist scheduler jobs, attempts, leases, and attestations

**Files:** Add control-plane migrations `0006_runner_jobs.sql`, `0007_runner_leases.sql`, `0008_runner_attestations.sql`; scheduler modules/API/tests.

- [ ] Add concurrent PostgreSQL tests for `SKIP LOCKED` claims, matching-generation renew, expiry/retry, cancel/complete CAS, duplicate submission, and one accepted attempt.
- [ ] Implement eligibility by policy/capability/residency/budget/region and durable terminal state.
- [ ] Commit: `feat(control-plane): schedule leased runner jobs`.

### Task 8.3: Build runner core and hostile input materialization

**Files:** Add `crates/runner/` client/identity/claim/materialize/execute/log/upload/attest/cleanup and tests.

- [ ] Add archive tests for absolute/parent path, symlink/hardlink escape, duplicate conflict, expansion/size limits, undeclared/wrong hashes.
- [ ] Add claim-loop partition tests and compromised runner tests requesting another job/repository/secret.
- [ ] Materialize content-addressed inputs, execute through an injected backend, stream bounded logs, upload complete outputs, sign attestation, and clean every workspace.
- [ ] Commit: `feat(runner): execute leased jobs securely`.

### Task 8.4: Add hardened container execution

**Files:** Add runner backend trait/container/process, runner Docker/Compose, container/network tests.

- [ ] Add failing tests for non-root, read-only root, dropped capabilities, workspace-only mounts, CPU/memory/PID/output bounds, cancellation, and deny-by-default network.
- [ ] Translate `SandboxSpec` into enforceable container controls; refuse unsupported grants rather than silently weakening them.
- [ ] Run the existing Codypendent runtime inside disposable job environments.
- [ ] Commit: `feat(runner): add hardened container backend`.

### Task 8.5: Verify artifacts, partial uploads, and attestations

**Files:** Add scheduler artifact/attestation modules and control-plane/runner tests.

- [ ] Add failing tests for partial upload retry, bad hash/unknown signer/revoked image or key/wrong lease-attempt/malformed attestation, and quarantine.
- [ ] Keep jobs `uploading` until every output hash verifies; only accepted attempt artifacts may continue workflows.
- [ ] Commit: `feat(runner): verify outputs and execution attestations`.

### Task 8.6: Dispatch existing workflow nodes remotely

**Files:** Add `crates/codypendentd/src/remote_node_executor.rs`; codypendentd/control-plane dispatch tests.

- [ ] Add failing e2e tests that policy-selected nodes submit jobs and map accepted outputs to `NodeOutcome` while local nodes still use `AgentLoopNodeExecutor`.
- [ ] Preserve node attempts, costs, attribution, retry, observer transitions, and workflow budget; do not alter `WorkflowDriver` semantics.
- [ ] Test daemon disconnect while remote work finishes and result sync on reconnect.
- [ ] Commit: `feat(workflow): dispatch eligible nodes to remote runners`.

### Task 8.7: Add Kubernetes runner controller

**Files:** Add `crates/runner-controller/`; Helm deployment; pod/controller tests.

- [ ] Add pod-spec tests for one hardened Job per attempt, scoped credentials, resource/security context, no broad service account, and terminal/expired cleanup.
- [ ] Keep PostgreSQL scheduler/lease truth authoritative, not Kubernetes annotations.
- [ ] Contract-test container and Kubernetes paths against the same job vectors.
- [ ] Commit: `feat(runner): add kubernetes job controller`.

### Task 8.8: Milestone 8 e2e gate

- [ ] Browser starts shared workflow → container runner claims → verified artifacts continue workflow.
- [ ] Kill Kubernetes runner → lease expires → retry → exactly one output set accepted.
- [ ] Partition at claim/execution/upload/completion and replay cancel/renew/complete messages.
- [ ] Run runner/scheduler/protocol/security/deployment and all common gates.

---

## Milestone 9 — Managed execution and continuous quality

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
- [ ] Deprecate/reject production caller-supplied `CanaryMetrics`; retain wire compatibility until a major protocol release.
- [ ] Commit: `feat(quality): evaluate and roll back canary routes`.

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
