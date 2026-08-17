# Milestone 2 — Session Library, bundles, and real local clients

**Audience:** the implementer of the *remaining* Milestone 2 tasks of
[`plans/2026-08-16-hybrid-platform-program.md`](../plans/2026-08-16-hybrid-platform-program.md)
(§201–283).
**Prerequisite:** read [`00-conventions-and-traps.md`](00-conventions-and-traps.md) first. This
guide does not repeat it.
**Status:** verified against the tree on 2026-08-17 (`release/v0.9.0` @ `b8e17bd`).

The plan gives you the file lists, the failing-test-first steps, and the commit messages. This
guide gives you the four things it does not: **what is already built**, the **column-level DDL**
for `0041_session_bundles.sql`, the **reserved-contract → implementation → authorization** map,
and the traps that are specific to these tasks.

---

## 1. Status — what is done, what remains

Re-verify with `git status --short` before you start; part of this milestone is sitting
uncommitted in the working tree.

| Task | Status | Evidence |
|---|---|---|
| **2.1** ranked Session Library | ✅ **committed** | `crates/daemon/src/session_library.rs` (1189 L), `migrations/0040_session_library.sql`, `crates/daemon/tests/session_library_it.rs` (660 L), socket coverage `crates/daemon/tests/server_it.rs:356`. Commits `2ef88cb`, `d60efef` |
| **2.2** archive internal sessions | 🟡 **built but UNCOMMITTED, one file short** | `crates/codypendentd/src/workflow_exec.rs:1214`, `crates/codypendentd/src/workflows.rs:402` — both `M` in `git status`. `crates/council/src/service.rs` and `crates/codypendentd/src/executor.rs` have **zero** `internal`/`archived_at` references despite being named in the task |
| **2.3** shared TS transport | 🟡 **partial — half the module, zero consumers** | `sdk/protocol/src/framing.ts` (87 L) and `src/session-store.ts` (133 L) exist. **`src/client.ts` does not.** Nothing in the repo imports `@codypendent/protocol`; `extensions/vscode/src/protocol/frame.ts` (105 L) is still the live duplicate, imported at `extensions/vscode/src/client.ts:21` |
| **2.4** Tauri host + desktop projection | 🟡 **partial — real transport, wrong deps** | `apps/desktop/src-tauri/{Cargo.toml,tauri.conf.json,src/main.rs,src/bridge.rs,src/daemon.rs}` exist and speak to a **real** daemon via `codypendent_council::connection::Connection` (`src-tauri/src/daemon.rs:21`). Not done: React upgrade, removal of the hand-mirrored protocol types in `apps/desktop/src/transport.ts:17-84` |
| **2.5** shared Remote UI renderer | ⬜ **not started** | No `sdk/ui/src/host-react/`. The shared base is `sdk/ui/src/react/{index,primitives,provider,renderer}.ts`; VS Code's `src/webview/remote-ui/*` are still host-local duplicates |
| **2.6** VS Code history + editor actions | ⬜ **not started** | No `extensions/vscode/src/editor-actions.ts`. `package.json` contributes only `codypendent.{openSession,approve,startRun}` and has **no `contributes.menus` key** |
| **2.7** session/support bundles | ⬜ **not started** | `crates/protocol/src/bundle.rs` (300 L) is contract-only. No `crates/daemon/src/bundles.rs`, no `migrations/0041_*`, no CLI bundle command |
| **2.8** M2 e2e gate | ⬜ **not started** | — |

**Untracked work in the tree** — do not delete, do not assume it is wired:

- `extensions/vscode/src/webview/session-library-view.tsx` (165 L) — a `SessionLibraryView`
  component plus a `mergeSessionSearchPage` cursor-merge helper.
- `extensions/vscode/test/session-library-view.test.tsx` (135 L) — renders it directly.
- Neither is imported by `panel.ts`, `extension.ts`, `webview/messages.ts`, or the esbuild config,
  and no command or view contribution surfaces it. It overlaps Task 2.6; wire it or replace it,
  but decide deliberately.

**Suggested order.** 2.2 (commit the finished work + close the council gap) → 2.3 (`client.ts`, the
blocker for both clients) → 2.4 (React 19 + consume `@codypendent/protocol`) → 2.5 → 2.6 → 2.7 →
2.8.

---

## 2. Task 2.2 — the part that is missing

