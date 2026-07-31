# TUI experience — in-app API keys, startup splash, chat header

**Status:** design approved 2026-07-30. Follow-up to the 4-PR program (A operability → B MCP client → C web-search + plan mode → D session ergonomics): operator-experience polish driven by dogfooding.

**Goal:** **D1** — set API keys from inside the TUI (hosted model keys AND the Tavily `web.search` key), never editing files or env by hand. **D2** — a splash while the TUI boots: today the user stares at a blank cooked terminal for up to 5s+ on daemon spawn/restart. **D3** — a persistent chat header bar (Claude Code / Kimi Code style).

**Non-goals (v1):** GitHub token management (stays `gh`-based), OAuth flows, MCP server editing, a CLI `keys` subcommand, per-run hot-reload of the Tavily key (restart-gated instead), splash on headless/`--jsonl` paths.

## Context (verified)

- **Key storage:** `auth.json` — `BTreeMap<model_id, {api_key}>`, mode 0600, redacting Debug. **Re-read by the daemon on every run** (`load_model_registry`), and `client_for` prefers it over env — so a client-side write takes effect on the next run with **no wire command** (matching the keep-secrets-off-the-wire invariant; the add-model `Intent` is client-only for the same reason). The add-model flow already captures masked keys (`SecretKey`, `render_masked_prompt`) and writes `auth.json` harness-side (`write_add_model`) — the machinery to reuse.
- **Tavily:** `TavilyKey::discover()` reads `TAVILY_API_KEY` from the daemon's env **at boot**. A client can't mutate daemon env → store under a reserved auth.json id + restart-gated effect (the DR7 idle-guarded restart machinery exists).
- **Startup flow:** crash hook → theme → `ensure_daemon` (up to 5s poll) → handshake → build-mismatch reconcile → session resolve → projections → `TerminalGuard::enter()` (first terminal touch) → first frame. Nothing renders before that; the reconcile `eprintln!` warnings assume cooked mode. `TICK = 200ms` + `state.tick` exist but nothing animates yet.
- **Layout:** `[Min(3) transcript, Length(composer), Length(1) status, Length(1) shortcuts]` — no top bar; one prepended `Length(1)` covers both chat and workspace branches. Theme tokens only (RULE 7: zero literal colors). `StatusProjection.model` exists but is unrendered; the daemon `build_id` is consumed by the reconcile and discarded.
- **Picker pattern:** PR C's `Overlay::ModePicker` (state/palette/reduce/render + tests) is the template for `/keys`.

## D1 — `/keys`: API keys in-app

1. `TavilyKey::discover()` gains an auth.json lookup ahead of env: `<data_dir>/auth.json` under a reserved collision-proof id (`integrations/tavily`) → then `TAVILY_API_KEY` env. Tests: file-first, env-fallback, absent.
2. New `Overlay::ApiKeys { query, selected }` + `PaletteCommand::ApiKeys` (ModePicker pattern): rows for every `models.toml` model with key status (`auth.json` entry / `api_key_env` var NAME shown, never the value / `missing`) and a `Tavily (web.search)` row. Enter → masked set/replace prompt; `d` → remove (with confirmation). Pure reducer + Intents; no I/O in the tui crate.
3. Harness drains the Intents: `auth.set/remove` + `save` (the `write_add_model` guards: load-before-write, atomic, 0600). Model key → "applies to the next run" notice. Tavily → "daemon picks it up on restart" notice + an offered idle-guarded restart (`commands::restart_daemon_if_idle`; `RefusedActive` → "applies on next restart").
4. Tests: reducer (list build, set/remove, masking), render (status symbols, NO key material ever), harness write helpers (temp data_dir), key.rs precedence.

## D2 — startup splash

5. `TerminalGuard::enter()` moves before `ensure_daemon`; a splash loop on a `tokio::time::interval` (~80ms) renders while boot proceeds: hand-drawn `CODYPENDENT` block-letter wordmark (no new deps), tagline, `v{version}+{sha}` (`codypendent_protocol::BUILD_ID`), and a stage line ("starting daemon…", "connecting…", "restoring session…", "loading workspace…") published by the boot steps through a `watch` channel. The reconcile path's stderr warnings become stage text (no cooked-mode prints inside the alt screen).
6. Flash guard: boot faster than the first tick → never shown; once shown, hold a minimum ~600ms. Same alt screen as the TUI — seamless transition, no teardown/re-enter.
7. Tests: pure `render_splash(state, w, h)` in `crates/tui` (stage text, version, narrow-width degradation); timing stays harness-side.

## D3 — chat header

8. Prepend `Constraint::Length(1)` to the top-level layout (both branches): `● codypendent · <session title> · <model> · <mode>` left, `<ctx%> · <cost>` right; theme tokens; width-tiered field dropping (the `render_status_line` tiers). Thread the daemon `build_id` from the handshake into `AppState` and show it at the full-width tier.
9. Tests: `render_to_string` at several widths (fields present/dropped; no secrets ever).

## Testing

Per-slice above; full workspace `fmt --check` / `clippy --all-targets` / `test`; vscode vitest (no protocol change expected — verify none). Smoke: real TUI boot (splash visible on daemon spawn), `/keys` set a model key + the Tavily key end-to-end against a temp data dir, header renders in chat and workspace layouts.

## Tasks

- **D1 `/keys`** — key.rs precedence + overlay + harness write/restart + tests.
- **D2 splash** — startup restructure + splash renderer + stage channel.
- **D3 header** — layout row + build_id threading + tiered render.

Controller-verifies by hand: no secret ever reaches a log/render/wire type; the cooked-mode warning elimination; the splash flash guard.
