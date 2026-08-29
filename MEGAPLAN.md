# Megaplan — Everything, Sequenced (v0.8 → v1.0)

**Status:** plan of record · **Supersedes:** [`16-master-implementation-plan.md`](docs/docs/adoptions/16-master-implementation-plan.md)
(product roadmap) and the execution ordering in [`../../../improvement_plan/README.md`](improvement_plan/README.md)
(remediation). Both remain authoritative for their own **evidence**; this document owns the
**sequence**.

One plan covering every open item: ~80 tracked units across four work streams, sequenced into
five releases. Every item carries an ID that traces back to its source document, so nothing is
re-litigated and nothing is lost.

---

## 1. Where we actually are

| Thing | State |
|---|---|
| **v0.7.0** | Shipped and published (adoption suite + security hardening). |
| **v0.8.0** | **PR #67 open, all 8 CI checks green**, at `5c9dbbc`. 21 actions across specs 17–21 + the tool-card reconciliation fix. Awaiting merge + tag. |
| **Uncommitted working tree** | A *later* batch (edit_match unicode stage, RUFF roster entry, `/undo` palette entry, four untracked SDK `.tsx` files). **Contains the two P0 defects.** Not in PR #67. |
| **Baseline health** | `cargo clippy --workspace --all-targets` clean; `cargo test --workspace` 3165+ passing, 0 failing. |

**The critical fact:** PR #67 is clean. The P0 defects live only in the uncommitted tree and would
ship only if that tree is committed. v0.8.0 can merge and tag today.

---

## 2. The four work streams

| Stream | What it is | Source | Items |
|---|---|---|---|
| **R — Remediation** | Defects found by the improvement-plan scan: blockers, security, correctness, hygiene. | `improvement_plan/findings-register.md` (A/B/C/D), `checklist.md` | ~55 |
| **P — Product** | Remaining feature roadmap: parity gaps, graphical client, engine completion. | `16-master-implementation-plan.md` (Tracks B/C/D) | ~16 |
| **T — Truth** | Documentation that is wrong in both directions — overclaims *and* understatements. | register §E1/E3, `checklist.md` Phase 4 | 3 |
| **I — Infrastructure** | CI/tooling gaps that let defects through. | `checklist.md` Phase 4 | 4 |

Track A (UI/UX) from the master plan is **substantially delivered in v0.8** — the transcript
redesign, modal system, empty states, and polish pass all shipped in PR #67. The residue folds
into P-B1 below.

---

## 3. Release sequence

| Release | Theme | Contents | Gate |
|---|---|---|---|
| **v0.8.0** | *Feels finished* | Merge PR #67 as-is; tag. **No new work.** | Already green |
| **v0.8.1** | *Trustworthy* | R-P0 (4) + R-SEC (6) + R-COR (13) + T (3) | No panic-reachable or file-corrupting path; an accepted cancel always cancels; no UI that claims an action it didn't perform |
| **v0.9.0** | *Hardened & connected* | R-HARD (12) + R-HYG (14) + I (4) + P-B (parity batch) + P-C1 (TSX SDK) | One writer per config file; every untrusted sink capped; MCP connected |
| **v0.10.0** | *The graphical client* | P-C2 (Tauri, milestones 1–6) | Desktop + TUI share one daemon; plugin parity from one artifact |
| **v1.0.0** | *Architecture complete* | P-D (WASM, OS sandbox, capture, measured routing) + eval corpus | Every ROADMAP `[x]` is a path the shipped default binary executes |

**Rationale for this order.** Remediation precedes features because two P0s are *trust*
failures — a match stage that corrupts files and a command that fabricates a rollback. Shipping
features on top of those erodes confidence faster than the features build it. Hardening batches
with the parity work because both touch config/IO paths. The graphical client is its own release
because it adds a build target and CI surface. Engine completion is last: it is the least
user-visible and the largest single unit (WASM).

---

## 4. v0.8.0 — merge and tag (today)

Nothing to build. Two actions:

1. Merge PR #67 (`release/v0.8.0-feels-finished` → `main`).
2. Tag `v0.8.0` at the merge commit → fires the release build (native Linux + macOS binaries,
   published GitHub Release).

**Do not** commit the current uncommitted working tree into this release. It goes to v0.8.1 after
its P0s are fixed.

---

## 5. v0.8.1 "Trustworthy" — remediation

### 5.1 R-P0 · Blockers (do not commit the tree until these are fixed)

