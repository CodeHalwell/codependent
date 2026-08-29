# Codypendent massive review — 2026-08-29

## Executive result

This review covered the Rust workspace, runner and sandbox trust boundary, daemon
recovery and control-plane synchronization, the control plane itself, desktop, web,
SDKs, TUI/accessibility, and CI/release supply chain. Eleven bounded specialist review
and implementation assignments were run in waves, followed by independent integration,
red-team, and release validation.

All concrete P0/P1 defects accepted during this review have either been fixed with
regression coverage or failed closed where the server capability does not yet exist.
The result is a substantial hardening pass rather than a cosmetic cleanup: remote jobs
can no longer choose their own host-access ceiling, container/process cancellation now
reaps the real workload, paused writing runs recover without repeating durable effects,
control-plane pairing and refresh rotation are atomic, WebSocket credentials are
one-time scoped tickets, clients no longer invent unsupported server APIs, and release
gates cover both Rust workspaces plus generated browser contracts.

The remaining items are explicitly recorded below. They are architecture or product
capability work, not known regressions hidden behind a green test suite.

## Scope and method

- 550 Rust source files and 340 TypeScript/TSX source files were in scope.
- Eleven specialist assignments covered backend architecture, runner security,
  control-plane red teaming, SDK contract alignment, frontend/TUI behavior,
  browser-build hygiene, workspace integration, and release validation.
- Existing dirty changes were preserved and reviewed in place. No unrelated user work
  was discarded or reset.
- Findings needed a concrete failure scenario, an implementable remediation, and a
  regression-test shape before they were accepted.
- Final checks were run from the resulting combined working tree, including live
  PostgreSQL pairing/concurrency tests and both Cargo lockfiles.

## Upgrades implemented

### Runner and host boundary

1. **Local policy is now the immutable ceiling.** A claimed job may narrow configured
   mounts, environment names, working directory, resources, and data classification,
   but cannot grant itself new host paths or secrets. Job specs and claims are bound by
   deterministic hashes.
2. **Container lifecycle is tied to the real container.** The full runtime argv is
   passed, a stable cidfile identifies the workload, and success, failure, timeout, and
   cancellation paths kill and reap it. Combined stdout/stderr is capped while streaming.
3. **Process cancellation reaches the workload.** The process backend polls host
   cancellation, terminates the process group, and waits for it instead of abandoning a
   child. Resource-byte arithmetic now rejects overflow.
4. **Artifact handling is durable and bounded.** Output declarations are confined below
   the attempt workspace using descriptor-relative, no-follow traversal; required output
   and upload failures fail the result. Upload progress is journaled for retry without
   re-running a successful non-idempotent job.
5. **Cleanup is symlink-safe and fail-closed.** Mode-000 trees can be repaired without
   following links, redirected roots are rejected, failed secure deletion quarantines
   the attempt, and teardown failure prevents a normal lease release.
6. **Attestations bind the actual execution.** Canonical signed envelopes cover the
   validated job input, policy, outputs, and signer. Claims use absolute expiry,
   bounded renewal/finalization, and cancellation on failed or non-monotonic renewal.

### Daemon recovery, provenance, and synchronization

7. **Writing checkpoints are mandatory.** Launch and turn checkpoints must exist before
   destructive work begins; failure is propagated instead of downgraded to a warning.
8. **Paused runs resume exactly once.** Recovery reserves ownership before spawning,
   retains cancellation/approval state, reattaches the current worktree lease, restores
   same-run durable turns, and fails closed if safe replay cannot be proven.
9. **Terminal and start races use compare-and-set semantics.** Late executor failures can
   no longer overwrite pause, cancellation, completion, or another terminal winner. A
   state- and event-guarded cancellation watchdog supplies missing `RunCompleted`
   evidence when assembly startup has not reached the cancellation-aware loop, and
   council cleanup polls the close barrier on one connection with write-free backoff
   instead of reconnecting and re-archiving in a retry storm.
