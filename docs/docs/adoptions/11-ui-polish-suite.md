# Adoption 11 — UI Polish Suite

**Effort:** S–M per item (independent of each other) · **Depends on:** varies per item · **Status:** ⬜ not started
**Ported from:** codex (S1, S4, M1, M2, M3, M5), cline (S2, M2's prewarmed index), opencode (S3, S5, M4)

One document, one section per item. Every item follows the house rules
(`docs/docs/build/00-how-to-use-this-guide.md` §3): RULES are MUST-level, no
`unsafe`, `-D warnings`, colors only via `Theme` tokens, and the TUI crate stays
pure — **no I/O in `crates/tui`** (`crates/tui/src/lib.rs` module doc: the CLI
harness owns the protocol connection and the terminal; `reduce` and `render`
perform no I/O). Wherever an item needs the wall clock, the network, or the
terminal, that half lands in `crates/cli/src/tui.rs` (the harness) or in the
daemon, never in the reducer.

---

## S1 — Session/resume picker overlay

### Reference

`reference-repos/codex/codex-rs/tui/src/resume_picker.rs` (6 900 lines): paginated
thread listing over a typed protocol query (`ThreadListParams` — cwd filter, sort
key, page cursor), fuzzy search, preview pane. **Scaled down here**: no
pagination, no preview, no archive — a flat, filterable list of this
workspace's sessions.

### Current state (verified)

- `codypendent attach <SESSION_ID>` requires a literal session id
  (`crates/cli/src/main.rs:15,117`); there is no way to discover ids without
  reading the SQLite db by hand.
- The TUI binds one session per repository via a client-side
  `SessionStore` file (`resolve_or_create_session`, `crates/cli/src/tui.rs:5968`)
  — attach the remembered id, else create. Old sessions become unreachable.
- **No session-list query exists in the protocol** (verified against
  `crates/protocol/src/command.rs` — `CommandBody` has no list variant) even
  though the daemon has the table: `migrations/0001_init.sql` —
  `sessions (id, workspace_id, title, state, created_at, updated_at, revision)`.
- In-place session swap already exists: `Intent::NewConversation` →
  `create_fresh_session_live` (`crates/cli/src/tui.rs:2780, 5683`) reconnects
  and re-attaches without restarting the TUI. The picker reuses this machinery
  with an *existing* id.
- Overlays live in `enum Overlay` (`crates/tui/src/state.rs:212`); list-style
  overlays with a filter query + selected index have precedent
  (`Overlay::OnboardProviderPicker { class, query, selected }`).

### Changes

- **`crates/protocol/src/command.rs`** — new variant (additive; bump
  `PROTOCOL_V1` minor to 5 in `version.rs` with the usual doc paragraph):

  ```rust
  /// List sessions the daemon knows, newest-updated first.
  ListSessions {
      /// Restrict to one workspace; `None` lists all.
      #[serde(default, skip_serializing_if = "Option::is_none")]
      workspace: Option<WorkspaceId>,
      /// Hard cap on returned rows (the daemon also caps at 200).
      #[serde(default)]
      limit: Option<u32>,
  },
  ```

- **`crates/protocol/src/envelope.rs`** — reply payload:

  ```rust
  SessionList { command_id: CommandId, sessions: Vec<crate::command::SessionSummary> },
  ```

  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
  pub struct SessionSummary {
      pub session_id: SessionId,
      pub workspace_id: Option<WorkspaceId>,
      pub title: String,
      pub state: String,          // 'open' | 'closed' — the sessions.state column
      pub updated_at: DateTime<Utc>,
      pub created_at: DateTime<Utc>,
  }
  ```

- **`crates/daemon/src/commands.rs`** — handle `ListSessions` with a plain
  `SELECT … FROM sessions ORDER BY updated_at DESC LIMIT ?` (read-only; no
  events appended, no ledger sequence).
- **`crates/tui/src/state.rs`** — `Overlay::SessionPicker { query: String, selected: usize }`
  plus `AppState::session_list: Vec<SessionRow>` (id, title, state, updated-at
  display string).
- **`crates/tui/src/action.rs`** — `Action::SessionListLoaded(Vec<SessionRow>)`,
  `Intent::ListSessions`, `Intent::SwitchSession(SessionId)`.
- **`crates/tui/src/palette.rs` / `reduce.rs`** — palette command `Sessions`
  ("resume another conversation") opens the overlay and emits
  `Intent::ListSessions`; type-to-filter (case-insensitive substring, like the
  provider picker), `Enter` emits `Intent::SwitchSession(id)`, `Esc` closes.
- **`crates/cli/src/tui.rs`** — dispatch `Intent::ListSessions` as the command
  and fold the `SessionList` reply into `Action::SessionListLoaded`; handle
  `Intent::SwitchSession` exactly like `Intent::NewConversation` except the
  swap attaches the chosen existing id (factor `create_fresh_session_live`'s
  connect+attach tail into a shared helper); persist the swap in `SessionStore`
  so the next launch resumes it.
- **`crates/tui/src/render.rs`** — render the overlay with the standard
  modal frame: title row, filter line, rows `title · state · updated`, selected
  row via `theme.selection`.

### Acceptance criteria

1. Palette → "Sessions" lists every session for the current workspace, newest
   first, current session marked.
2. Selecting a session swaps the live TUI onto it (catch-up replays its
   transcript) without a process restart; the next `codypendent` launch resumes
   the chosen session.
3. A `closed` session renders but selecting it is refused with a status notice
   (the event loop would exit on folding `SessionClosed` — verified behavior of
   `catchup_shows_closed`).
4. Old daemons: a `CommandRejected`/unknown-command reply degrades to a notice,
   never a crash (negotiated minor < 5 ⇒ don't offer the palette entry).

### Gotchas

- The daemon cannot distinguish an absent session from an empty one at attach
  time (see the long comment in `resolve_or_create_session`); the picker must
  only offer ids that came from `ListSessions`, never free-typed ids.
- `updated_at` is a TEXT column of RFC-3339 strings; sort in SQL, not in the
  reducer, so ordering can't drift from the daemon's.
- Don't reuse `Overlay::OnboardProviderPicker`'s Esc-return-address machinery —
  the session picker's Esc simply closes.

---

## S2 — Context meter in the footer

### Reference

`reference-repos/cline/apps/cli/src/tui/components/status-bar.tsx` —
`createContextBar(used, total, width = 6)`: 6 `█` cells; filled =
`ceil(ratio*6)` clamped so the bar shows **≥ 1 cell as soon as usage is
non-zero** and **never all 6 until `used >= total`**; filled cells in an accent
fg, empty cells gray.

### Current state (verified)

The plumbing already exists end-to-end; only the visual is missing:

- Estimate: `estimate_request_tokens` (`crates/runtime/src/agent.rs:416`) — the
  4-chars/token heuristic over transcript + system prompt + tool definitions.
- Emission: `token_budget_event` (`agent.rs:444`) emits
  `EventBody::BudgetWarning { dimension: Tokens, used, limit }`, deduped per
  integer percent, only when the driver reports a known context window.
- Projection: `crates/tui/src/reduce.rs:1955–1965` folds it into
  `run.context_percent: Option<u16>`; the status projection exposes it
  (`state.rs:2984`).
- Render: `render_run_telemetry` (`crates/tui/src/render.rs:284–420`) already
  shows `ctx {pct}/{left}%` as text (compact `c:{pct}%` under 48 cols).

**No protocol change is needed** — the item's "specify how it reaches the TUI"
answer is: it already does, via `BudgetWarning{Tokens}`.

### Changes

- **`crates/tui/src/render.rs`** — pure helper + use in the telemetry row:

  ```rust
  /// cline-style 6-cell block meter. `None` (unknown window) renders "──────"
  /// in `text.muted` — never a fabricated 0%.
  fn context_meter(percent: Option<u16>) -> (String /*filled*/, String /*empty*/) {
      const CELLS: u16 = 6;
      let Some(p) = percent else { return (String::new(), "─".repeat(CELLS as usize)); };
      let p = p.min(100);
      let filled = if p == 0 { 0 } else if p >= 100 { CELLS }
                   else { ((p * CELLS).div_ceil(100)).clamp(1, CELLS - 1) };
      ("█".repeat(filled as usize), "█".repeat((CELLS - filled) as usize))
  }
  ```

  Filled cells colored by severity: `theme.status.info` below 60 %,
  `theme.status.warning` 60–84 %, `theme.status.error` ≥ 85 % (85 ≈ five points
  past the 80 % compaction threshold, `COMPACTION_THRESHOLD_PCT`); empty cells
  `theme.text.muted`. The meter replaces the leading part of the existing
  `ctx …` telemetry item in both the ≥ 48-col row and (as `c:` + bar) the
  compact tier.

### Acceptance criteria

1. With no `BudgetWarning` yet received (or unknown window), the slot shows the
   dashes — the honesty rule in `reduce.rs`'s test at line 8084 stays intact.
2. 1 % usage shows exactly one filled cell; 99 % shows five; 100 % shows six.
3. Colors come only from `Theme` tokens (RULE 7); the render test tier
   (`render.rs` `TestBackend` tests) pins the three severity bands.

### Gotchas

- `context_percent` is per-run; the footer must read the *selected* run's value
  (it already does via `state.status()`).
- Don't double-draw: remove the numeric percent only in the compact tier; keep
  the `used/left/window` text at ≥ 160 cols — operators asked for numbers.

---

## S3 — "Did you mean" ENOENT handler

### Reference

`reference-repos/opencode/packages/opencode/src/tool/read.ts` lines ~76–99
(`miss()`): on file-not-found, read the parent directory, keep entries where
`entry.toLowerCase().includes(base)` **or** `base.includes(entry)`, take 3,
and fail with `File not found: {path}\n\nDid you mean one of these?\n{items}`.

### Current state (verified)

`crates/runtime/src/tools/read_file.rs` — `ReadFile::execute` opens through
`secure_fs::open_read` (descriptor-relative, `O_NOFOLLOW` per component); a
missing file surfaces as `ToolError::Io(NotFound)` from the leaf `openat`
(`secure_fs.rs:92–101`), so the model sees a bare `i/o error: No such file or
directory` with no scent of what *does* exist.

### Changes

- **`crates/runtime/src/tools/mod.rs`** — new structured variant so the message
  is typed, not formatted ad hoc:

  ```rust
  /// The file does not exist; `suggestions` are up to three same-directory
  /// entries whose names resemble the requested leaf.
  #[error("file not found: {path}{}", render_suggestions(suggestions))]
  FileNotFound { path: PathBuf, suggestions: Vec<String> },
  ```

  with code `"tool.file-not-found"` in `ToolError::code`.

- **`crates/runtime/src/tools/read_file.rs`** — catch the NotFound from
  `open_read`; compute suggestions with a scope-respecting sibling scan:

  ```rust
  /// Up to 3 entries of `path`'s parent whose lowercase name contains the
  /// requested leaf's lowercase name or vice versa. The parent is re-checked
  /// against `scope` and read with std::fs::read_dir on the RESOLVED parent;
  /// any error yields no suggestions (the not-found error stands alone).
  fn did_you_mean(path: &Path, scope: &PathScope) -> Vec<String>
  ```

  RULES: (1) never suggest across the scope boundary — if the parent classifies
  anything but `Allowed`, return empty; (2) suggestions are file *names*, not
  absolute paths (don't leak sibling tree structure beyond the directory the
  model already named); (3) cap 3, deterministic order (sorted).

### Acceptance criteria

1. Reading `src/mian.rs` where `src/main.rs` exists returns
   `file not found: src/mian.rs — did you mean: main.rs?` (formatting via the
   thiserror display; exact text pinned by a unit test).
2. A miss in a directory outside the scope returns the plain not-found error
   with zero suggestions.
3. Existing behavior for present files, ranges, FIFOs, and symlinks unchanged.

### Gotchas

- The suggestion scan is a *convenience* read outside the `O_NOFOLLOW` chain;
  it must therefore only ever *list names* (never open/stat contents) and only
  under an `Allowed` parent — otherwise it becomes a scope-probe oracle.
- `spawn_blocking` for the `read_dir` (consistent with `open_read`'s worker).

---

## S4 — Terminal title + focus-gated notifications

### Reference

- `reference-repos/codex/codex-rs/tui/src/terminal_title.rs`: OSC 0 with BEL
  terminator; sanitization strips control chars **and** the Trojan-Source
  bidi/invisible set (`U+00AD, U+034F, U+061C, U+180E, U+200B–200F,
  U+202A–202E, U+2060–206F, U+FE00–FE0F, U+FEFF, U+FFF9–FFFB, U+1BCA0–1BCA3,
  U+E0100–E01EF`), collapses whitespace, bounds to 240 chars; typed
  `NoVisibleContent` outcome instead of writing an empty title.
- `codex-rs/tui/src/notifications/`: OSC 9 (`\x1b]9;{msg}\x07`) on terminals
  known to support it (Ghostty, iTerm2, Kitty, Warp, WezTerm — detected, not
  probed), tmux DCS passthrough (`\x1bPtmux;\x1b\x1b]9;…\x07\x1b\\` with ESC
  doubling), BEL fallback elsewhere; **suppressed while the terminal is
  focused** (FocusChange tracked).

### Current state (verified)

- `crates/tui/src/terminal.rs` (92 lines) is the crate's only terminal touch:
  `TerminalGuard` enters raw mode + alt screen + mouse + bracketed paste. No
  title writes, no notifications, **no `EnableFocusChange`**.
- `crates/tui/src/input.rs:266` maps `Event::FocusGained | Event::FocusLost` to
  `Action::NoOp`.
- Precedent for raw escape output from the harness: OSC 52 clipboard copy
  (`crates/cli/src/tui.rs:668`).

### Changes

- **`crates/tui/src/terminal.rs`** — add (all synchronous, std-only, matching
  the module's charter):
  - `pub fn sanitize_terminal_title(title: &str) -> String` — port codex's
    function verbatim including the codepoint table and the 240-char bound.
  - `pub fn set_terminal_title(title: &str) -> io::Result<TitleOutcome>` /
    `pub fn clear_terminal_title()` — OSC 0 + BEL via a crossterm `Command`
    (port `SetWindowTitle`); `TitleOutcome::{Applied, NoVisibleContent}`.
  - `pub fn notify(message: &str, method: NotifyMethod) -> io::Result<()>` with
    `enum NotifyMethod { Osc9 { tmux_passthrough: bool }, Bel }` — port the two
    write paths including tmux ESC-doubling.
  - `pub fn detect_notify_method() -> NotifyMethod` — decide from
    `TERM_PROGRAM`/`TERM`/`TMUX` env (ghostty, iTerm.app, kitty, WarpTerminal,
    WezTerm ⇒ Osc9; `TMUX` set ⇒ passthrough wrapper; else Bel). Env reads are
    process-local and sync — acceptable in this one privileged module, exactly
    like `ColorDepth::detect()` in `theme.rs`.
  - `TerminalGuard::enter`: add `event::EnableFocusChange` (and the disable in
    both teardown paths); `Drop` additionally calls `clear_terminal_title()`.
- **`crates/tui/src/input.rs`** — map focus events to
  `Action::TerminalFocus(bool)` (replacing the `NoOp` arm).
- **`crates/tui/src/state.rs` / `reduce.rs`** — `AppState.terminal_focused: bool`
  (default `true`); the reducer records it and, on the events below, pushes a
  client-local `Intent::Notify { message: String }` into the outbox **only when
  unfocused**:
  - a pending approval arrives (`ApprovalRequested` folded while
    `pending_approvals` transitions 0→1): "Codypendent needs approval";
  - the selected run reaches a terminal state: "Run complete" / "Run failed".
- **`crates/cli/src/tui.rs`** — drain `Intent::Notify` → `terminal::notify(...)`
  with the method detected once at startup; set the title on session attach and
  on run-state changes: `codypendent — {session title} {spinner state}` through
  `sanitize_terminal_title` (title text includes the session title, which is
  derived from a directory name — untrusted enough to sanitize).

### Acceptance criteria

1. Titles containing `\x1b`, newlines, or bidi controls render with those
   characters stripped (unit tests port codex's four cases, including the
   pending-space truncation case).
2. No notification is emitted while `terminal_focused == true`; unfocusing then
   triggering an approval emits exactly one OSC 9 (or BEL) write.
3. Under `TMUX`, the emitted bytes are the DCS-wrapped form with doubled ESC
   (byte-exact unit test on the `Command::write_ansi` output).
4. Exiting the TUI (including panic-unwind through `Drop`) clears the title.

### Gotchas

- Focus events only arrive after `EnableFocusChange`; without the guard change
  the reducer would never see `TerminalFocus(false)` and notifications would
  never fire — wire both or neither.
- Crossterm may deliver an initial synthetic focus event on some terminals;
  defaulting `terminal_focused = true` makes a missing initial event safe.
- Never notify from the reducer directly (no I/O); the `Intent` outbox is the
  sanctioned channel (same pattern as every daemon command).
- OSC 9 on unknown terminals can print garbage — the allow-list is load-bearing;
  default to BEL, not to OSC 9.

---

## S5 — Agent-aware truncation hints

### Reference

`reference-repos/opencode/packages/opencode/src/tool/truncate.ts`: when output
exceeds limits, the full text is written to a spill file and the preview ends
with an *instructional* hint — and the hint is **agent-aware**: an agent that
can delegate gets "Use the Task tool to have explore agent process this file …
Do NOT read the full file yourself — delegate to save context"; otherwise "Use
Grep … or Read with offset/limit".

### Current state (verified)

`crates/runtime/src/tools/shell.rs` + `salient.rs`: overflow already spills to
the artifact store (`spill()` → `ArtifactSink`) and the model-facing
`SalientView::render()` cites the artifact **as data only**:
`--- stdout: 5000 lines, 212992 bytes (truncated), artifact 01H… sha256:ab12… ---`.
There is no instruction telling the model *what to do about it*; the
rehydration tool exists (`artifact.read`, offered whenever
`self.artifacts.is_some()` — `agent.rs::offered_tool_names`), and the fold stub
(`folded_result_stub`) already demonstrates the instructional style
("reopen it with artifact.read").

### Changes

- **`crates/runtime/src/tools/salient.rs`** — render takes capability context:

  ```rust
  /// What the current run can actually do about a truncated stream — decided
  /// by the agent loop (which knows the offered tool set), not by the tool.
  #[derive(Debug, Clone, Copy)]
  pub struct RetrievalHint {
      /// `artifact.read` is offered to this run.
      pub artifact_read: bool,
  }

  impl SalientView {
      pub fn render(&self) -> String { self.render_with_hint(RetrievalHint { artifact_read: false }) }
      pub fn render_with_hint(&self, hint: RetrievalHint) -> String { … }
  }
  ```

  `render_with_hint` appends, **only when at least one stream is
  `truncated || overflowed`**, one trailing block:
  - with `artifact_read`: `full output is preserved: call artifact.read
    {"artifact_id":"<id>"} to page through it — do not ask for it to be inlined
    and do not re-run the command to see more.`
  - without: `output was truncated at capture; re-run with a narrower command
    (grep/head) instead of re-running the same command.`

- **`crates/runtime/src/agent.rs`** — the two render sites
  (`execute_prepared`'s `PreparedTool::Shell` arm at ~3947, and the
  `RepositoryTest` arm) call
  `outcome.salient.render_with_hint(RetrievalHint { artifact_read: self.artifacts.is_some() })`.

### Acceptance criteria

1. A truncated `shell.run` observation ends with the artifact.read instruction
   when a reader is wired, and with the narrow-the-command instruction when not.
2. Non-truncated output renders byte-identically to today (no hint block) —
   pinned by updating the existing `salient.rs` unit tests plus one new case
   per hint branch.
3. The hint names the *actual* artifact id of the stream it refers to (prefer
   stdout's; fall back to stderr's when only stderr spilled).

### Gotchas

- Do not put the hint inside `SalientStream.lines` — it would be double-counted
  as an "error line" by future passes and would survive folding; it belongs to
  the rendered view only.
- `render()` is also called from fold stubs and tests; the zero-arg wrapper
  keeps every existing call site compiling with unchanged output.

---

## M1 — Paste placeholders + paste-burst detection

### Reference

`reference-repos/codex/codex-rs/tui/src/bottom_pane/paste_burst.rs` — a pure
state machine for terminals **without bracketed paste**: rapid plain-char
streams (interval ≤ 8 ms, ≥ 3 chars) are buffered and flushed as one paste;
first-ASCII-char hold suppresses flicker; retro-capture pulls already-inserted
chars back out; an Enter-suppression window (120 ms) keeps mid-paste newlines
from submitting. Large pastes render as `[Pasted Content N chars]` placeholder
elements in the composer, expanded at submit.

### Current state (verified)

- Bracketed paste is enabled by `TerminalGuard` and handled end-to-end:
  `input.rs::sanitized_paste` (normalizes newlines, expands tabs, strips
  control/bidi, 64 KiB bound) → `Action::InputPaste(String)` →
  `reduce.rs:711` inserts at the cursor.
- The composer is a plain `String` + cursor in `AppState`; there are no
  placeholder elements.
- The input side is a stateless pure function (`map_event`); the only stateful
  pure layer is the reducer, which has no wall clock (only event timestamps and
  `Action::Tick` every 200 ms) — too coarse for 8 ms burst timing.

### Changes

Split the item along the purity boundary:

**(a) Placeholders for large pastes — reducer-owned, pure.**

- `crates/tui/src/state.rs`:

  ```rust
  /// A large paste held out of the composer text. `marker` is the exact
  /// placeholder substring present in `input`; `text` is the full payload,
  /// re-expanded at submit.
  pub struct PasteBlock { pub marker: String, pub text: String }
  // AppState: pub pasted_blocks: Vec<PasteBlock>,
  ```

- `crates/tui/src/reduce.rs` — in the `Action::InputPaste` arm: if the paste
  has ≥ 5 lines or ≥ 1 KiB, insert `[Pasted #{n}: {lines} lines]` as the marker
  (n = 1-based ordinal) and push the block; otherwise inline as today. RULES:
  (1) cursor motion treats a marker atomically — left/right jump over it,
  backspace/delete anywhere inside removes the whole marker and its block;
  (2) submit (`NewRun`/steering/follow-up building) expands markers back to
  full text in ordinal order before the Intent is emitted; (3) clearing the
  composer clears the blocks.
  Implementation note: recover marker ranges by substring search of each
  `marker` in `input` at edit time (markers are unique via the ordinal), so no
  parallel offset bookkeeping can drift.

**(b) Burst detection for non-bracketed terminals — harness-owned.**

- Port `PasteBurst` (the pure struct + its unit tests) into
  `crates/cli/src/tui.rs`'s input-bridge thread (the blocking thread that reads
  crossterm events — it owns real time legitimately). Feed it plain chars; when
  it flushes `FlushResult::Paste(s)`, synthesize `Event::Paste(s)` into the
  channel so the reducer path is *identical* for both paste sources. Keep
  codex's constants (`PASTE_BURST_MIN_CHARS = 3`, char interval 8 ms, active
  idle 60 ms non-Windows, Enter window 120 ms). Retro-capture is **omitted** —
  chars are held before forwarding, so nothing is ever inserted prematurely
  (simpler than codex because our bridge sits before the reducer, not inside a
  textarea).

