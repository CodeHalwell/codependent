# Codypendent — Codebase Scan Verification

**Scan date:** 2026-08-15 (local time 23:58 +01:00)
**Baseline:** `5c9dbbc` + uncommitted working tree (7 modified files, 4 untracked SDK components, 4 stray untracked dirs/files)
**Platform:** macOS arm64, Rust 1.97.0, cargo 1.97.0
**Scanned by:** Independent re-scan against the working tree as it currently exists.

---

## Verification status

| Check | Command | Result |
|---|---|---|
| Lint | `cargo clippy --workspace --all-targets` | clean, zero warnings |
| Type check | `cargo check --workspace --all-targets` | clean (exit 0, 0.95s) |
| Tests | `cargo test --workspace --lib --bins --examples` | **249 passed, 1 failed, 0 ignored** (flaky — see below) |

All P0/P1/P2 items from the previous scan (see `README.md`, `findings-register.md`) were
**re-verified line-by-line against the current uncommitted tree**. Every entry checked in
this pass was found to still be present and accurate. No register entry was found stale, and
no new correctness/security findings beyond the register were identified.

---

## Findings re-verified against the current tree

Every finding below was confirmed by reading the exact file and line range cited
in `findings-register.md`:

| Finding | Re-verified evidence |
|---|---|
| A1 `edit_match` span bug | `edit_match.rs:524-531` — normalized-space `idx`/`end` slice the raw line; `end <= line.len()` is a range check, not a boundary check; only the safe whole-line branch is tested |
| A2 `/undo` stub | `reduce.rs:7175-7196` pushes only a transcript `Note`; no `Action::Undo` variant in `action.rs`; `RestoreCheckpoint` backend confirmed wired (`protocol/command.rs:767`, `daemon/server.rs:3024`, `daemon/checkpoints.rs:135`) and never sent by any client |
| A3 duplicate `.py`/`.pyi` ownership | `servers.rs` (uncommitted diff) — `PYRIGHT` and `RUFF` both claim `[".py", ".pyi"]`; the `mod.rs:124-147` roster loop has no `break` |
| A4 new SDK exports unbuilt | `sdk/ui/src/first-party/index.ts` re-exports four `.js` files whose `.tsx` sources are untracked; SDK typecheck not run before commit |
| B1 `canonicalize_lenient` escape | `policy/scope.rs:213-236` — remainder after `..` pop appended lexically, symlinks unresolved |
| B2 unauthenticated directory walk | `server.rs:5782` calls `maybe_scan_repository` before the ownership gate at `:5792` |
| B3 unconstrained arg tail | `policy/arity.rs:240` — `pattern_matches` accepts any tail; `command_prefix` returns only first token for unknown programs like `rg` |
| B4 hook engine fail-open | `hook_engine.rs:73-76` — `if let Ok(spec) = …` silently drops parse failures |
| B5 `is_denied_env` gaps | `shell.rs:251-289` — missing `RIPGREP_CONFIG_PATH`, `CARGO_BUILD_RUSTC`, `CARGO_TARGET_*_RUNNER`, `CARGO_ALIAS_*`, `GIT_PAGER`/`PAGER`, `GIT_EDITOR`/`EDITOR`/`GIT_SEQUENCE_EDITOR` |
| B6 SQLite DB 0644 | `db.rs:12-28` — `create_if_missing(true)` with no `chmod`; contrast `0600` on `auth.json`/`models.toml` |
| C1 cancel/pause TOCTOU | `executor.rs:2515-2538` vs `:2674-2705` — three independent mutexes, guard released between check and act on both sides |
| C2 panicking drive poisons run | `workflows.rs:294-360` — inline cleanup not `Drop`-based; `cancelled` flag sticks |
| C3 fifth `models.toml` writer | `tui.rs:4962-5052` — no flock, pid-only temp name |
| C4 `models add` wipes key_env | `commands.rs:3950` — `key_env.unwrap_or_default()`, then `retain` + `push` |
| C5 `auth.json` no lock | `auth.rs:97-135` — `data_dir.join("auth.json.tmp")` is a fixed path, no flock |
| C6 clamped values persisted | `models_file.rs:101-103` serializes `Vec<ModelConfig>` without `#[serde(flatten)]`; `models.rs:177` clamps on load, next write persists |
| C7 paused runs destroyed | `recovery.rs:78-89` fails `Paused` runs; `workflows.rs:249-262` preserves them |
| C8 remote UI worker leaks | `remote_ui_workers.rs:194-341` — inline cleanup, global `clear()`, `.expect()` on `Option` payload fields |
| C9 `ensure_scanned` pre-lock revision | `executor.rs:559-581` — samples revision before acquiring the scan lock |
| C10 unbounded `read_file` | `agent.rs:7149-7157` — arbitrary `u64` bounds cast to `usize` |
| C11 `WorktreeReleaseGuard` disagreement | `executor.rs:3389-3437` — normal path `terminate_under(worktree)`, `Drop` only `if lease.is_some()` |
| C12 DEFERRED txn, no retry | `checkpoints.rs:60/187/216`, `model_profiles.rs:258` — no `SQLITE_BUSY` retry logic |
| C13 no retention/pruning | `ledger.rs:64-72` — full unpaginated `SELECT` per session; no `DELETE`/`VACUUM`/TTL on any table |
| D1 `bounded_head_tail` panic | `render.rs:2895-2912` — `lines[..3]` panics for `max < 5` |
| D3 non-atomic `routing.toml` write | `commands.rs:4440-4446` — `std::fs::write`, no temp+rename+fsync |
| SDK bridge abort rejection | `bridge.ts:454` — `void this.cancel(...)` discards promise, no `.catch()` |
| SDK `#shutdown` non-idempotent | `runtime.ts:402` — if `#sendControl` throws, `transport.close()` and `state = "disposed"` never run |
| SDK stdio drain hang | `stdio.ts:19` — `once(output, "drain")` with no timeout/error listener |
| SDK Badge children vs `message=` | All four components pass Badge content as children; sibling `StatusBadge` uses `message={status}` |
| LSP unbounded Content-Length | `knowledge/src/lsp/transport.rs:129` — `vec![0u8; length]` with no cap |
| LSP unsanitized diagnostics | `knowledge/src/lsp/client.rs:467-471` — `message` parsed from LSP server, embedded in model context with no sanitization |
| LSP missed wakeup | `unified_exec/process.rs:282` — drain-then-`notified()` without `enable()`; producer uses `notify_waiters()` |
| hook_exec hardcoded verdict | `hook_exec.rs:241,354` — `DispatchAudit.verdict = "deny"` even for `FailurePolicy::Warn` |
| fork-stash temp store | `executor.rs:3202-3209` — `ArtifactStore::new(std::env::temp_dir())` |
| instruction-file starvation | `instructions.rs:79-82` — `push_file()` silently drops on 64 KB cap |
| workflow_id path validation | `workflows.rs:723` — `directory.join(format!("{workflow_id}.yaml"))`, no path-safety check |
| dead network-allowlist branch | `sandbox/executor.rs:1284-1288` vs `:566-572` — validation rejects, seatbelt generator unreachable |
| checksum trim asymmetry | `verify.rs:134` (trimmed) vs `:141` (untrimmed); `signing_digest` uses untrimmed |
| manifest id/version/publisher | `manifest.rs:781-789` — `.trim().is_empty()` only, no format validation |
| council Unicode Cf gap | `service.rs:1433-1435` — `char::is_control()` misses Unicode Cf category |
| agent.version path traversal | `acp_registry.rs:489-494` — `agent.version` only non-empty + ≤128 chars |
| idempotency false positive | `github/idempotency.rs:42-48` — whole-body scan for marker pairs |
| UiWorker::selection panic | `ui-host/runtime.rs:1927-1931` — `.expect()` panics pre-handshake |
| migration numbering gap | `migrations/` jumps `0019` → `0022`; no `0020` or `0021` |