The workflow half is done. `crates/codypendentd/src/workflow_exec.rs:1214-1226` marks
`internal = 1` with `parent_run_id`/`parent_session_id` immediately after `ledger::create_session`,
and `crates/codypendentd/src/workflows.rs:402-418` archives with
`archived_at = COALESCE(archived_at, ?)` only when the child run is terminal **and** the parent
`workflow_runs` row is `completed|failed|cancelled`, called from all four terminal paths including
crash recovery (`workflows.rs:241-254`). Default-omission already works:
`session_library.rs:581` appends `AND s.internal = 0 AND s.archived_at IS NULL` when the query
string is empty.

What remains:

1. **Commit it.** No `feat(session): archive completed internal work` commit exists.
2. **Council child sessions.** `crates/council/src/service.rs:161-184` issues `CreateSession` for
   each council member and closes them at `:230` with `CommandBody::CloseSession`. They are never
   marked internal, so every council run permanently pollutes the user's Session Library with
   N extra sessions. Apply the same `UPDATE sessions SET internal = 1, parent_session_id = ?,
   parent_run_id = ?` immediately after creation, in the same transaction, and archive on the
   parent's terminal persistence — not on close.
3. `CloseSession` is not archival. `state = 'closed'` and `archived_at` are independent columns;
   the library filter reads `archived_at`, so closing alone changes nothing.

---

## 3. Task 2.3 — what `sdk/protocol/src/client.ts` must contain

`framing.ts` gives you `encodeEnvelope`, `FrameDecoder`, `MAX_FRAME_BYTES`, `FrameError`.
`session-store.ts` gives you `SessionStore` with `applyEvent`/`applyCatchup`, live-event buffering
until catch-up arrives (`session-store.ts:52-56`), watermark validation (`:76-80`, `:100-102`), and
`SessionSequenceGapError`. Neither contains a socket, a handshake, request correlation, ping/pong,
a resume token, an offline queue, or reconnect backoff — those are `client.ts`, and they are all
listed in plan §227.

Non-negotiables:

- **Inject the transport.** `client.ts` must take a duplex byte-stream factory, not open a socket.
  Node (`net.connect`), Tauri (`invoke` + `Channel`, see `apps/desktop/src/transport.ts:14`), and
  tests all supply their own. This is plan §229 and it is what makes 2.4 possible at all.
- **Wire semantics do not change.** Port `extensions/vscode/src/client.ts` behaviour verbatim; do
  not "improve" the handshake or correlation while moving it.
- **Delete the duplicate only after both suites pass** (plan §230). `extensions/vscode/src/
  protocol/frame.ts` goes away, and `extensions/vscode/src/client.ts:21` imports from
  `@codypendent/protocol`.
- `extensions/vscode/package.json` currently has **no `dependencies` block at all**. Adding one,
  plus the esbuild external/bundle decision, is part of this task — as is adding
  `sdk/protocol/package-lock.json` to the `cache-dependency-path` list in
  `.github/workflows/ci.yml` (it lists only `sdk/ui` and `extensions/vscode` today).

**Cursor semantics you must respect in the client.** `SessionSearchPage.next_cursor` is bound to a
hash of `(principal_uid, query.query, query.filters)` (`session_library.rs:1049-1091`). Changing a
filter mid-scroll and re-sending the old cursor returns
`session-library.invalid-cursor` — not silently re-ranked results. The client must reset paging
state on any filter change.

---

## 4. Task 2.4 — the two blocking facts

1. **`apps/desktop/src-tauri/Cargo.toml` declares its own `[workspace]`.** It is deliberately
   excluded from the root workspace, so `cargo test --workspace`, `cargo clippy --workspace
   --all-targets`, and `cargo deny check` **do not cover it**. `tauri build` on macOS CI (plan
   §241) is the only gate it has. Add its own `cargo clippy`/`cargo test` step or it ships
   unchecked.
2. **React version mismatch.** `apps/desktop/package.json` pins `react`/`react-dom` `^18.3.1` and
   `@types/react` `^18.3.3`. `sdk/ui/package.json` `peerDependencies` demands
   `react >=19.0.0 <19.1.0` **and** `react-reconciler >=0.31.0 <0.32.0`. Desktop already depends on
   `@codypendent/ui` via `file:../../sdk/ui`. The upgrade is a prerequisite for Task 2.5, not an
   optional bullet.

`apps/desktop/src/transport.ts:17-84` hand-declares `ConnectionInfo`, `SessionRow`, `RunHandle`,
`SessionEventFrame`, `PendingApprovalSnapshot`, `SessionProjectionSnapshot`, `CatchupSnapshot`,
`DaemonFrame`. Deleting these in favour of `@codypendent/protocol` is the "remove manually mirrored
protocol types" bullet (plan §239) and it is the whole point of 2.3 landing first.

