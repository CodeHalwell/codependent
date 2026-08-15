# Adoption 12 — Architecture & Testing Adoptions

**Effort:** S–M per practice (independent) · **Depends on:** nothing (A1 helps 11/M3 and 11/M5 land safely) · **Status:** ⬜ not started
**Ported from:** codex (A1, A4, A5, A6), cline (A2, A3)

One section per practice: what it is, the reference, what codypendent does today
(**verified against the code**, not the review documents), concrete adoption
steps, acceptance criteria. House rules apply throughout
(`docs/docs/build/00-how-to-use-this-guide.md` §3): no `unsafe`, `-D warnings`,
migrations append-only, never weaken a security rule.

---

## A1 — vt100 screen-state snapshot testing

### What it is

Render tests that drive **real escape output** through a terminal emulator and
snapshot the resulting screen grid — so cursor addressing, scroll regions,
styling, and OSC side-channels are all exercised, not just ratatui's in-memory
`Buffer`. Codex has 655 `.snap` files on this foundation.

### Reference

`reference-repos/codex/codex-rs/tui/src/test_backend.rs` — `VT100Backend`
wraps `CrosstermBackend<vt100::Parser>`: every ratatui draw is serialized to
actual ANSI bytes by the crossterm backend, whose writer is a `vt100::Parser`;
`Display` renders `parser.screen().contents()` for `insta::assert_snapshot!`.
It deliberately avoids any crossterm call that touches the real stdout (size
and cursor position come from the vt100 screen). Codex pins
`vt100 = "0.16.2"`, `insta = "1.46.3"` (`codex-rs/Cargo.toml:488, 351`).

### Current state in codypendent (verified)

- `crates/tui` render tests use ratatui's plain `TestBackend` + direct buffer
  assertions (`render.rs:10207, 10251, 10343, 11092`) — cell-level truth, but
  nothing downstream of the backend (no ANSI serialization, no diffing, no
  escape sequences) is ever tested.
- `crates/test-support` is 20 lines: the JSONL `SessionEvent` fixture loader.
  It depends only on `codypendent-protocol` — it **cannot** host a ratatui
  backend without growing a `ratatui` dependency.
- Neither `vt100` nor `insta` is in the workspace.

### Adoption steps

1. Workspace `Cargo.toml`:

   ```toml
   [workspace.dependencies]
   vt100 = "0.16"
   insta = { version = "1", features = ["redactions"] }
   ```

2. Put the backend **in `crates/tui`** behind a feature, not in `test-support`
   (keeps `test-support` protocol-only, and lets `crates/cli` reuse it for
   harness tests):

   ```toml
   # crates/tui/Cargo.toml
   [features]
   vt100-tests = ["dep:vt100"]
   [dependencies]
   vt100 = { workspace = true, optional = true }
   [dev-dependencies]
   codypendent-tui = { path = ".", features = ["vt100-tests"] }
   insta = { workspace = true }
   ```

   New module `crates/tui/src/vt100_backend.rs` (`#[cfg(feature = "vt100-tests")]`),
   a port of codex's `VT100Backend` **adapted to ratatui 0.29's `Backend`
   trait**: methods return `io::Result<_>` directly (0.29 has no associated
   `Error` type) and the `scroll_region_up/down` methods do not exist in 0.29 —
   drop them. Keep `with_scrollback`, `vt100()`, the `Write` passthrough, and
   the `Display` impl verbatim; keep `crossterm::style::force_color_output(true)`
   so styling reaches the emulator even under CI's non-tty.