10. **Repository/model provenance is sequence-bound.** Re-drive resolves the originating
    durable event rather than accepting the session's newest pin, and missing/corrupt
    provenance never falls back to the daemon working directory.
11. **Worktree release preserves untracked data.** A forced release is refused if the
    safety patch cannot be prepared.
12. **Control-plane sync is a real startup service.** The daemon uses generated
    `SyncEnvelope`/`SyncBatchResponse` contracts, repository-scoped cursors, recorded
    per-pairing backoff, bounded provider bodies/timeouts, and owner-only OS credential
    storage. A real TCP end-to-end test covers daemon/control-plane exchange.

### Control-plane authentication, pairing, storage, and events

13. **Credentials use OS-CSPRNG 256-bit secret material.** Refresh, pairing, daemon, and
    WebSocket credentials are no longer UUID-shaped bearer secrets. JWT verification
    pins HS256, issuer, audience, expiry, issued-at bounds, and maximum lifetime.
14. **Refresh rotation is atomic.** Compare-and-set consumption permits only one child;
    replay revokes the stolen descendant family rather than every session belonging to
    the user.
15. **Authorization is live and purpose-aware.** Active user, membership, organization,
    credential audience, and explicit route purpose are checked rather than trusting
    stale token claims.
16. **Pairing completion is transactional.** Challenge validation, row locks, daemon
    creation, credential creation, scope validation, consumption, and rollback occur in
    one store operation in both memory and PostgreSQL. Concurrent live-PostgreSQL tests
    cover the race.
17. **Unverified account linking is unavailable.** `/v1/auth/link` returns a clear 501
    until a verified identity-provider flow exists; it no longer accepts identity claims
    supplied by the caller.
18. **Object access is metadata-authorized.** Arbitrary direct PUT is disabled. Upload
    policy is rechecked against live organization and daemon ceilings before storage,
    GET keys are derived only from validated metadata, and failed authorization writes
    no object.
19. **WebSocket replay closes the race.** Subscribers register before replay, paginate
    the complete repository-scoped backlog, deduplicate event IDs, and connect with a
    short-lived, one-use, scope-bound ticket rather than a reusable credential in the URL.
20. **Sync payload integrity is verified.** The server recomputes the declared payload
    hash and the generated stream protocol carries redacted sync-delta and artifact
    summary shapes.

### Desktop, web, SDKs, and TUI

21. **Desktop attachment is generation-correlated.** The old session stays authoritative
    until the new attach succeeds; attach-time frames are buffered; stale catch-up is
    rejected; snapshots establish sequence watermarks; and gap repair is session-bound
    across reconnects.
22. **Web access and streams are lifecycle-safe.** Protected routes have an authenticated
    guard, stale async responses cannot replace newer state, and multiple React consumers
    share one multicast wire subscription with reference-counted teardown.
23. **The control-plane SDK matches the Axum router.** Implemented calls use exact
    generated request/response adapters and route-contract tests anchored to router
    source. Methods without a server route preserve API compatibility but reject locally
    with `UnsupportedControlPlaneCapabilityError` instead of making phantom requests.
24. **TUI network work no longer blocks input.** New/switch/fork/reconnect preparation is
    asynchronous, superseded work is aborted and generation-rejected, deferred input is
    bounded FIFO, reconnect catch-up is deduplicated, and dropped I/O aborts reader and
    writer tasks. Accessible mode now has command parity, incremental streaming output,
    cold-blackboard loading, and visible writer failure.
25. **Browser builds are native-ESM clean.** The protocol client's Node crypto fallback
    is isolated from browser bundling, while desktop Vite/Vitest paths use
    `import.meta.url` instead of `__dirname`.

### CI and release supply chain

26. **Release actions are immutable and least-privilege.** Every action is pinned to a
    full commit SHA, non-publish checkouts disable persisted credentials, permissions
    default to read-only, and only publishing receives `contents: write`.
27. **Both Rust workspaces are release gates.** Root and Tauri lockfiles receive format,
    compile, strict Clippy, tests, advisory/license/source policy, and exact-version tag
    checks. The PostgreSQL service image is digest-pinned.