`apps/desktop/src/App.tsx:22-24` hardcodes `const documents = new Map()` — the Remote UI panel is
permanently empty by construction, and `src/components/RemoteUiRenderer.tsx:38-46` renders only a
metadata card (title, revision, protocol version, root id/kind). That is what Task 2.5 replaces.

The plan names `src/daemon/{transport,projection,commands}.ts` and `src/hooks/useDaemonSession.ts`;
the equivalent logic already lives flat in `src/transport.ts`, `src/daemonState.ts` (418 L, a pure
reducer over durable events), and `src/useDaemon.ts`. Moving files is optional; replacing the
mirrored types is not.

---

## 5. Migration `0041_session_bundles.sql` — column-level model

Migrations are **append-only and checksum-gated**. Write the file, then
`python3 .github/scripts/check_migration_immutability.py --update` and commit
`migrations/checksums.json` with it. `--update` records only strictly-higher numbers and still
rejects any change to a historical file, so it cannot bless drift. 0040 is the highest today; 0041
is yours and 0042/0043 belong to M3 — do not take them.

Conventions this follows, from `0040_session_library.sql` and its neighbours: TEXT UUID primary
keys, TEXT RFC3339 timestamps, `INTEGER` owner uid, `CHECK` constraints instead of application-only
enums, `idx_<table>_<purpose>` index names, and a comment on every non-obvious column explaining
*why*.