---

## New findings not in the prior register

### N1: Flaky test under parallel load — `fix_ci_resolves_the_built_in_and_runs_end_to_end`

**File:** `crates/codypendentd/src/workflow_exec.rs:5498`

Under `cargo test --workspace` (all suites parallel), this test panics:

```
thread 'workflow_exec::tests::fix_ci_resolves_the_built_in_and_runs_end_to_end' panicked at
crates/codypendentd/src/workflow_exec.rs:5443:9:
run never reached Completed; last state Running
```

Root cause: `wait_for_run_state` (`workflow_exec.rs:5426-5447`) polls every 10 ms for 500
iterations (5 s ceiling). Under CPU contention from parallel test execution, a 7-node `/fix-ci`
orchestration graph cannot reach `Completed` within 5 s. The test **passes individually**
(`cargo test -p codypendent-codypendentd --lib fix_ci_resolves_the_built_in_and_runs_end_to_end`
→ 1.38 s, ok).

**Fix:** Increase the polling ceiling (e.g. 30 s), or make the test `#[serial]`, or use a
notification-based wait instead of polling.

### N2: Stray untracked files polluting the working tree

The following untracked paths are **not** in `.gitignore` and should be cleaned up or ignored:

| Path | Size | Content |
|---|---|---|
| `300726.txt` | 236 KB | Claude Code terminal transcript dump |
| `.cursor-tmp/` | ~780 KB + subdirs | Cursor editor cache (contains `tui-audit-findings.jsonl`) |
| `.idea/` | small | IntelliJ project files (has its own `.gitignore`) |
| `.poolside/settings.local.yaml` | 497 B | Poolside agent settings |