| ID | Item | Location | Effort |
|---|---|---|---|
| **R-P0-1** | `edit_file` **panics and silently corrupts source files** — the `unicode_normalized` stage computes a byte index on the *normalized* line and slices the *original* line; `end <= line.len()` is a range check, not a char-boundary check. Contained today only because `spawn_blocking` catches the unwind; a `panic="abort"` profile makes it a remote daemon kill from model output. Fix: restrict the stage to whole-line/whole-block candidates via `line_spans` (as the other nine stages do) **or** carry a normalized→original offset map; drop the backtick→`'` mapping (makes `'a'` match `` `a` ``). Tests must cover the **content-carries-wide-characters** direction (the real-world one the new test misses). | `runtime/src/tools/edit_match.rs:524-531`, `:494` | M |
| **R-P0-2** | **`/undo` is cosmetic and fabricates a completion record** — pushes a present-progressive "restoring latest checkpoint" note into durable scrollback and does nothing else. A user who reads it and commits is building on changes they believe were reverted. Backend is **fully built** (`RestoreCheckpoint` command + approval-gated daemon handler); only a TUI `Intent` is missing. Fix: wire the intent behind a `ConfirmCancel`-style confirmation, **or remove the palette entry** until then. Also fix the phantom `key: "u"` (only `Ctrl-U` is bound) and strengthen the palette guard test to cross-check every key against `input.rs`. | `tui/src/reduce.rs:7175-7196`, `tui/src/palette.rs`, `tui/src/input.rs:488` | S |
| **R-P0-3** | **Every Python file spawns two language servers** — `RUFF` claims `.py`/`.pyi` that `PYRIGHT` already owns and the roster loop has no `break`: doubled latency, duplicated diagnostics. Also re-confirm the `_ => Some(worktree_canon)` contract change (a server now spawns with no project marker; `tsserver` on a marker-less tree is expensive) and whether `ruff server` satisfies the initialize/touch/diagnostics handshake. | `knowledge/src/lsp/servers.rs:24-28,48-52`, `lsp/mod.rs:124-147` | S |
| **R-P0-4** | Four untracked SDK components are re-exported with **no build check**; bring them to the sibling `SurfaceOptions` contract (spread, caller-derived ids, `message=` badge, remove dead `onApplyHunk`, cap payloads) and add catalogue + snapshot tests. | `sdk/ui/src/first-party/*.tsx` | M |

### 5.2 R-SEC · Security

| ID | Item | Location | Effort |
|---|---|---|---|
| **R-SEC-1** (B1) | ~~**Scope escape via symlink after a `..` pop**~~ — **DONE 2026-08-25** (see register B1): the `scope.rs` copy was already rewritten per-component in v0.9.0; the duplicate `worktrees.rs` copy was deleted in favour of the shared `policy::canonicalize_lenient`, and `shell.rs`/`unified_exec.rs`/`git.rs::guard` now act on the `PathScope::resolve` result instead of the raw cwd. Three regressions pinned. | `daemon/src/policy/scope.rs`; `daemon/src/worktrees.rs`; `runtime/src/tools/{shell,git,unified_exec}.rs`; `runtime/tests/tools_it.rs` | M |
| **R-SEC-2** (B2) | **Unauthenticated peer forces an arbitrary directory walk** — `maybe_scan_repository` runs on a client-supplied path *before* the ownership gate. Fix: gate first, warm after. | `daemon/src/server.rs:5782` (and `:3648`) | S |
| **R-SEC-3** (B3) | **Learned approval patterns leave the argument tail unconstrained** — a learned `rg *` auto-approves `rg --pre=/tmp/evil` (arbitrary exec); `cargo build *` covers `--config build.rustc=…`. (The *prefix*-flag case was closed in v0.7; the tail was not.) | `daemon/src/policy/arity.rs:214-256` | M |
| **R-SEC-4** (B4) | Hook engine **fails open** on an unparseable `spec_json` — silently drops an approved `failure = "block"` `tool.pre` validator. | `daemon/src/hook_engine.rs:71-77` | S |
| **R-SEC-5** (B6) | SQLite DB (full transcripts + tool output) created at **`0644`**, bypassing the `owner_uid`/`SO_PEERCRED` isolation from migration 0031. Every other sensitive file is `0600`. One-liner. | `daemon/src/db.rs:12-28` | S |
| **R-SEC-6** (B5) | `is_denied_env` gaps — `RIPGREP_CONFIG_PATH`, `CARGO_BUILD_RUSTC`, `CARGO_TARGET_*_RUNNER`, `CARGO_ALIAS_*`, `GIT_PAGER`/`PAGER`/`GIT_EDITOR`/`GIT_SEQUENCE_EDITOR` are execution hijacks for default-allow-listed programs. | `runtime/src/tools/shell.rs:251-289` | S |