```sql
-- Milestone 2 Task 2.7: durable receipts for versioned, redacted session and
-- support bundles. The archive BYTES live in `artifacts` (content-addressed);
-- these tables are the auditable record of what was included, what was
-- redacted, and — on import — which local identities were minted.
--
-- Nothing here can restore a credential or an approval: see the module
-- contract in `crates/protocol/src/bundle.rs:1-6` and its
-- `contract_has_no_credential_restoration_field` test.

-- One row per completed export.
CREATE TABLE session_bundles (
    -- Daemon-minted. Not on the wire: `BundleExportReceipt` identifies a bundle
    -- by its artifact, so this id is internal join-key only.
    id TEXT PRIMARY KEY,
    -- The exporting principal's kernel-derived uid. NOT NULL, unlike 0031/0039:
    -- there are no pre-existing rows to adopt, so a NULL here would be a bug
    -- rather than history and must fail at write time.
    owner_uid INTEGER NOT NULL,
    -- `BundleExportReceipt.bundle` — the archive blob. The artifact row carries
    -- its own `owner_uid` (0039); set it to the same principal or the exporter
    -- cannot `ReadArtifact` its own bundle back.
    artifact_id TEXT NOT NULL REFERENCES artifacts(id),
    -- `BUNDLE_FORMAT_V1` at write time. Stored (not assumed) so a future
    -- importer can refuse a version it does not understand without guessing.
    format_version INTEGER NOT NULL,
    -- `BundleManifest.manifest_sha256`, lowercase hex SHA-256 of the canonical
    -- entry manifest. 64 chars; the CHECK is cheap and catches a truncated
    -- hash at insert instead of at verification time.
    manifest_sha256 TEXT NOT NULL CHECK (length(manifest_sha256) = 64),
    -- Serialized `BundleInclusionPolicy` verbatim. Stored as the policy that
    -- was APPLIED, not re-derived later: every switch defaults to false, so a
    -- newer exporter adding a category must not retroactively widen this row.
    inclusion_json TEXT NOT NULL,
    -- `BundleRedactionPolicy`. `Unknown` is deliberately absent from the CHECK:
    -- the contract says receivers must not treat it as less restrictive than
    -- `Standard`, so persisting it would let a later re-export claim a policy
    -- this daemon never applied. Reject it at the command boundary instead.
    redaction_policy TEXT NOT NULL
        CHECK (redaction_policy IN ('Standard', 'SupportSafe')),
    -- `BundleRedactionSummary` counters. The audit answer to "what was removed";
    -- kept even when every counter is zero, because a measured zero and an
    -- unrecorded export are different facts.
    redaction_summary_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    -- The durable command that produced this bundle, so a replayed
    -- `ExportBundle` can return the prior receipt instead of re-archiving.
    command_id TEXT REFERENCES commands(id)
);

-- One artifact is one bundle: a second row against the same blob would make
-- "which manifest describes these bytes" ambiguous.
CREATE UNIQUE INDEX idx_session_bundles_artifact
    ON session_bundles (artifact_id);
CREATE INDEX idx_session_bundles_owner
    ON session_bundles (owner_uid, created_at DESC, id);

-- The (bundle, source session) pairs behind `BundleManifest.source_session_ids`.
CREATE TABLE session_bundle_sources (
    bundle_id TEXT NOT NULL REFERENCES session_bundles(id),
    session_id TEXT NOT NULL REFERENCES sessions(id),
    -- Preserves the request's session order. The manifest hash is computed over
    -- a canonical ordering, so losing order would make a re-export of the same
    -- input produce a different `manifest_sha256`.
    ordinal INTEGER NOT NULL,
    PRIMARY KEY (bundle_id, session_id)
);

-- Ownership is joined through `sessions`; it is intentionally not duplicated
-- here where it could drift (same rationale as `session_search_sources`, 0040).
CREATE INDEX idx_session_bundle_sources_session
    ON session_bundle_sources (session_id);

-- One row per `BundleEntryManifest`.
CREATE TABLE session_bundle_entries (
    bundle_id TEXT NOT NULL REFERENCES session_bundles(id),
    -- Normalized relative archive path. Part of the primary key, which is what
    -- makes the "duplicate paths" hostile-import case (plan §270) a constraint
    -- violation rather than a hand-rolled check somebody can forget.
    path TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN (
        'TranscriptEvents', 'RoutingMetadata', 'Approvals',
        'ArtifactManifest', 'Patch', 'EnvironmentDiagnostics'
    )),
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
    byte_length INTEGER NOT NULL CHECK (byte_length >= 0),
    media_type TEXT NOT NULL,
    -- `DataClassification`. An importer may only ever RAISE this above the
    -- operator ceiling, never lower it (conventions §8) — so the exported value
    -- is a floor, not a fact to be re-derived.
    classification TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    PRIMARY KEY (bundle_id, path)
);

CREATE INDEX idx_session_bundle_entries_hash
    ON session_bundle_entries (sha256);

-- One row per completed import.
CREATE TABLE session_bundle_imports (
    id TEXT PRIMARY KEY,
    owner_uid INTEGER NOT NULL,
    -- `BundleImportRequest.bundle` as uploaded. The ownership gate resolves this
    -- through `NamedResource::Artifact` before the handler runs.
    bundle_artifact_id TEXT NOT NULL REFERENCES artifacts(id),
    -- SHA-256 of the bytes this daemon ACTUALLY read and hashed — never the
    -- `ArtifactRef.sha256` the client asserted. The two disagreeing is the
    -- "hash mismatch" hostile case.
    bundle_sha256 TEXT NOT NULL CHECK (length(bundle_sha256) = 64),
    -- The hash asserted by the manifest inside the verified archive.
    manifest_sha256 TEXT NOT NULL CHECK (length(manifest_sha256) = 64),
    format_version INTEGER NOT NULL,
    collision_policy TEXT NOT NULL
        CHECK (collision_policy IN ('Reject', 'Remap', 'Skip')),
    imported_at TEXT NOT NULL,
    -- `BundleImportReceipt.skipped_entries`. Reported, never inferred.
    skipped_entries INTEGER NOT NULL DEFAULT 0 CHECK (skipped_entries >= 0),
    command_id TEXT REFERENCES commands(id)
);

-- Deliberately NOT unique on (owner_uid, bundle_sha256): under `Remap` a second
-- import of the same archive is a legitimate request for a second copy.
-- Idempotency belongs to `commands.idempotency_key` (UNIQUE, 0002), which is
-- what a retried delivery replays against.
CREATE INDEX idx_session_bundle_imports_owner
    ON session_bundle_imports (owner_uid, imported_at DESC, id);
CREATE INDEX idx_session_bundle_imports_hash
    ON session_bundle_imports (bundle_sha256);

-- `BundleIdentityMapping`: source identity -> freshly minted local identity.
CREATE TABLE session_bundle_identity_map (
    import_id TEXT NOT NULL REFERENCES session_bundle_imports(id),
    kind TEXT NOT NULL CHECK (kind IN (
        'Session', 'Run', 'Artifact', 'Approval', 'ChangeSet'
    )),
    -- Opaque. The exporting daemon's id, retained as provenance ONLY. It is
    -- never used as a local key: reusing it is how an import would resurrect a
    -- tombstoned session (`sessions.tombstoned_at`, 0040).
    source_id TEXT NOT NULL,
    -- The local id this import minted. An 'Approval' row records that a source
    -- approval was SEEN AND DROPPED — approvals are never restored — so
    -- `local_id` may be empty for that kind, and there is deliberately no
    -- foreign key (the column spans five different tables).
    local_id TEXT NOT NULL,
    PRIMARY KEY (import_id, kind, source_id)
);

CREATE INDEX idx_session_bundle_identity_local
    ON session_bundle_identity_map (kind, local_id);

-- Provenance label on every session an import created, so the Session Library
-- and any later publication decision can tell imported content from local
-- content without a join through five tables. NULL = created locally.
ALTER TABLE sessions ADD COLUMN imported_from_bundle TEXT
    REFERENCES session_bundle_imports(id);

CREATE INDEX idx_sessions_imported_bundle
    ON sessions (imported_from_bundle) WHERE imported_from_bundle IS NOT NULL;
```