3. First customers (same PR, so the layer is proven):
   - re-express one existing full-frame render test
     (`render.rs`'s 80×24 chat frame) as
     `insta::assert_snapshot!(backend.to_string())`;
   - a styled assertion the plain `TestBackend` cannot make: the selected-row
     highlight uses `theme.selection` after a real ANSI round-trip
     (`screen().cell(r,c).fgcolor()`).
   - Snapshots live in `crates/tui/snapshots/` (insta default); `cargo insta
     review` documented in the crate README.

### Acceptance criteria

1. `cargo test -p codypendent-tui` runs the vt100 tests with no real terminal
   (CI-safe; no stdout probing — the codex "avoids calling any crossterm
   methods which write to stdout" property preserved).
2. At least two snapshot tests exist and `cargo insta test --check` (or plain
   `cargo test`) fails on a one-cell regression.
3. `test-support` remains dependency-light (no ratatui) — verified by its
   unchanged `Cargo.toml`.
4. Gotcha pinned in a comment: snapshot content includes trailing-space
   normalization from `screen().contents()`; never hand-edit `.snap` files.

---

## A2 — PTY end-to-end harness (design sketch)

### What it is

E2E tests that spawn the **real compiled TUI in a real PTY**, feed keys, and
assert against the **emulated screen state** — not the byte stream — so "old
content is gone" and "a dialog covered the view" are assertable facts. This is
cline's tuistory approach, which caught a promo dialog rendering over the chat
view that stream-grepping could never see.

### Reference

`reference-repos/cline/apps/cli/` — `vitest.tuistory.e2e.config.ts`,
`src/cli.tuistory.e2e.test.ts`, and `DEVELOPMENT.md` ("Manually testing the
TUI", lines 340–396): tuistory wraps the TUI in a named background PTY session
backed by a Ghostty terminal emulator; `wait "text" --timeout` resolves
**reactively** (no sleeps); `snapshot --trim` returns the current screen as
text; `screenshot` renders a styled PNG; `attach` lets a human watch the same
session; hermetic temp dirs per test. Explicitly framed as "the preferred way
for AI agents to poke at the interactive TUI."

### Current state in codypendent (verified)

- No e2e tier exists for the TUI. The CLI has connected-core unit seams
  (`run_over_connection` / `attach_over_connection` in
  `crates/cli/src/commands.rs`) but nothing drives the interactive TUI.
- Constraints an e2e run must satisfy: the TUI auto-starts/attaches a daemon
  (socket under `CODYPENDENT_DATA_DIR`, ~104-byte Unix path limit — build-guide
  §4), and a **run** requires a configured model (`models.toml` in the data
  dir), so v1 e2e sticks to UI-only flows that need no model.
- Adoption 09 introduces `portable-pty` to the workspace — the harness reuses
  it; `vt100` arrives with A1.

### Adoption steps (minimal v1 — keep it a sketch)

1. New crate `crates/tui-e2e` (not published, `[[test]]`-only), dev-deps:
   `portable-pty`, `vt100`, `insta`. Marked `#[ignore]` by default and driven
   by `CODYPENDENT_E2E=1` so the standard gate stays hermetic and fast.
2. Harness type — the terminal page object:

   ```rust
   pub struct TuiSession { pty: …, parser: vt100::Parser, child: Box<dyn Child> }
   impl TuiSession {
       /// Spawn `codypendent` with a fresh short tmp CODYPENDENT_DATA_DIR
       /// (socket-length safe), cols/rows fixed (120x36).
       pub fn launch(args: &[&str]) -> anyhow::Result<Self>;
       /// Pump PTY output into the vt100 parser until `needle` appears on the
       /// SCREEN (not the stream) or the deadline passes. Reactive: reads with
       /// small poll timeouts, no fixed sleeps.
       pub fn wait_for(&mut self, needle: &str, deadline: Duration) -> anyhow::Result<()>;
       pub fn type_str(&mut self, s: &str); pub fn press(&mut self, key: Key);
       /// Trimmed screen text for insta snapshots.
       pub fn snapshot(&mut self) -> String;
       /// Assert `needle` is NOT on the current screen (the "old content is
       /// gone" assertion class).
       pub fn assert_absent(&mut self, needle: &str);
   }
   ```

3. V1 test set (no model, no network):
   - boot → splash → chat frame appears; help overlay opens on its key and
     `assert_absent` proves it *closes*;
   - palette opens with `/`, filters, `Esc` restores the frame;
   - resize (PTY `resize()`) reflows without panic and the footer stays on the
     last row;
   - clean shutdown restores the terminal (parser sees alt-screen exit).
4. Later (out of v1): a scripted-model daemon profile to e2e a full run; a
   `codypendent-tuistory`-style dev binary for agents to drive interactively
   (cline's `bunx tuistory -s cline …` ergonomics).

### Acceptance criteria

1. `CODYPENDENT_E2E=1 cargo test -p codypendent-tui-e2e` passes the v1 set on
   macOS and Linux; without the env var the crate compiles and skips.
2. Every wait is reactive (poll-until-deadline); the word `sleep` appears in
   the harness only inside the poll loop's interval.
3. Each test gets a hermetic temp data dir; two tests can run concurrently
   without socket collisions (unique short dirs under `/tmp`).
4. One test demonstrates the screen-state advantage: an overlay's dismissal is
   asserted via `assert_absent`, which byte-stream grepping cannot prove.

---

## A3 — Provider VCR cassettes (integration tier)

### What it is

Record real model traffic once, replay it deterministically forever — with
secrets redacted — so integration tests exercise the *real* driver plumbing
(streaming, tool-call parsing, usage accounting) without network or model
nondeterminism.

### Reference

`reference-repos/cline/sdk/packages/shared/src/vcr.ts`: patches global `fetch`;
`CLINE_VCR=record|playback` + cassette path env vars; **key-based redaction**
(exact key names, key patterns, value regexes — "more robust than
regex-matching values"); path normalization so playback matches across
environments; SSE chunk replay with configurable delay; graceful-shutdown via
SIGINT so exit handlers flush cassettes; `initVcr()` at the very top of the
entrypoint.

### Current state in codypendent (verified)

- **Unit tier exists and is good**: `ScriptedDriver` (`crates/runtime/src/agent.rs:851`)
  — a queue of `ModelStep`s; the whole agent-loop test suite and
  `bench.rs` run on it. VCR is *not* a replacement; it is the tier above.
- The HTTP seam is **not** in `crates/providers` (the plan's assumption is
  wrong there): `codypendent-providers` is a *network-free* catalog/credential
  leaf crate (its own `Cargo.toml` description says so). Live traffic flows
  through `FrameworkModelDriver` in `codypendent-runtime` — the only
  `agent-framework-rs` consumer (ADR-009) — behind the `ModelDriver` trait
  (`agent.rs:800`: `async fn next_step(&self, transcript, tools, sink) ->
  StepOutcome`).
- Intercepting inside `agent-framework-rs`'s HTTP client is not practical from
  outside the dependency; the honest seam codypendent owns is `ModelDriver`.

### Adoption steps

Record/replay at the `ModelDriver` boundary — the codypendent equivalent of
cline's fetch patch (the outermost seam the codebase owns):

1. `crates/runtime/src/vcr.rs` (feature `vcr`, off by default):

   ```rust
   /// Wraps any live driver; records each (request fingerprint → outcome).
   pub struct RecordingDriver<D: ModelDriver> { inner: D, cassette: Mutex<Cassette>, path: PathBuf }
   /// Replays a cassette; panics with a diff on fingerprint mismatch
   /// ("re-record if the request change is intentional" — cline's message).
   pub struct CassetteDriver { cassette: Cassette, cursor: AtomicUsize }

   #[derive(Serialize, Deserialize)]
   pub struct Cassette { pub model_id: String, pub interactions: Vec<Interaction> }
   #[derive(Serialize, Deserialize)]
   pub struct Interaction {
       /// sha256 of (transcript items + tool definition names) — the request
       /// identity, cheap to diff and stable across path/user differences.
       pub fingerprint: String,
       /// Human-readable request summary for mismatch diagnostics.
       pub summary: String,
       pub deltas: Vec<String>,          // what the DeltaSink received, in order
       pub step: ModelStep,              // Say / CallTool / Finish (already serde)
       pub extra_calls: Vec<ToolCallRequest>,
       pub usage: Option<ModelUsage>,
   }
   ```

   `RecordingDriver::next_step` delegates, captures the sink stream through a
   tee, appends, and flushes the cassette on `Drop` (and on an explicit
   `flush()` — the SIGINT-flush lesson).
2. Env contract mirroring cline's:
   `CODYPENDENT_VCR=record|playback`, `CODYPENDENT_VCR_CASSETTE=<path>`.
   The codypendentd executor consults them when building the driver (a
   ~6-line branch where `FrameworkModelDriver` is constructed) — recording a
   cassette is then just running a real session once with the env set.
3. Redaction: cassettes contain **transcript content** (repo text), never
   credentials — the `ModelDriver` seam sees no API keys by construction
   (verified: keys are resolved inside `FrameworkModelDriver`/framework, not
   passed through `next_step`). Still apply cline's key-based scrub to the
   `summary` field and refuse to record when the transcript matches an obvious
   secret pattern (reuse the deny patterns from
   `codypendent_sandbox::sanitize`) — recording is for fixture repos, not
   client work.
4. Fixture home: `crates/runtime/tests/cassettes/*.json`; the integration test
   tier (`crates/runtime/tests/vcr_it.rs`) drives the **full agent loop**
   (policy, approvals auto-granted, real tools against a tempdir repo) on a
   `CassetteDriver` — everything real except the model.

### Acceptance criteria

1. A cassette recorded against a local Ollama model replays green with the
   network disabled (test asserts no `endpoint()` is ever dialed — the
   `CassetteDriver` returns `None`).
2. A deliberate transcript change fails playback with a message naming the
   step index, both fingerprints, and the re-record instruction.
3. `ScriptedDriver` suites are untouched (unit tier unchanged); the VCR tier
   is additive and feature-gated, contributing zero deps to default builds.
4. No cassette in the repo contains a string matching the sanitize deny
   patterns (a test iterates the fixture dir).

---

## A4 — Clippy `disallowed-methods` as architecture enforcement

### What it is

Encoding architectural rules ("colors come only from Theme tokens") as lints
so violations fail `-D warnings` CI instead of surviving until review.

### Reference

`reference-repos/codex/codex-rs/clippy.toml`: bans
`ratatui::style::Color::Rgb` and `Color::Indexed` ("Use ANSI colors, which
work better in various terminal themes"), `Stylize::white/black/yellow`, and —
the same technique for a different subsystem — every raw `sqlx` pool/connect
entry point ("Create SQLite pools through codex-state's sqlite shim"). Plus
`allow-expect-in-tests`/`allow-unwrap-in-tests` and
`await-holding-invalid-types` for tokio guards.

### Current state in codypendent (verified)

- `clippy.toml` **exists** at the repo root and contains exactly one knob:
  `large-error-threshold = 192` (with a long rationale comment). No
  `disallowed-methods`.
- The color rule already *holds* by convention (`theme.rs` module doc, STEP
  1.12 RULE 7) and an audit confirms the construction sites:
  `Color::Rgb`/`Color::Indexed` are built only in `crates/tui/src/theme.rs`
  (145 uses — the token definitions themselves), `theme_pack.rs:125` (hex →
  token parsing), and tests (`render.rs:10820` is a pattern-match,
  `reduce.rs:15481` / `cli/theme_select.rs:343` are test fixtures). Nothing
  enforces it.

### Adoption steps

Append to the existing `/Users/…/codypendent/clippy.toml` (keep the
`large-error-threshold` block):

```toml
allow-expect-in-tests = true
allow-unwrap-in-tests = true

await-holding-invalid-types = [
    "tokio::sync::MutexGuard",
    "tokio::sync::RwLockReadGuard",
    "tokio::sync::RwLockWriteGuard",
]

disallowed-methods = [
    { path = "ratatui::style::Color::Rgb",     reason = "Widgets read colors only from Theme tokens (STEP 1.12 RULE 7). Construct concrete colors in crates/tui/src/theme.rs or theme_pack.rs only." },
    { path = "ratatui::style::Color::Indexed", reason = "Widgets read colors only from Theme tokens. Construct concrete colors in theme.rs only." },
    { path = "ratatui::style::Stylize::white",  reason = "No hardcoded white; use theme.text tokens or modifiers." },
    { path = "ratatui::style::Stylize::black",  reason = "No hardcoded black; use theme tokens or modifiers." },
    { path = "ratatui::style::Stylize::yellow", reason = "No hardcoded yellow; use theme.status.warning." },
]
```

Then place the negotiated allows at the *definition* sites:

- `crates/tui/src/theme.rs` — module-level
  `#![allow(clippy::disallowed_methods)]` is too broad; instead annotate the
  seven variant constructors + `ColorDepth` helpers (or wrap the module body's
  impl blocks) with `#[allow(clippy::disallowed_methods)]` and a one-line
  comment naming this spec.
- `crates/tui/src/theme_pack.rs::parse hex` — one `#[allow]` at line ~125.
- Test fixtures (`reduce.rs`, `render.rs`, `cli/theme_select.rs`): the lint
  fires in test code too (there is no test exemption for
  `disallowed_methods`); add `#[allow]` on the three test fns.

Note on `await-holding-invalid-types`: audit first — `collect_output_until_deadline`
patterns (Adoption 09) and existing daemon code hold `tokio::sync::MutexGuard`
across awaits *by design* in a few places; if the audit finds load-bearing
cases, drop that table entry rather than sprinkling allows (the codex entry is
included above as the target state, not a precondition).

### Acceptance criteria

1. Introducing `Color::Rgb(1,2,3)` in `render.rs` fails
   `cargo clippy --workspace --all-targets -- -D warnings`.
2. The full workspace passes the gate with the new table (all existing sites
   either allowed-with-comment or migrated).
3. `clippy.toml` retains the `large-error-threshold` block unchanged.

---

## A5 — Module-doc discipline additions

### What it is

Codex's rule that every hard-won behavioral invariant lives in a module doc
*next to the code that enforces it* — with two specific instruments: (a) an
`AGENTS.md` per hot directory instructing agents to keep module docs in sync
with edits, and (b) **symptom→constant tuning guides** on modules whose
behavior is a set of tuned thresholds (`chunking.rs`: "lag starts too late ⇒
lower enter thresholds; smooth/catch-up chatter ⇒ increase hold windows…").

### Reference

`codex-rs/tui/src/streaming/chunking.rs` module doc (mental model + policy
flow + tuning order + symptom table); `codex-rs/tui/src/bottom_pane/AGENTS.md`
(keep the module docs in sync); `paste_burst.rs` / `terminal_title.rs`
(call-pattern contracts and threat models as docs).

### Current state in codypendent (verified — largely already followed)

Codypendent already writes design-document module docs; three verified
examples:

1. `crates/tui/src/lib.rs` — the full unidirectional-loop architecture diagram
   plus the numbered RULES the crate enforces (no I/O in widgets, mouse
   parity, theme tokens).
2. `crates/runtime/src/agent.rs` — every behavioral constant carries its
   rationale and evidence: `DELTA_COALESCE_WINDOW` (why 50 ms, what broke),
   `COMPACTION_THRESHOLD_PCT` (why 80, the silent-head-loss failure mode),
   `MAX_CONSECUTIVE_IDENTICAL_CALLS` (the evidence run that motivated 3).
3. `crates/cli/src/repo_anchor.rs` — a defect-history doc ("the 2026-08-13
   review found…") explaining why the module exists and pinning the invariant
   with a test.

(Also in this class: `tools/secure_fs.rs`'s TOCTOU rationale,
`daemon/executor.rs`'s dependency-inversion doc.) There are **no** per-directory
`AGENTS.md`/`CLAUDE.md` files anywhere under `crates/` (verified by find), and
no module currently uses the symptom→constant *table* form.

### Adoption steps (the delta only)

1. **Symptom→constant guides** — adopt the table form for modules whose
   behavior is tuned thresholds. First three targets:
   - the streaming pacing constants landed by Adoption 11/M3 (port codex's
     guide verbatim — this is a hard requirement of that item);
   - `agent.rs`'s compaction constants (`COMPACTION_THRESHOLD_PCT`,
     `COMPACTION_KEEP_RECENT_RESULTS`) — the rationale exists; add the
     symptom-oriented lines ("runs die at the context cliff ⇒ lower the
     threshold; model loses the observation it just requested ⇒ raise
     keep-recent");
   - Adoption 09's yield clamps.
2. **Doc-sync instruction files** — add a short `AGENTS.md` (≤ 15 lines) to
   the two directories where drive-by edits most often orphan docs:
   `crates/tui/src/` and `crates/runtime/src/` — content: "module docs here
   are normative design documents; an edit that changes behavior MUST update
   the module doc and the constant rationales in the same commit; constants
   with tuning guides are changed via the guide's symptom table, and the
   commit message names the symptom." Do **not** duplicate build-guide rules
   into them (rule 2 of §3 forbids competing sources of truth; link instead).
3. **Review checklist line** — add to the PR template (or create
   `.github/pull_request_template.md` if absent): "behavioral constants
   changed? → module-doc tuning guide updated."

### Acceptance criteria

1. The two `AGENTS.md` files exist, are ≤ 15 lines, and link to the build
   guide rather than restating it.
2. `agent.rs` compaction constants carry a symptom table; 11/M3's pacing
   module lands with its guide (cross-checked in that item's review).
3. No existing module doc is deleted or weakened by this change (docs-only PR).

---

## A6 — Generated client SDK from the protocol (design sketch)

### What it is

Make `crates/protocol` the single machine-readable source of truth: export
TypeScript types + JSON Schemas from the Rust wire types, and pin wire
compatibility with schema-fixture tests, so client codecs are generated, not
hand-mirrored.

### Reference

`codex-rs/app-server-protocol/`: `ts-rs` derives TS types, `schemars` derives
JSON Schemas (`Cargo.toml:49–55`); `export.rs` walks every
request/notification/response type and writes a generated tree
(`GENERATED_TS_HEADER: "// GENERATED CODE! DO NOT MODIFY BY HAND!"`);
`precomputed_exports.rs` + `schema_fixtures.rs` snapshot the generated
`typescript/` and `json/` trees into the repo and a test
(`schema_fixtures_tests.rs`) regenerates and diffs — any wire-affecting Rust
change fails CI until the fixtures (and thus the visible diff) are updated.

### Current state in codypendent (verified)

- `ROADMAP.md:651` flags exactly this: "**Generate the protocol SDK.** The VS
  Code extension hand-duplicates the Rust wire codec…".
- The duplication is real and substantial: `extensions/vscode/src/protocol/`
  — `types.ts` (497 lines) reproduces the serde contract **by hand**, with a
  header comment documenting serde's internally-tagged newtype flattening;
  `frame.ts` mirrors `framing.rs` (u32-BE length prefix, 16 MiB cap);
  `discovery.ts` mirrors `discovery.rs` including the `ProjectDirs` data-dir
  derivation per platform. It even hand-tracks the protocol-version history in
  a comment.
- `crates/protocol` is dependency-light (serde/serde_json/uuid/chrono/
  thiserror/tokio/directories) with `#[serde(tag = "type")]` internally-tagged
  enums throughout — a shape both `schemars` and `ts-rs` handle.
- Every wire type is `Serialize + Deserialize` already; `test-support`'s JSONL
  fixture (`fixture_events_jsonl` — "replay the exact historical bytes")
  is the existing, event-only wire-compat pin.

### Adoption steps (sketch level)

1. **Schema export** — feature-gate to keep the wire crate lean:

   ```toml
   # crates/protocol/Cargo.toml
   [features]
   schema-export = ["dep:schemars"]
   [dependencies]
   schemars = { version = "0.8", optional = true }
   ```

   Derive `#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]`
   on `Envelope`, `Payload`, `Command`/`CommandBody`, `SessionEvent`/`EventBody`,
   `Catchup`, the handshake types, and the id newtypes. A small
   `codypendent-protocol-export` bin (workspace `xtask`-style, or a
   `--features schema-export` example) writes `docs/specs/protocol-schema/*.json`
   (one file per root type) with codex's DO-NOT-MODIFY header.
2. **Fixture tests pinning wire compat** — two layers:
   - *Schema fixtures* (the codex mechanism): a test regenerates the schema
     tree in-memory and diffs it against the committed files; any additive
     field shows up as a reviewed diff, any breaking change is unmissable.
   - *Byte fixtures* (extend the existing mechanism): grow
     `crates/test-support/fixtures/` beyond events — one committed JSONL of
     historical `Envelope` frames per protocol minor (commands, replies,
     catch-up), with a round-trip test in `crates/protocol` that parses the
     exact bytes and re-serializes to an equivalent value. This is the layer
     that catches serde-attribute regressions schemas can't see
     (`skip_serializing_if` defaults, tag flattening).
3. **TypeScript generation** — from the JSON Schemas via
   `json-schema-to-typescript` in the extension's build (an npm dev step; no
   `ts-rs` in the Rust tree — one generator, one direction), emitting
   `extensions/vscode/src/protocol/generated/types.ts`. Migration is
   incremental: `types.ts` re-exports from `generated/` type-by-type; the
   extension's existing 217 vitest tests are the safety net. `frame.ts` and
   `discovery.ts` stay hand-written (they are behavior, not types) but gain
   fixture tests against the same byte fixtures.
4. **CI**: the schema-diff test runs in the normal `cargo test --workspace`;
   the extension build fails if `generated/` is stale (regenerate-and-diff npm
   script).

### Acceptance criteria

1. Renaming a `CommandBody` field fails a Rust test (schema diff) *and* the
   byte-fixture round-trip — before any client breaks.
2. The VS Code extension compiles with at least `EventBody` and `Payload`
   consumed from generated types; deleted hand-written lines ≥ added glue.
3. `codypendent-protocol` builds byte-identically without the
   `schema-export` feature (no new default deps — verified with
   `cargo tree -p codypendent-protocol`).
4. `docs/specs/protocol-schema/` is committed, headered as generated, and
   regenerating on an unchanged tree is a no-op (deterministic output —
   sort maps, fixed schema draft).