### 5.3 R-COR · Correctness & data integrity

| ID | Item | Location | Effort |
|---|---|---|---|
| **R-COR-1** (C1) | **Lost `CancelRun`/`PauseRun`** — `pending_cancellations` and `cancellations` are independent mutexes with the guard released between check and act on both sides; a cancel arriving during `spawn_run` is silently overwritten. The client is told the cancel was accepted and the run completes anyway. Fix: one mutex, or re-check the pending set after installing the handle. | `codypendentd/src/executor.rs:2515-2538` vs `:2674-2705` | M |
| **R-COR-2** (C3) | **A fifth `models.toml` writer bypasses the lock** — `write_remove_model` does its own read-modify-write with **no `flock`** and a **pid-only** temp name, breaking the documented "independent updates cannot erase each other" invariant. Route through `models_file::update_model_entries`. | `cli/src/tui.rs:4962-5052` | S |
| **R-COR-3** (C4) | **`models add` on an existing id wipes `api_key_env` and `context_tokens`**, then prints `updated model <id>`. Same success-message-over-silent-destruction shape as the bug it replaced. | `cli/src/commands.rs:3944-3963` | S |
| **R-COR-4** (C5) | **`auth.json` has no lock and a fixed temp path** — concurrent saves discard credentials; a shared `auth.json.tmp` can rename a mixed document into place. Mirror `models_file.rs`. | `runtime/src/auth.rs:97-135` | S |
| **R-COR-5** (C2) | **Panicking workflow drive poisons the run permanently** — cleanup is inline after the await, not `Drop`-based, so a panic leaves `drive_active`/`cancelled` set forever and a later `recover()` is **born cancelled**. RAII guard. | `codypendentd/src/workflows.rs:294-360` | M |
| **R-COR-6** (C8) | Remote UI worker: quota leak on panic; `stop_plugin`/`shutdown` `clear()` **every** session's epoch set (defeating the guard for unrelated live sessions); six `.expect()` on `Option` payloads. Shares R-COR-5's root cause. | `daemon/src/remote_ui_workers.rs:194-341`, `:564-591` | M |
| **R-COR-7** (C7) | ~~**Paused agent runs are *failed* by restart recovery**~~ — **DONE 2026-08-25** (see register C7): recovery now preserves `Paused` runs (`RecoveryReport.preserved_paused`, their approvals survive), and `RuntimeExecutor::resume_run` re-drives a parked run with no live handle from the durable row via the existing ledger-reconstructed seed transcript. Three regressions pinned. | `daemon/src/recovery.rs`; `codypendentd/src/executor.rs` | S |
| **R-COR-8** (C10) | **Unbounded `read_file` range** → ~67M `String` allocations (>1.5 GB RSS) from one tool call. | `runtime/src/agent.rs:7149-7157`, `tools/read_file.rs:105-155` | S |
| **R-COR-9** (C6) | Unknown `[[model]]` keys dropped on every write (no `serde(flatten)`); clamped `context_tokens` **persisted**, permanently editing the user's file. | `cli/src/models_file.rs:101-104` | S |
| **R-COR-10** (C9) | `ensure_scanned` stamps the pre-lock revision, so the map can assert a revision the graph does not hold and a later run skips a needed scan. | `codypendentd/src/executor.rs:559-581` | S |
| **R-COR-11** (C11) | `WorktreeReleaseGuard`: the normal path terminates processes under the whole repo root for read-only runs; the unwind path terminates **nothing**, so children outlive the run. | `codypendentd/src/executor.rs:3389-3437` | S |
| **R-COR-12** (C12) | DEFERRED transactions on read-then-write with no `SQLITE_BUSY` retry where the codebase otherwise uses `BEGIN IMMEDIATE`. Touches invariant 4 (checkpoints before destructive modification). *Suspicion — not reproduced.* | `daemon/src/checkpoints.rs:60/187/216`, `model_profiles.rs:258` | S |
| **R-COR-13** (C13) | **No retention or pruning** on `events`/`learning_records`/`model_task_outcomes`/`memory_forget_audits`; `load_events` is a full unpaginated scan per session. | `migrations/0001_init.sql:25-35`, `daemon/src/ledger.rs:64-72` | M |

### 5.4 T · Documentation truth (cheap, parallel, high-trust)