`BundleImportProvenance` is not a table: its four fields (`bundle_sha256`, `manifest_sha256`,
`imported_at`, `source_session_ids`) are all derivable from `session_bundle_imports` joined to the
manifest, so materialising it a second time would let the two drift.

---

## 6. Contract → implementation → authorization map

`crates/daemon/src/commands.rs:2971-2986` (`is_reserved_unsupported_command`) makes all of these
answer `protocol.unsupported-payload` today. **Removing a variant from that list is the "you are
done" signal for that contract.** M2 removes four of the nine.

| Reserved contract | Daemon implementation | Handler site | `role_permits` (`commands.rs:3001`) | `named_resources()` (`command.rs:985`) |
|---|---|---|---|---|
| `CommandBody::MutateSessionLifecycle { action: Export { options: SessionExportOptions } }` → `Payload::SessionExported { artifact }` | new arm in `commands.rs::apply_session_lifecycle` (`:789`), replacing the early return at `:806-808` and the `_ =>` at `:930` | write path — falls through `server.rs:3607` into `state.commands.apply`; reply already shaped by `lifecycle_response` at `server.rs:3814-3846` | already covered: `MutateSessionLifecycle { .. }` ⇒ `Controller` only (`:3025`) | already `NamedResource::Session` (`command.rs:1030`) |
| `CommandBody::RunEditorAction { session_id, action: EditorNativeAction, context: EditorActionContext, model }` → `Payload::EditorActionAccepted { run_id }` | reuse the ordinary `StartRun` path; no new module. `EditorActionContext.ide` becomes an `IdeContextUpdate` on the same session | write path | **ADD an arm**: `Contributor \| Controller \| Approver` — same floor as `StartRun`/`SubmitUserInput`. It falls through `_ => false` today | already `NamedResource::Session` (`command.rs:1031`) |
| `CommandBody::ExportBundle { request: BundleExportRequest }` → `Payload::BundleExported { receipt: BundleExportReceipt }` | new `crates/daemon/src/bundles.rs::export` | write path (it mints an artifact and a durable receipt) | **ADD an arm**: `Controller` — it is an exfiltration surface, so match the lifecycle floor, not the contributor floor | already maps each `request.source_session_ids` entry to `NamedResource::Session` (`command.rs:1044-1049`) — **but see §7.1** |
| `CommandBody::ImportBundle { request: BundleImportRequest }` → `Payload::BundleImported { receipt: BundleImportReceipt }` | new `crates/daemon/src/bundles.rs::import` | write path | **ADD an arm**: `Controller` | already `NamedResource::Artifact(request.bundle.id)` (`command.rs:1050-1052`) — the strongest gate in the file |

Every one of these must **also** be added to the `client_issued` vec in
`every_client_issued_command_has_a_decided_role_floor` (`commands.rs:5516`, list at `:5520-5535`).
The guard test only protects what it enumerates.

Capability flags already exist and default false: `ClientCapabilities.bundles` and
`.editor_actions` (`crates/protocol/src/capabilities.rs:39-52`). Note that **the daemon reads none
of them** — gating is entirely `is_reserved_unsupported_command`. Two tests assert they stay absent
for old clients and must keep passing:
`v0_9_client_hello_degrades_additive_capabilities_without_disconnect`
(`crates/daemon/tests/socket.rs:132`) and
`current_client_handshakes_with_v0_9_daemon_and_omits_new_capabilities`
(`crates/council/tests/previous_daemon_compat.rs:31`).

---

## 7. Ownership and authorization, per command

The pipeline, in order, for every `Payload::Command`: handshake gate (`server.rs:1451`) →
**reserved gate** (`server.rs:1464`) → **ownership gate** `authorize_command` (`server.rs:1482`) →
the read-command match (`server.rs:1502`) → catch-all into the write path (`server.rs:3607`).

Note the ordering consequence: the reserved gate fires *before* the ownership gate. Today an
`ExportBundle` naming another user's session returns `protocol.unsupported-payload`. **The moment
you unreserve it, the ownership gate goes live for that command for the first time** — so its
authorization behaviour has never been exercised. Write the negative test first.

### 7.1 `ExportBundle` — the empty-list hole