### Acceptance criteria

1. Pasting 200 lines shows one `[Pasted #1: 200 lines]` marker; submitting
   sends the full 200 lines; backspace on the marker removes all of it.
2. Two pastes produce two independently deletable markers; deleting #1 keeps
   #2's expansion correct.
3. With bracketed paste artificially disabled, a simulated 50-char burst at
   ≤ 8 ms spacing arrives as a single `Action::InputPaste`, and a mid-burst
   Enter does not submit (unit-test the ported state machine directly plus one
   bridge-level test with synthetic timestamps).
4. Reducer tests (pure) cover marker atomicity without any timing.

### Gotchas

- The 64 KiB `MAX_PASTE_BYTES` bound still applies before placeholder logic —
  a marker must report the *kept* size, and the truncation must remain
  UTF-8-safe (existing `sanitized_paste` test at `input.rs:830`).
- Markers must be excluded from slash-command detection (`/` on empty composer)
  and from history recall equality checks.
- `flush_if_due` uses `>` not `>=`; the bridge's tick must overshoot by ≥ 1 ms
  (codex's `recommended_flush_delay`).

---

## M2 — @-file-mentions with fuzzy index + Ctrl+R history search

### Reference

- `codex-rs/tui/src/bottom_pane/file_search_popup.rs`: `display_query` vs
  `pending_query` with stale-result rejection; waiting state; rows rendered
  with match indices highlighted.
- `codex-rs/file-search/`: background search over an `ignore` walker scored by
  **nucleo** (`file-search/Cargo.toml:23–24`).
- cline: the file index is **prewarmed** at startup so the first `@` is instant;
  Ctrl+R reverse-incremental history search lives in the composer
  (`chat_composer.rs` two-tier history).

### Current state (verified)

- The TUI does no I/O (module doc, `crates/tui/src/lib.rs`) — the index and the
  walk must live daemon-side; the TUI only sends a query intent and folds
  results.
- No file-name search exists anywhere: `workspace.search` is content search via
  ripgrep (a runtime tool for the *model*, not the composer).
- No composer history recall exists (no `↑`-recall of past prompts in
  `reduce.rs`).
- `ignore` and `nucleo` are not workspace deps.

### Changes

- **Workspace `Cargo.toml`**: `ignore = "0.4"`, `nucleo-matcher = "0.3"`
  (matcher only — scoring a cached path list needs no background thread pool;
  the daemon's tokio tasks provide the async boundary). Add both to
  `crates/daemon/Cargo.toml`.
- **`crates/daemon/src/file_index.rs` (new)** — per-repository cached index:

  ```rust
  pub struct FileIndex { roots: Mutex<HashMap<PathBuf, CachedWalk>> }
  struct CachedWalk { paths: Arc<Vec<String>>, built_at: Instant }
  impl FileIndex {
      /// Walk `root` with ignore::WalkBuilder (gitignore on, hidden off,
      /// max 50_000 entries), relative UTF-8 paths. Cached 30 s; `prewarm`
      /// refreshes in the background (called from AttachSession handling,
      /// beside ensure_repository_scanned).
      pub async fn query(&self, root: &Path, query: &str, limit: usize) -> Vec<FileMatch>;
      pub fn prewarm(self: &Arc<Self>, root: PathBuf);
  }
  pub struct FileMatch { pub path: String, pub indices: Vec<u32>, pub score: u32 }
  ```

  Scoring: `nucleo_matcher::Matcher` with `Pattern::parse(query, CaseMatching::Smart)`,
  keep top `limit` (default 8) by score, tie-break shorter-path-first.
- **Protocol** (same minor bump as S1): `CommandBody::SearchWorkspaceFiles
  { repository: String, query: String, limit: Option<u32> }` and
  `Payload::FileSearchResults { command_id, query: String, matches: Vec<FileMatchWire> }`
  — the echoed `query` is what makes client-side stale-rejection possible.
- **`crates/daemon/src/commands.rs` / `server.rs`** — handle the command
  read-only against the `FileIndex` (held beside the other server singletons).
- **TUI composer** (`state.rs`, `reduce.rs`, `render.rs`, `action.rs`):
  - typing `@` in the composer opens `MentionPopup { query, selected, waiting,
    display_query, matches }` state on `AppState` (not an `Overlay` — it must
    coexist with `InputMode::Composer`, like codex's bottom-pane popup);
  - each keystroke updates `query` and emits `Intent::SearchFiles { query }`
    (the harness debounces 50 ms before dispatch);
  - `Action::FileSearchResults { query, matches }` folds **only if
    `query == pending_query`** (stale rejection, exactly the
    `display_query`/`pending_query` split of the reference);
  - `Tab`/`Enter` replaces the `@token` with the selected path; `Esc` closes;
    match indices render bold via a span split (no new theme tokens).
  - **Ctrl+R history**: `AppState.prompt_history: Vec<String>` — every
    submitted composer text is pushed (deduped against the last entry).
    Ctrl+R opens `HistorySearch { query, selected }` popup filtering history by
    case-insensitive substring, newest first; Enter loads the entry into the
    composer; Ctrl+R again cycles older matches. Session-local only (no
    persistence — the harness owns files, and cross-session history is a
    follow-up).

### Acceptance criteria

1. `@comp` in a repo containing `crates/tui/src/palette.rs` lists fuzzy matches
   with highlighted match positions within 1 round-trip; results for a stale
   (superseded) query are never displayed.
2. Selecting a match inserts the repo-relative path at the `@` position.
3. The first `@` after attach is served from the prewarmed cache (no walk in
   the command handler's hot path — assert via a daemon unit test that a second
   query within 30 s does not re-walk).
4. Ctrl+R with three submitted prompts finds a middle one by substring, loads
   it, and leaves history order unchanged.
5. Reducer tests drive the popup entirely with synthetic actions (pure).

### Gotchas

- `.gitignore` semantics come from `ignore`'s defaults; explicitly enable
  `require_git(false)` so non-git repos still index.
- Cap walk size and per-path length — a `node_modules` accident must degrade
  (truncated index + a `truncated: bool` on the reply) rather than stall the
  daemon.
- The popup must not swallow `Enter` when no row is selected-by-navigation
  (codex: Enter with popup open but untouched still submits the message — pick
  the simpler rule: popup-open ⇒ Enter completes; document it in the help
  overlay).
- Match indices are byte indices from nucleo over UTF-8; convert to char
  indices before span-splitting (`char_indices`), or bold offsets drift on
  non-ASCII paths.

---

## M3 — Streaming pipeline: newline gating + two-gear pacing + table holdback

### Reference

- `codex-rs/tui/src/markdown_stream.rs`: `MarkdownStreamCollector` — buffer
  deltas, expose commit boundaries only at `\n`; re-render from one accumulated
  source string.
- `codex-rs/tui/src/streaming/chunking.rs`: two gears — `Smooth` (1 line/tick)
  vs `CatchUp` (drain all), hysteresis: enter at depth ≥ 8 **or** oldest ≥
  120 ms; exit at depth ≤ 2 **and** oldest ≤ 40 ms held 250 ms; 250 ms
  re-entry cooldown; severe bypass at depth ≥ 64 or ≥ 300 ms. Its module doc
  carries the symptom→constant tuning guide (port it).
- `codex-rs/tui/src/streaming/table_holdback.rs`: one-line-lookbehind,
  fence-aware scanner: a table-header-looking line ⇒ `PendingHeader`; header +
  delimiter ⇒ `Confirmed { table_start }` — content from the header onward
  stays in the mutable tail until finalization.

### Current state (verified)

- Deltas: the runtime coalesces stream chunks for 50 ms or until a newline
  (`DELTA_COALESCE_WINDOW`, `agent.rs:221`) into `ModelStreamDelta` events; the
  reducer appends them into the last `TranscriptEntry::Model`
  (`AppState::append_model_text`, `state.rs:3107`) and **clears the rich cache**
  — the streaming tail renders *plain* every frame; markdown parses once at
  finalization (`rendered: Option<Vec<RichLine>>`).
- `Action::Tick` fires every 200 ms (`crates/cli/src/tui.rs:79`).
- `crates/tui/src/markdown.rs` parses a whole source string at a width
  (`parse(text, width)`); the cache is width-keyed
  (`state.rs:2474–2480`).

So today there is no pacing (text appears in 50 ms lumps), no committed/tail
split (headings flash from plain to styled at finalization), and half-streamed
tables render as raw pipes.

### Changes

- **`crates/tui/src/state.rs`** — extend the streaming entry:

  ```rust
  TranscriptEntry::Model {
      text: String,                       // full accumulated source (unchanged)
      committed_len: usize,               // bytes of `text` promoted to rich rendering
      queue: VecDeque<PendingLine>,       // newline-complete lines not yet committed
      rendered: Option<Vec<RichLine>>,    // cache of parse(&text[..committed_len])
      gear: StreamGear,                   // Smooth | CatchUp (+ hysteresis stamps)
      holdback: TableHoldback,            // ported scanner state
  }
  struct PendingLine { end_offset: usize, arrived: DateTime<Utc> }
  ```

  (Wrap the enum variant's new state in a `StreamingModel` box if the variant
  grows unwieldy — the enum is `PartialEq` and cloned in tests.)
- **`crates/tui/src/reduce.rs`**:
  - `ModelStreamDelta` arm: append text (unchanged cap logic), then push a
    `PendingLine` for every newline the delta completed, feed the new source
    bytes to the holdback scanner. No commit here — arrival never renders
    directly.
  - `Action::Tick` arm: run the ported `AdaptiveChunkingPolicy::decide` over
    `(queue.len(), now - queue.front().arrived)` using the tick event's wall
    time; commit 1 line (`Smooth`) or the whole queue (`CatchUp`) by advancing
    `committed_len` — but never past `holdback.table_start` while a table is
    pending/confirmed. After advancing, rebuild `rendered =
    Some(markdown::parse(&text[..committed_len], width))`.
  - Finalization (`RunStateChanged` to terminal / next entry pushed): flush the
    queue, clear holdback, commit everything (existing full-parse behavior).
  - Constants: port codex's six thresholds **and the module-doc tuning guide
    verbatim** ("lag starts too late ⇒ lower enter thresholds; smooth/catch-up
    chatter ⇒ increase holds or tighten exits; eager re-entry ⇒ increase
    re-entry hold"), adjusted for the coarser tick: with a 200 ms tick,
    `Smooth` = 1 line per tick ≈ 5 lines/s — raise the tick to **80 ms**
    (`TICK` in `crates/cli/src/tui.rs`) as part of this item and note that the
    spinner/elapsed displays already tolerate any tick rate.
- **`crates/tui/src/render.rs`** — the Model entry renders
  `rendered` (rich) + `&text[committed_len..]` (plain tail), replacing the
  all-plain streaming path. Settled content never re-renders plain — the
  heading-flash fix, same effect as cline's block-identity reuse.
- **`crates/tui/src/markdown.rs`** — add the table-detect helpers the scanner
  needs (`is_table_header_line`, `is_table_delimiter_line`, fence tracking for
  \`\`\` blocks) as pure functions with unit tests (port shape from codex's
  `table_detect.rs`).

### Acceptance criteria

1. A steady stream renders line-by-line (typewriter) instead of 50 ms lumps; a
   burst of 40 lines drains within two ticks (CatchUp) and returns to Smooth
   only after the hysteresis hold.
2. Committed text is styled markdown while streaming; the mutable tail is
   plain; **finalized output is byte-identical to today's full parse** (pinned:
   parse-of-whole == concat of committed parses at the same width for a corpus
   of fixtures — or simpler, the final rebuild discards incremental caches and
   full-parses, asserted equal).
3. A streamed table shows no half-rendered pipe rows: rows stay in the plain
   tail until the entry finalizes (or the table's trailing blank line arrives).
4. Reducer tests drive gears and holdback purely with synthetic
   `ModelStreamDelta` + `Tick` actions carrying fabricated timestamps.

### Gotchas

- `markdown::parse` is whole-source; committing per line re-parses the
  committed prefix each commit. That is codex's model too, but bound it: cache
  keyed on `(committed_len, width)` and skip when unchanged; a pathological
  10 000-line answer re-parses O(lines) times — acceptable at ≤ 80 ms cadence,
  but keep `MAX_MODEL_ENTRY_BYTES` (existing) as the backstop.
- Markdown constructs that span lines (setext headings, lazy blockquote
  continuations) can render differently when the source is cut at a line
  boundary vs finalized. Codex accepts brief mid-stream inaccuracy in the
  committed region *except* tables (the holdback). Mirror that: only tables
  get holdback; do not chase perfect incremental markdown.
- The reducer must never read `Instant::now()` — all ages come from event/tick
  timestamps (`at`), preserving replay determinism (`state.rs` module doc:
  same events ⇒ same state).
- Width changes (resize) invalidate `rendered`; recompute from
  `committed_len` at the new width (the existing width-keyed cache rule).

---

## M4 — System theme synthesis from the terminal palette

### Reference

opencode `packages/tui/src/theme/index.ts::generateSystem(colors, mode)`
(line 360): read the terminal's **actual** default bg/fg + 16-color palette;
compute dark/light by luminance; synthesize a full theme: primary/accent from
ANSI cyan, status colors from ANSI red/yellow/green/cyan, gray ramp generated
*from the real background*, diff backgrounds as alpha tints of bg toward
green/red, and — crucially — `background: transparent` so terminal
transparency survives.

### Current state (verified)

`crates/tui/src/theme.rs`: semantic token groups
(`surface/text/status/syntax/diff/agent/focus/selection`), seven const
variants, `Theme::select(depth, prefs)` where a manual
`prefs.override_variant` **always wins** (line 565), `ColorDepth::detect()`
from `COLORTERM`/`TERM`/`NO_COLOR` env (line 666). **Nothing queries the
terminal's real colors** — no OSC 10/11/4 anywhere in the workspace (verified
by grep); dark-vs-light is a variant choice, not detected.

### Changes

- **`crates/tui/src/theme.rs`** — pure synthesis (no I/O):

  ```rust
  /// The terminal's own colors, queried by the harness before the event loop.
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct TerminalPalette {
      pub background: (u8, u8, u8),
      pub foreground: (u8, u8, u8),
      pub ansi: [(u8, u8, u8); 16],
  }
  impl Theme {
      /// opencode-style synthesized theme. `surface.background` is
      /// `Color::Reset` (terminal transparency preserved); panel/overlay/user
      /// are gray-ramp blends off the REAL background; status/diff/syntax
      /// draw from the user's own ANSI palette; text from real fg.
      pub fn system(palette: &TerminalPalette) -> Self;
      pub fn is_light(bg: (u8, u8, u8)) -> bool;   // relative luminance > 0.5
  }
  // ThemeVariant gains no const variant; instead:
  pub enum ThemeChoice { Variant(ThemeVariant), System(TerminalPalette) }
  ```

  Synthesis rules (mirror generateSystem): gray ramp = 10 steps blending bg
  toward fg (dark: lighten; light: darken); `status.{error,warning,success,info}`
  = ansi 1/3/2/6; `focus.active` = ansi 6 (cyan); `diff.added/removed` =
  ansi 2/1; selection bg = ramp step 3, fg = real fg; `text.muted` = ramp ~60 %
  toward bg. Every computed color is `Color::Rgb`, so **System is offered only
  at `ColorDepth::TrueColor`** — on lower depths fall back to the existing
  ansi256/ansi16 selection (they already use terminal-relative ANSI colors).
- **`Theme::select`** — precedence unchanged: manual override →
  accessibility prefs → **System when a palette was captured and depth is
  TrueColor** → the existing dark/light/depth ladder.
- **`crates/cli/src/tui.rs`** — one-shot startup probe (before the splash, raw
  mode already active): write OSC 10/11 (`\x1b]10;?\x07`, `\x1b]11;?\x07`) and
  OSC 4 for indices 0–15, read responses with a **100 ms overall budget**;
  parse `rgb:RRRR/GGGG/BBBB`. Any timeout/garbage ⇒ `None` (fall back, never
  block). Bytes consumed that are *not* responses (a fast typist) are pushed
  back into the input bridge's queue verbatim — the codex `terminal_probe`
  replay lesson, scoped down: probe *before* spawning the input thread so
  there is nothing to race.
- **`crates/tui/src/theme_pack.rs` / theme picker** — surface "System" as a
  selectable choice when available; persisting the choice stores the *name*,
  and the palette is re-queried each launch (the terminal may have changed).

### Acceptance criteria

1. On a truecolor terminal with a custom scheme, the default theme uses the
   terminal's own bg (transparent), fg, and ANSI accents; on `TERM=dumb` or
   probe timeout, behavior is byte-identical to today.
2. Manual `/theme` override still wins over System (existing test extended).
3. `Theme::system` is fully unit-tested from fabricated palettes: dark and
   light backgrounds produce ramps in the correct direction; every token
   differs from `surface.background` (no invisible text).
4. Probe parsing handles `rgb:` 8/12/16-bit forms and BEL/ST terminators
   (pure parser unit tests on byte fixtures).

### Gotchas

- `Color::Reset` as background: several render sites fill with
  `theme.surface.background`; `Reset` in a `bg()` style is valid ratatui and
  yields the terminal default — but any code that *blends* against
  `surface.background` must use the captured real bg instead (pass it inside
  the synthesized theme's panel/overlay tokens; never blend against `Reset`).
- tmux/screen swallow OSC 4/10/11 queries by default — the probe must tolerate
  zero bytes back (it already must, for the timeout path). Don't attempt DCS
  passthrough for queries in v1.
- A6/A4 interaction: if clippy `disallowed-methods` bans `Color::Rgb`
  (Adoption 12/A4), `theme.rs` is the negotiated `#[allow]` site — synthesis
  lives entirely inside it, which is exactly the architecture the lint
  enforces.

---

## M5 — OSC-8 hyperlink sidecar

### Reference

`codex-rs/tui/src/terminal_hyperlinks.rs`: `HyperlinkLine` carries
`(column-range → destination)` annotations **beside** the visible line;
wrapping remaps annotations; OSC 8 bytes are injected only at write time so
escape sequences never affect layout math; destinations are typed `Web` vs
`TrustedFile`, and only generated visualization files may become `file://`
links (a security boundary).

### Current state (verified)

- `crates/tui/src/markdown.rs`: `RichSpan { text, role }` — `SpanRole::Link`
  exists but **the destination URL is discarded at parse time**; links render
  styled-but-dead.
- Rendering goes through stock ratatui 0.29 `CrosstermBackend<Stdout>`
  (`terminal.rs`); ratatui cells carry no hyperlink attribute, and codex solved
  this with a forked terminal — not available here.
- Precedent for frame-scoped geometry registries in `AppState` (interior
  mutability, `Cell`/`RefCell` caches: click targets, scroll geometry —
  `state.rs:2335, 2468–2484`).

### Changes

- **`crates/tui/src/markdown.rs`** — keep destinations:

  ```rust
  pub struct RichLine { pub spans: Vec<RichSpan>, pub links: Vec<LinkAnnotation> }
  /// Byte-offsets into the CONCATENATED span text of this line.
  pub struct LinkAnnotation { pub range: std::ops::Range<usize>, pub destination: String }
  ```

  `parse` records the URL from `Tag::Link`; only `http://` and `https://`
  destinations are kept (**Web only in v1** — codypendent has no trusted-file
  generator, so the `TrustedFile` arm is deliberately absent; `file:` /
  anything else is dropped, matching codex's default-deny posture).
- **`crates/tui/src/render.rs`** — while emitting a `RichLine`, register each
  visible link region into a frame-scoped registry on `AppState`
  (`RefCell<Vec<(Rect, String)>>`, cleared at frame start — the exact pattern
  of the existing click-target caches). Regions are in screen coordinates and
  clipped to the drawn area, so wrapping/scrolling is handled by construction
  (a link split across wrapped rows registers one region per row).
- **`crates/cli/src/tui.rs` + `crates/tui/src/terminal.rs`** — inject at write
  time: after `terminal.draw(..)` returns, the harness reads the registry and,
  for each region, re-addresses the cursor and re-emits **nothing** — instead,
  injection happens *during* draw via a backend wrapper:

  ```rust
  /// Wraps CrosstermBackend. `draw` splits the incoming cell run at hyperlink
  /// region boundaries and brackets each linked group with raw OSC 8 writes:
  ///   \x1b]8;;URL\x1b\\  …cells…  \x1b]8;;\x1b\\
  /// OSC 8 is a state toggle applying to subsequently written cells, so
  /// interleaving raw writes between inner.draw calls is safe; the bytes never
  /// enter layout math (they are written, not measured).
  pub struct HyperlinkBackend<W: Write> { inner: CrosstermBackend<W>, regions: Vec<(Rect, String)> }
  impl<W: Write> HyperlinkBackend<W> { pub fn set_regions(&mut self, regions: Vec<(Rect, String)>); }
  impl<W: Write> Backend for HyperlinkBackend<W> { /* forward everything; draw() splits */ }
  ```

  `TerminalGuard` generalizes to `Terminal<HyperlinkBackend<Stdout>>`; the
  harness copies the frame's registry into the backend before each `draw`.
  Destinations are sanitized before emission (strip control bytes, cap 2 KiB —
  the OSC payload must not be able to break the sequence).

### Acceptance criteria

1. A finalized transcript with `[docs](https://example.com)` emits an OSC 8
   open/close pair around exactly the link's cells (assert on captured backend
   output — this is a natural first customer for Adoption 12/A1's
   vt100 backend, whose parser tolerates unknown OSC).
2. `file:///etc/passwd` and `javascript:` markdown links render as styled text
   with **no** OSC 8 emitted.
3. Layout is byte-identical with links present vs stripped (escape bytes never
   affect geometry): render to a plain `TestBackend`, compare buffers.
4. Terminals without OSC 8 support: sequences are ignored by definition; no
   allow-list needed (unlike OSC 9 — note in the module doc why the two differ).

### Gotchas

- The diff-based `draw` only receives *changed* cells; a link whose text didn't
  change re-renders without its cells, leaving the old (linked) cells intact —
  correct, since the link state was written with them. But a *partial* overlap
  (cursor moving through a linked region writing one changed cell) must
  re-open/close the OSC state for that cell alone; the region split in
  `HyperlinkBackend::draw` handles any subset of cells, so implement the split
  per-cell-run, not per-line.
- `Rect` regions must be recomputed every frame (scrolling moves them); a stale
  registry links the wrong rows — hence frame-scoped clearing, mirrored on the
  existing click caches.
- Multi-width graphemes: regions are computed from rendered columns
  (`unicode_width`), not byte offsets — reuse the width logic markdown already
  applies when padding table cells.