28. **Generated and migration state is enforced.** CI/release check both generated
    protocol families and immutable checksum manifests for the 50 root plus 10
    control-plane migrations. Shallow clones fail closed if migration ancestry is
    unavailable; a regression test covers depth-one SQL and manifest tampering.
29. **Workflow invariants are executable.** A structural checker prevents action tag
    drift, mutable service images, credential persistence, excessive permissions, missing
    fetch depth, and omitted release gates.

## Remaining architectural limitations

These are deliberate follow-ups, not claims that the affected capability is complete:

- Runner images can still use a mutable default tag and attestations may lack an image
  digest. Lease renewal plus terminal submission are not one atomic server operation.
  A deliberately permitted child could create a new process group, and durable upload
  continuation depends on the server reissuing the same job/attempt identity.
- Pairing repository allowlists are captured on the challenge but are not yet persisted
  and enforced in daemon authorization. The consent-manifest hash is not revalidated on
  reconnect.
- WebSocket tickets and broadcasts are process-local; multi-instance deployment needs a
  shared ticket registry/event bus or enforced instance affinity.
- Several control-plane authorization-check/mutation and resource/audit pairs are not a
  single database transaction. Full hardening needs transactional store operations or
  row-level security plus complete audit coverage.
- JWT signing still uses one global HS256 key without `kid`-based rotation. Interactive
  login/account linking remains intentionally unavailable until a verified provider flow
  is implemented.
- The server does not yet implement teams/roles, organization or repository
  update/delete, session detail/filter/cursor, inbox/approvals, member listing/invites,
  API-key management, or daemon-management APIs. SDK calls for these fail locally and
  explicitly.
- Aborting a superseded TUI new/fork request can leave an unused durable server session
  because the protocol has no cancellation endpoint; generation checks prevent it from
  becoming active locally.
- The separate Tauri build observes six non-fatal dead-code warnings in its path
  dependency on `codypendent-runtime`; strict Clippy still passes because dependency
  warnings are cap-linted.

## Validation results

- Root workspace format, both generated-protocol checks, strict all-target/all-feature
  Clippy, and the full all-feature workspace suite passed: 3,991 tests passed, 9
  expected tests were ignored, and 0 failed.
- Runner: 58/58 tests; sandbox: 186/186; daemon library: 294/294; worktree: 29/29;
  recovery: 2/2; strict touched-crate Clippy passed.
- Control plane: 117/117 crate tests plus 9 live PostgreSQL pairing/concurrency tests;
  strict Clippy passed.
- Desktop: TypeScript check/build and 158/158 Vitest tests. Control-plane SDK: 41/41;
  React SDK: 7/7; web: 15/15; protocol SDK: 460/460.
- TUI: 727 unit plus 5 terminal-emulation tests. Its custom `overlay_cpu` benchmark
  requires benchmark arguments and is not part of the canonical test gate.
- Separate Tauri workspace: format, locked all-target check, strict Clippy, 33/33 unit
  tests, and doc tests passed.
- Both workflow YAML files parsed; workflow-security assertions, migration checker tests,
  root/control-plane manifests, both Cargo-deny scopes, and `git diff --check` passed.
  Cargo-deny reported allowed duplicate-version groups and no policy failure.

## Build-cache cleanup

Generated Rust artifacts, not project data, caused the disk pressure. Earlier cleanup
passes removed 369.9 GiB from the root workspace, 43.9 GiB from the separate Tauri
workspace, and 19.2 GiB recreated during intermediate validation: 433.0 GiB at that
stage. Final validation deliberately rebuilt another 39 GiB root target and 6.8 GiB
Tauri target; both were purged after every gate completed. Across the review that is
approximately 479 GiB of compiled artifacts removed. A final search found no `target`
directory anywhere below the repository, and the volume reported 310 GiB free. No
source, lockfile, Cargo registry cache, or user data was removed.