`command.rs:1044-1049` maps `source_session_ids` to sessions. An **empty** `source_session_ids`
therefore produces an empty `Vec`, `authorize_command`'s `for` loop never executes, and the gate
passes vacuously — the exact shape that leaked `ListSessions` and `SearchWorkspaceFiles` before.
`BundleExportRequest.source_session_ids` is `#[serde(default)]` (`bundle.rs:118-119`), so an empty
list is a valid wire payload.

Reject it explicitly in the handler with a fixed code (`bundle.invalid-request`, message
`"a bundle export must name at least one source session"`, `retryable: false`) **before** touching
the database. Never interpret "no sessions named" as "export everything".

### 7.2 `ImportBundle` — never trust the `ArtifactRef`

`BundleImportRequest.bundle` is a full `ArtifactRef` including a client-asserted `sha256` and
`byte_length`. The gate authorizes the *id* only. Re-hash the stored bytes yourself and compare
against both the ref and the manifest; a mismatch is a rejection, not a warning. This is the
`hash mismatch` hostile case in plan §270 and it is the reason `session_bundle_imports.bundle_sha256`
is documented as "bytes this daemon actually read".

The remaining hostile cases from plan §270 map to concrete defences:

| Case | Defence |
|---|---|
| oversized entries | clamp per-entry and total byte budgets before extraction; reuse the `MAX_READ_ARTIFACT_BYTES` discipline |
| path / symlink escape | reject any entry whose normalized path is absolute, contains `..`, or is not a regular file; canonicalize like `canonicalize_for_scope` (`server.rs:4996`) |
| duplicate paths | `PRIMARY KEY (bundle_id, path)` on `session_bundle_entries` |
| identity collision | `BundleCollisionPolicy` + `session_bundle_identity_map`; **always mint fresh local ids**, never reuse `source_id` |
| credentials | the contract has no credential field; the import must additionally refuse an unknown `BundleEntryKind` rather than passing it through |
| unsupported versions | `format_version > BUNDLE_FORMAT_V1` ⇒ reject; the column exists so this is a comparison, not a guess |

### 7.3 `RunEditorAction` — client-supplied editor scope

The session is gated, but `EditorActionContext` carries `ide: IdeContextUpdate` (paths, buffers,
selections) and an optional `repository_id`, all client-authored. Do **not** let them widen scope.
Resolve the repository from the session's own provenance
(`crates/daemon/src/commands.rs::session_run_provenance`) or validate the supplied path with
`principal_owns_repository` (`server.rs:4965`) — the component-wise canonicalized containment check
that already stops `..` and symlinked prefixes. A run started from an editor action must be an
ordinary attributable run, so `plan §261`'s "no extension-only model/tool loop" is satisfied
structurally: reuse `StartRun`, do not add a second entry point.

### 7.4 `MutateSessionLifecycle::Export` — artifact ownership

The export mints an artifact. Set `artifacts.owner_uid` (migration 0039) to `principal.uid()`, or
the `NamedResource::Artifact` arm (`server.rs:5202-5214`) will treat it as daemon-owned and the
user who exported it cannot read it back. Also honour
`SessionExportOptions.include_internal_sessions`: the default-omit rule at `session_library.rs:581`
is the library's contract and the export must not quietly diverge from it.

### 7.5 Error shapes

Copy the `SearchSessions` translation (`server.rs:2834-2852`): a specific, non-retryable code for a
caller-fixable problem (`session-library.invalid-cursor`), and a **single generic retryable code**
for everything internal (`session-library.query-failed`) with the real error logged via `warn!` and
never put on the wire. Use `bundle.invalid-request` / `bundle.verification-failed` /
`bundle.export-failed` in the same shape. Refusal messages must contain **no resource id** — the
`artifact.not-found` / `"artifact is unavailable"` pair (`server.rs:5209-5213`) is the template.

---

## 8. Acceptance criteria

Numbered, objectively checkable, each tied to a test name. A criterion is met when the named test
exists, fails before the change, and passes after.

**Task 2.2**

1. A council run leaves no non-internal child sessions — `council::service` tests:
   `council_child_sessions_are_internal_at_creation`.
2. Success, failure, cancel, and crash-recovery all archive internal children exactly once —
   `crates/codypendentd/src/workflows.rs` tests:
   `internal_sessions_archive_on_every_terminal_path`.
3. Default browse omits them; explicit search finds them — already covered by
   `default_browse_omits_archived_internal_sessions_but_explicit_search_can_find_them`
   (`crates/daemon/tests/session_library_it.rs:511`); extend it to a council-created session.

**Task 2.3**

