# Operability bundle — `doctor`, `update`, `completion`

**Status:** design approved 2026-07-30. First of a 4-PR program adopting selected Codex/Claude-Code CLI features (A operability → B MCP client → C web-search + plan mode → D session ergonomics).

**Goal:** three small, high-ROI operator commands for the single-binary daemon: diagnose health, self-update (and pick up the new build via the auto-restart we just shipped), and emit shell completions. No protocol change, no daemon change beyond what already exists.

**Non-goals:** `-c key=value` / config profiles (dropped — Codypendent has no unified `config.toml`; config is split across `models.toml`/`providers.toml`/`auth.json` + env vars + flags, so a generic override is a solution without a problem). Being an MCP server, plugins, remote-control — later PRs or out of scope.

## Context (verified)

- CLI is clap 4 (derive). Subcommands are `TopCommand` variants in `crates/cli/src/main.rs`, dispatched in the `main()` match (each arm calls a `commands::*` fn). `-V/--version` already prints `CARGO_PKG_VERSION` via clap's `version` attribute.
- The single binary runs the daemon from itself via `resolve_daemon_binary()` → `DaemonInvocation` (`current_exe` + `__daemon`). `codypendent_protocol::BUILD_ID` is the per-build id (from the daemon-auto-restart work).
- Client helpers exist: `client::ping(socket) -> bool`, `client::daemon_status(socket) -> DaemonStatus` (carries `build_id`, `active_run_count`, uptime, pid, protocol_version), `commands::restart_daemon_if_idle(paths) -> IdleRestartOutcome` (idle-guarded restart, DR7).
- `RuntimePaths` (`crates/protocol/src/discovery.rs`): `data_dir`, `config_dir`, `run_dir`, `socket_path`, `pid_path`, `log_dir`. Model config at `<data_dir>/models.toml`; provider catalog at `<data_dir>/providers.toml` via `providers::Catalog::load_with_user_overrides(path)`; each provider carries `base_url: Option<String>`.
- `install.sh` is the canonical installer: detect target (`aarch64-apple-darwin` / `x86_64-apple-darwin` / `x86_64-unknown-linux-gnu`), resolve latest release tag via `gh release list`, `gh release download <tag> -p codypendent-<target>.tar.gz`, untar, macOS `xattr -dr com.apple.quarantine`, `install -m 0755` the `codypendent` (and optional `codypendentd`) binaries. The repo is **private**, so downloads authenticate through `gh`.
- `reqwest` (rustls, json) is available in the cli crate.

## 1. `codypendent doctor [--json] [--deep]`

Read-only diagnostic. Emits a checklist; each item is `ok` / `warn` / `fail` with a message and a one-line fix hint. Exit code: `0` if no `fail`, `1` if any `fail` (scriptable). `--json` prints a structured report instead of text. Never mutates anything.

Checks (in order):
1. **binary** — `current_exe()` path; `CARGO_PKG_VERSION` + `BUILD_ID`. Always `ok` (informational).
2. **daemon** — `client::ping`; if up, `daemon_status`: report `build_id`, `active_run_count`, uptime, pid. `warn` if the daemon's `build_id != BUILD_ID` ("a newer build is installed; it will auto-restart on next launch, or run `codypendent daemon restart`"). `warn` (not fail) if not running ("no daemon running; it starts on first use").
3. **paths** — `data_dir`, `config_dir`, `run_dir`, `log_dir` exist and are writable; `fail` on a non-writable data_dir with the path in the hint.
4. **model config** — `<data_dir>/models.toml` exists and parses; ≥1 `[[model]]`; the default/selected model resolves to a provider with a `base_url`. `fail` with the exact "create `<data_dir>/models.toml`" hint if absent (the known "model —" friction).
5. **providers** — for each provider referenced by a configured model: local `base_url` (loopback host) → a short TCP connect (default) or HTTP GET with `--deep`; hosted → API-key presence in `auth.json` (a live call only with `--deep`). `warn` on unreachable/keyless, never `fail` (offline is legitimate).

Design: a pure `Report { items: Vec<Check> }` with `Check { name, status: Status, message, hint: Option<String> }` and a pure text/JSON renderer, so the check-gathering (which does I/O) is separable from formatting and the renderer is unit-testable. Probes are bounded (≤2s each), never hang.

## 2. `codypendent update [--check] [<tag>]`

Self-update, mirroring `install.sh` but in-process.
- Resolve the target release tag: `<tag>` arg if given, else the newest release via `gh release list -R <repo> -L 1`.
- Compare against the running build: if the tag already matches the installed version, print "already up to date" and exit `0`.
- `--check`: print whether an update is available (and the tags), exit `0` (2 if available, for scripts) — **no download**.
- Otherwise: detect this machine's target, `gh release download <tag> -p codypendent-<target>.tar.gz` into a temp dir (always cleaned up), untar, macOS quarantine-clear, then `install -m 0755` over the directory of `current_exe()` (falling back to `sudo install` when that dir is not writable — same rule as `install.sh`). Verify the extracted `codypendent` binary exists and is executable before replacing anything.
- After a successful install: attempt `commands::restart_daemon_if_idle(paths)` so the new build is live immediately; on `RefusedActive` print "installed — the daemon will load the new build once the current run(s) finish, or on next launch." Never force-kill a run.
- Uses `gh` (checked on PATH with a legible error) for consistency with `install.sh` and to authenticate against the private repo. `REPO` and asset naming are shared constants with a note pointing at `install.sh`.

Design: a pure planner `decide_update(current_build: &str, latest_tag: &str) -> UpdatePlan { UpToDate | Available(tag) }` (unit-testable), and an effectful driver with the `gh`/`tar`/`install` steps behind small injectable seams (mirroring DR3's `restart_daemon_with`) so a test asserts the ordering and the up-to-date short-circuit without a real download. Target detection is a pure fn with per-`(os,arch)` unit tests.

## 3. `codypendent completion <shell>`

`clap_complete::generate` writes a completion script for `<shell>` (`bash` | `zsh` | `fish`) to stdout. Adds the `clap_complete` dependency (small, first-party to clap). The command uses the same `clap::Command` the app already derives, so completions never drift from the real CLI. Doc the per-shell install line in `--help`.

## Testing

- `doctor`: unit-test the renderer (text + json) against a hand-built `Report` covering ok/warn/fail; unit-test the model-config check against a temp `models.toml` (present/absent/malformed). Provider probes bounded; integration-light.
- `update`: unit-test `decide_update` (up-to-date vs available), target detection per `(os,arch)`, and the driver ordering via injected fakes (up-to-date short-circuits before any download; a refused restart still reports success).
- `completion`: a smoke test that each shell generates non-empty output containing the binary name.
- Full workspace `cargo fmt`/`clippy -D warnings`/`test`; no protocol/golden change (verify none).

## Tasks (subagent-driven)

- **A1 `completion`** — `clap_complete` dep; `Completion { shell }` subcommand + `commands::completion`; smoke tests.
- **A2 `doctor`** — `Report`/`Check`/`Status` + pure renderer (text/json) + check-gatherers (binary, daemon, paths, model-config, providers) + `Doctor { json, deep }` subcommand; renderer + model-config tests; exit-code wiring in `main.rs`.
- **A3 `update`** — `decide_update` + target detection + driver (gh/tar/install seams) + idle-guarded restart; `Update { check, tag }` subcommand; planner/target/driver tests.

Independent PR off `main`. Controller-verifies the `update` install path (it overwrites the running binary — the one destructive step) and confirms no protocol/golden/daemon change.