| ID | Item | Effort |
|---|---|---|
| **T-1** | **Soften five overclaims** (§E1): the router is real but **default-OFF** (and defaults on a *malformed* `routing.toml`); the classification hard filter never receives a real per-run classification (both production sites pass `None` — fails closed, so the *safety* claim holds, the *capability* claim does not); 7.3's cascading escalation is `[x]` but `escalate` has **only test callers**; 7.5's shadow/canary are DB flips fed **caller-supplied** metrics; voice has never run against a real speech provider and defaults to a `Confidential` off-device ceiling. | S |
| **T-2** | **Correct five understatements** (§E3): session forking (STEP 5.6), the WASM host (`wasmi`, not `wasmtime`), the hook engine, client voice capture, and live language-server spawning are all built and wired but listed as unbuilt. | S |
| **T-3** | **Change what `[x]` means** — make it "wired into a shipped default path" and add a distinct marker for "engine built, not yet driven". This single change removes most of the overclaim/understatement whiplash. | S |

### 5.5 v0.8.1 definition of done

- No panic-reachable or file-corrupting path in `edit_file`; tests cover content-carries-wide-chars.
- No palette command reports an action it did not perform; every advertised key exists in `input.rs`.
- One language server per file type.
- An accepted cancel/pause always takes effect (with a concurrent cancel-vs-spawn stress test).
- One writer per config file, each holding a lock with a unique temp name.
- `cwd` is spawned from the **resolved** path everywhere.
- Every ROADMAP `[x]` claim matches shipped behaviour.
- `cargo test --workspace` at 0 failures with a regression test per R-P0/R-SEC/R-COR item.

---

## 6. v0.9.0 "Hardened & connected"

### 6.1 R-HARD · Remaining hardening (P2 residue)

`R-HARD-1` remote-UI epoch bookkeeping follow-ups · `R-HARD-2` SDK worker lifecycle
(`bridge.ts:454` abort rejection, `runtime.ts:402` `#shutdown` idempotency, `stdio.ts:19` drain
timeout) · `R-HARD-3` PTY missed wakeup (`unified_exec/process.rs:257-292`) · `R-HARD-4` hardcoded
`"deny"` verdict in `DispatchAudit` (`hook_exec.rs:241,354`) · `R-HARD-5` real artifact store on
fork-stash failure (`executor.rs:3202-3209`) · `R-HARD-6` cap `Content-Length` allocation
(`lsp/transport.rs:116-129`) · `R-HARD-7` sanitize LSP diagnostics before they enter model context.

### 6.2 R-HYG · Hygiene sweep (D1–D14)

`bounded_head_tail` slice panic for `max < 5` · UTF-8-splitting head/tail truncation ·
non-atomic `routing.toml`/`trust_store`/`councils.toml` writes · reserved-process-id leak ·
`exec` deadline shortened by the grace wait · cosmetic `Blackboard` palette command · silent
no-op `Steer`/`PauseResume` · O(n) transcript eviction · `QuestionBroker` waiter growth ·
`admitting_network` re-widening after the untrusted narrow · unguarded per-repository code-graph
access · `auth.json`-outranks-env surprise · stale `TODO(ownership)` wording · unbounded
`report_rewrite` update. Plus the Phase-3 low-severity list (instruction-file starvation at the
byte cap, sink-side `workflow_id` validation, dead seatbelt network branch, `agent.version` path
validation, checksum `.trim()` asymmetry, manifest non-empty checks, Unicode Cf handling,
idempotency whole-body scan, `UiWorker::selection()` pre-handshake panic, migration numbering gap).

### 6.3 I · Infrastructure

| ID | Item |
|---|---|
| **I-1** | **Add a macOS CI job** — the Seatbelt executor (and therefore the sandboxed `!` path) is never exercised in CI today. |
| **I-2** | Add Dependabot/renovate. |
| **I-3** | Resolve the 0003 migration-immutability violation; ship a repair path or document the break (DBs from `v0.1.0-build.43/.44/.45` cannot be opened by any later release). |
| **I-4** | Extend the palette/keymap guard test pattern to other advertised-vs-bound surfaces. |

### 6.4 P-B · Product parity batch

MCP client (+OAuth, prewarm) → then deferred-tool loading and MCP server mode; prompt-template
commands (`.codypendent/commands/*.md`); policy-gated web tools (WebFetch/WebSearch behind
network-egress approvals); configurable keymap (codex `RuntimeKeymap` port); composer/terminal
residue from Track A (resize reflow, paste-burst, IME, hyperlinks).

### 6.5 P-C1 · TSX Remote UI authoring SDK

Spec [13](docs/docs/adoptions/13-remote-ui-authoring-sdk.md) as written. Land the protocol schema export first so
prop types are generated rather than hand-mirrored.

---

## 7. v0.10.0 — the graphical client