Recommended `.gitignore` additions: `*.txt` at repo root is too broad; instead add:
```
/300726.txt
/.cursor-tmp/
/.idea/
/.poolside/
```

### N3: `.poolside/` directory is un tracked and unignored

`.poolside/settings.local.yaml` exists with mode `0600` (good — sandbox settings only readable
by the owner) but is not in `.gitignore`. If it contains local sandbox configuration, it should
remain untracked. Confirm it is not meant to be committed.

### N4: No `EndSession` protocol command — confirmed

`CommandBody` in `protocol/src/command.rs` was grepped for `EndSession`/`ArchiveSession`/`CloseSession`
— all absent. `council/src/service.rs:1099-1103` has an explicit `TODO(protocol)` noting this and
that each council run leaves sessions behind. This is a known gap, not a latent bug.

---

## Verified sound (no action needed)

The following items from the prior register were spot-checked and confirmed correct:

- **Secret redaction** (invariant 6) — `AuthStore` has no derived `Debug`; hand-written `impl Debug`
  emits `<redacted>`; workspace-wide grep for `tracing` interpolating secrets returned one hit and
  it logs an *absence*.
- **Migration immutability** — all 38 migration files are additive (no `DROP`/rebuild/data-`UPDATE`).
  The `0003` comment-clarification incident is documented in `migrations/README.md`.
- **`CommandScope::allows_program`** is exact-string, never basename.
- **Sandbox is fail-closed** — `UnsupportedPlatform` off macOS/Linux, `RefusingSandbox` refuses
  everything, `validate_enforceable_profile` rejects non-empty `network_allowlist` on all platforms.
- **No `MutexGuard` held across `.await`** in non-test daemon code (mechanically scanned every
  `.lock()` within 8 lines of an `.await`).
- **TUI byte-offset slicing** — all `render.rs` offsets derive from `str::find`, which returns
  char boundaries.
- **Ownership gate** is compile-enforced: `named_resources()` is an exhaustive, wildcard-free match.
- **No missing index** on any hot query path.

---

## Recommendation

Do not commit the working tree as-is. The three P0 items (A1, A2, A3) are file-corrupting,
user-misleading, or correctness-affecting and must be resolved before the uncommitted changes
land on `main`. Follow the prioritized execution order in `README.md`.