4. A frame split across three chunk boundaries reassembles — `sdk/protocol/test/client.test.ts`:
   `fragmented_frames_reassemble_in_order`.
5. Handshake → attach → paginated catch-up → live, with an overlapping sequence delivered twice,
   yields each event once — `catchup_overlap_is_deduplicated`.
6. Requests correlate by id under interleaved replies; a reconnect replays the resume token and
   drains a bounded offline queue — `requests_correlate_and_queue_is_bounded`.
7. `extensions/vscode/src/protocol/frame.ts` no longer exists and the extension suite is green:
   `test -f extensions/vscode/src/protocol/frame.ts` returns 1, and
   `npm --prefix extensions/vscode test` passes.

**Task 2.4**

8. Discovery → connect → create → attach → full paginated catch-up → live overlap dedup →
   start/cancel → approval → question → artifact read, all against a stubbed transport —
   `apps/desktop/test/daemon.test.tsx`: `desktop_projects_a_full_session_lifecycle`.
9. `apps/desktop/src/transport.ts` declares no protocol interfaces: `grep -c "^interface" ` returns
   0 and the file imports from `@codypendent/protocol`.
10. `npm --prefix apps/desktop run build` and `tauri build` succeed on macOS CI with React 19.

**Task 2.5**

11. Every ported VS Code renderer test passes unchanged against `sdk/ui/src/host-react/` —
    `sdk/ui/test/host-react/renderer.test.tsx`.
12. A denied capability, an unknown node kind, an unregistered slot, and an invalid event payload
    each render a safe fallback instead of throwing —
    `capability_denial_and_unknown_nodes_degrade_safely`.
13. Desktop renders a real daemon Remote UI document, not a metadata card —
    `apps/desktop/test/remote-ui.test.tsx`: `desktop_renders_semantic_remote_ui`.

**Task 2.6**

14. Attach loads every paginated history page before the first live event is projected —
    `extensions/vscode/test/client.test.ts`: `attach_drains_all_history_pages_before_live`.
15. Each of the five editor actions contributes a command with correct
    `when`-clause enablement (editor focus, selection, diagnostic present) —
    `extensions/vscode/test/editor-actions.test.ts`, one case per action.
16. Every editor action produces an ordinary `StartRun`-equivalent daemon run carrying the current
    `IdeContextUpdate`, source identity, and idempotency key —
    `editor_actions_start_attributable_daemon_runs`.
17. No model or tool call originates in the extension: `grep -rE "fetch\(|https?://" extensions/vscode/src`
    matches nothing outside the daemon transport.

**Task 2.7**

18. `migrations/checksums.json` contains `0041_session_bundles.sql` and
    `python3 .github/scripts/check_migration_immutability.py` exits 0.
19. An export with every inclusion switch false produces a manifest with zero entries and a stable
    `manifest_sha256` — `crates/daemon/tests/bundles_it.rs`:
    `inclusion_policy_is_fail_closed_and_manifest_is_deterministic`.
20. Two exports of identical input produce byte-identical archives —
    `export_is_content_addressed_and_reproducible`.
21. Each hostile import case is rejected with its own code and leaves the database unchanged —
    `hostile_imports_are_rejected_without_partial_writes`, one `#[case]`/section per row of §7.2.
22. `ExportBundle` with an empty `source_session_ids` is rejected —
    `empty_export_request_is_rejected_not_treated_as_export_all`.
23. `ExportBundle` naming another uid's session returns the generic session not-found —
    `crates/daemon/tests/server_it.rs`: `bundle_export_cannot_name_another_principals_session`.
24. A round trip into a fresh database restores sessions under **new** ids with imported provenance,
    and restores no approvals and no credentials —
    `round_trip_into_fresh_database_remaps_identities_and_restores_no_authority`.
25. Export and import are driven from the CLI — `crates/cli/tests/bundle_it.rs`:
    `cli_exports_and_imports_a_bundle`.

**Task 2.8 (gate)**

26. `is_reserved_unsupported_command` (`commands.rs:2971`) no longer lists
    `MutateSessionLifecycle { action: Export }`, `RunEditorAction`, `ExportBundle`, or
    `ImportBundle`. `ManageAutomationBinding`, `ListInbox`, `MutateInbox`, `QueryAnalytics`, and
    `ExportAnalytics` remain.
27. `reserved_command_is_rejected_before_role_checks` (`commands.rs:5983`) still passes — it
    currently probes `QueryAnalytics`, which M2 does not unreserve, so it needs no change here.
    Confirm rather than assume.
28. `every_client_issued_command_has_a_decided_role_floor` (`commands.rs:5516`) lists
    `RunEditorAction`, `ExportBundle`, and `ImportBundle`.