Spec [14](docs/docs/adoptions/14-tauri-react-companion-client.md), milestones 1–6. Milestone 5 (shared React
`UiDocument` renderer) consumes P-C1. Exit: a run started in either surface streams live in the
other, approvals answered from either side produce **one** ledger record, reconnect resumes from
last-seen sequence, and the TUI/daemon suites are unchanged (purely additive).

---

## 8. v1.0.0 — architecture complete

`P-D1` wasmtime/wasmi component runtime + WASM **SDK** (host is done; the SDK is what makes it
usable) + brokered-secrets host + Linux/Windows OS enforcement consuming `SandboxProfile` ·
`P-D2` client capture (clipboard/voice/IDE drag-drop) + the agentic `setup` assistant ·
`P-D3` **real measured routing**: derive a per-run `DataClassification` so the hard filter has an
input, drive `escalate` from the agent loop, and *measure* `CanaryMetrics` instead of accepting
them from the request · `P-D4` eval corpus 13 → 50–100 pinned fixtures, and document that the
current gate cannot detect prompt/skill regressions because the model is a deterministic stub ·
`P-D5` generated protocol SDK (the VS Code extension hand-duplicates the wire codec — live drift
risk).

---

## 9. Dependencies

```
R-P0-1..4        ── block committing the current working tree (everything downstream)
R-COR-5 ─shares root cause─ R-COR-6      (do the RAII guard once, apply twice)
R-SEC-3 ─extends─ the v0.7 arity prefix fix (tail, not prefix)
T-1..3           ── no code dependency; run in parallel with anything
I-1 (macOS CI)   ── should precede v0.9 sandbox work so Seatbelt is actually exercised
P-B MCP          ── gates deferred-tool loading + MCP server mode
P-C1             ── gates P-C2 milestone 5
P-D3             ── converts three Phase 7 `[x]`-with-caveats into honest completions (pairs with T-1)
```

---

## 10. Methodology (unchanged — it has caught real bugs twice)

1. **Spec first** for anything non-trivial, in `docs/docs/adoptions/`, house template.
2. **Implement** with agents partitioned by **disjoint file ownership**; a regression test per fix.
3. **Adversarial review** the merged diff — green tests are necessary, not sufficient. This flow
   has now caught, pre-merge: a policy bypass, two cross-user leaks, a dead `/context` feature, a
   role-gate that silently killed two commands, and an always-failing fork insert.
4. **Full gate**: `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo test --workspace`, **plus** `doc-counts` (test-count markers **and**
   `check_docs_manifest.py --fix`) and `extension` (protocol-vector partition + vitest markers).
   Run these **locally before pushing** — they are invisible to `cargo test`.
5. **Ship**: version bump (single-source workspace `Cargo.toml`), PR, address review, tag at the
   merge commit. Push via the owner account, restore after.

**Verify claims against code, not summaries.** Two implementation reports in this project have
overstated "green" — once by running clippy without `--all-targets`, once by testing a handler
directly while the wire path was role-denied.

---

## 11. Risks

- **The uncommitted tree is the single biggest risk.** It contains a file-corrupting defect and a
  trust-breaking one. It must not be committed before R-P0-1/2/3 land.
- **Migration numbering** — assign centrally per release (next free: `0039`).
- **Doc gates bite every release** — new tests drift ROADMAP markers; new docs break the manifest;
  new wire vectors break the extension partition. Budget a fix commit each time.
- **WASM (P-D1) is the largest single unit** — likely warrants its own point releases rather than
  one drop.
- **Tauri (P-C2) adds a build target** — the release workflow currently builds Rust binaries only.
- **`ruff` may not be an LSP** — R-P0-3 must confirm `ruff server` satisfies the handshake before
  keeping it in the roster at all.

---

## 12. One-glance summary

| | Release | One line |
|---|---|---|
| ✅ | **v0.8.0** | Merge PR #67, tag. Clean; nothing to build. |
| ⬜ | **v0.8.1** | *Trustworthy* — fix the two P0 trust failures, the sandbox `cwd` escape, the lost cancel, the config writers; tell the truth in the roadmap. |
| ⬜ | **v0.9.0** | *Hardened & connected* — hygiene sweep, macOS CI, MCP, prompt-templates, web tools, TSX SDK. |
| ⬜ | **v0.10.0** | *Graphical* — the Tauri desktop client. |
| ⬜ | **v1.0.0** | *Complete* — WASM + OS enforcement, capture, measured routing, eval corpus. |

**Release philosophy (reaffirmed):** ship fewer, fully reliable features. Both P0s are cases of a
feature reaching the user interface ahead of its correctness. Fixing that ordering *is* the plan.