29. End-to-end: create/run/search/rename/pin/archive/restore/export from desktop, attach and review
    the patch from VS Code, against one real daemon with networking disabled.
30. The full gate from conventions §10 is green, plus `npm --prefix sdk/protocol run check`,
    `npm --prefix apps/desktop run check`, and the `tauri build` step.

---

## 9. Gotchas

1. **The reserved gate precedes the ownership gate.** `server.rs:1464` vs `:1482`. Unreserving a
   command exposes its authorization path for the first time. Every test that currently asserts
   `protocol.unsupported-payload` for a variant you unreserve will break — that is the signal, not
   a regression.
2. **`role_permits` ends in `_ => false`.** `ExportBundle`, `ImportBundle`, and `RunEditorAction`
   are all unlisted today. Shipping them without an arm produces a feature that is dead on the wire
   with a fully green unit-test suite. This has happened three times (conventions §1).
3. **`named_resources()` has no wildcard arm** (`command.rs:985`, doc at `:962-984`) — a new
   `CommandBody` variant will not compile until you classify it. That is deliberate and it is your
   friend. What it cannot catch is a variant classified into the `Vec::new()` group when it should
   name something (§7.1).
4. **`apps/desktop/src-tauri` is outside the root cargo workspace** (its `Cargo.toml` declares its
   own `[workspace]`). `cargo clippy --workspace --all-targets` does not lint it.
5. **`sdk/ui` demands React 19 and `react-reconciler` 0.31.** Desktop is on React 18. Task 2.5 is
   blocked on Task 2.4's upgrade bullet.
6. **`extensions/vscode/package.json` has no `dependencies` block.** Adding
   `@codypendent/protocol` is a packaging change, not just an import change, and it touches the CI
   `cache-dependency-path` list.
7. **Protocol vectors partition.** Any new vector under `protocol-vectors/*.json` must be added to
   the `modeled` or `notModeled` list in `extensions/vscode/test/protocol-vectors.test.ts` or
   `assertPartitionIsComplete` fails; `sdk/protocol` auto-discovers and needs no edit. Adding a
   vector also drifts the `doc-count:vitest` markers, which `--skip-vitest` hides locally
   (conventions §5).
8. **Search cursors are query-bound.** `session_library.rs:1075-1091` rejects a cursor whose
   `query_hash` does not match `(principal_uid, query, filters)`. Both clients must clear paging
   state on filter change or users see `session-library.invalid-cursor` mid-scroll.
9. **`SessionSearchResult.score` is an `f64` encoded into the cursor as raw bits**
   (`session_library.rs:1065`, validated `is_finite` on decode at `:1086`). Do not "normalize"
   scores in a client and feed them back.
10. **Imports must never reuse source ids.** `sessions.tombstoned_at` (0040) means a deleted session
    can be resurrected by an import that reuses its id, bypassing the retention decision entirely.
    Identity remap is a security property, not a convenience.
11. **Untracked files in this tree are other people's work.** `session-library-view.tsx`,
    `.codypendent/`, `.cursor-tmp/`, `.idea/`, `.poolside/`, `300726.txt`. Stage by explicit path;
    `git add -A` will sweep them in (conventions §9).
12. **Adding these two guide files drifts `docs/MANIFEST.json`.** Run
    `python3 .github/scripts/check_docs_manifest.py --fix` and commit the regenerated manifest.

---

## 10. Where the plan and the shipped code disagree

- **Plan §216 names `crates/council/src/service.rs`** for Task 2.2. It is the one file in that list
  with no implementation at all, while the two `codypendentd` files are already done. The plan's
  file list is accurate about scope and misleading about progress.
- **Plan §225 says "Refactor `extensions/vscode/src/{client,protocol/frame}.ts` to consume it."**
  `sdk/protocol` currently has no `client.ts` to consume, so the refactor is blocked on the missing
  half of its own task, not on the extension.
- **Plan §235 names `apps/desktop/src/daemon/{transport,projection,commands}.ts` and
  `src/hooks/useDaemonSession.ts`.** None exist; the equivalent logic shipped flat as
  `src/transport.ts`, `src/daemonState.ts`, `src/useDaemon.ts`. Treat the paths as descriptive.
- **Plan §271 says imports "never restore credentials or approvals"** — the wire contract enforces
  the credential half structurally (`bundle.rs` has no credential field, pinned by
  `contract_has_no_credential_restoration_field`), but `BundleIdentityKind::Approval`
  (`bundle.rs:168`) exists, so the approval half is an implementation obligation with no type-level
  guard. Test it explicitly.
