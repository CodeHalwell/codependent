# Adoption 20 — Composer Parity

**Effort:** M · **Depends on:** nothing (18 handles the empty-`—` render; this provides /context data) · **Reference:** reference-repos/codex/codex-rs/core/src/agents_md.rs, reference-repos/codex/codex-rs/core/src/agents_md_manager.rs, reference-repos/opencode/packages/opencode/src/session/prompt.ts, reference-repos/cline/apps/cli/src/tui/components/status-bar.tsx
**Ported from:** codex+opencode+claude-code · **Status:** ⬜ not started

## 1. Summary

Four independent composer/session parity features from spec 15 §9 (master plan Track B: B2, B3, B5, B6). Each is a small composition over machinery that already ships; none needs a new subsystem.

- **Action 17 — Hierarchical instruction files (B2).** Walk `cwd → project root`, concatenating `AGENTS.md` / `CLAUDE.md` (ecosystem interop: we read the competitors' files) plus `.codypendent/instructions.md`, root-first so the most specific file wins by appearing last. The concatenation feeds the system prompt. **Net-new:** verified below, the runtime reads **no** instruction files today — the system prompt is a hardcoded constant plus static mode overlays plus the run objective.
- **Action 18 — `!` shell prefix (B3).** Typing `!<cmd>` in the composer and pressing Enter runs a shell command as a **user-initiated, transcript-recorded turn** — not an agent tool call. The command and its bounded output enter the ledger (so the model sees what the user ran on its next turn) via the already-shipped `Shell::execute` seam. **Net-new:** no bang-command path exists today.
- **Action 19 — `/context` breakdown card (B5).** The runtime already estimates context-window usage every turn (`estimate_request_tokens`) but only emits it as a `BudgetWarning` at the 80 % compaction threshold, and only when the model's window is known. This action emits a **live per-turn context-usage event**, projects it into the existing `RunView::context_percent` (which already drives the status-bar `ctx` meter), adds a component breakdown, and adds a `/context` overlay card. Adoption 20/A4 (spec 18) hides the empty `—`; **this action supplies the data that makes the meter fill.**
- **Action 20 — `#` memory quick-add (B6).** Typing `#<text>` and pressing Enter files a curated memory without leaving the composer, routing through the shipped memory curator (`MemoryStore::curate`). **Net-new:** no `#` path and no quick-add command exist today.

Actions 18 and 20 share the exact same composer-submit interception seam (`crates/tui/src/reduce.rs`, the `Overlay::None` arm of `submit_prompt`). Action 19 shares no code with the others.

## 2. Reference implementations

**Instruction discovery — `reference-repos/codex/codex-rs/core/src/agents_md.rs`:**
- Module doc (lines 1–16): determine the project root by walking up from cwd until a `project_root_markers` entry is found (default marker `.git`; an empty marker list disables parent traversal); collect every `AGENTS.md` from the project root **down to** cwd inclusive and concatenate in that order; **do not walk past the project root.**
- `DEFAULT_AGENTS_MD_FILENAME = "AGENTS.md"` (line 37), `LOCAL_AGENTS_MD_FILENAME = "AGENTS.override.md"` (line 39), `AGENTS_MD_SEPARATOR = "\n\n--- project-doc ---\n\n"` (line 43), `MAX_CONCURRENT_ANCESTOR_PROBES = 256` (line 48).
- `load_project_instructions(...)` (line 52) combines discovered project docs with host-provided user instructions; `read_agents_md(...)` (line 99) is the per-directory read. `find_nearest_ancestor_with_markers` (from `codex_file_system`) is the root finder. `agents_md_manager.rs` caches and watches for changes (we do **not** port the watcher — we re-read at run launch).
- `reference-repos/opencode` instruction resolution: a global `~/.claude/CLAUDE.md` plus project files, first-match precedence — we port the "global then project" layering (global = lowest precedence, appended first).

**`!` shell as a recorded turn — `reference-repos/opencode/packages/opencode/src/session/prompt.ts` `shell()` / `shellImpl` (line 451+):**
- A user-run command is recorded as a **session turn**: a synthetic user text part (`synthetic: true`, lines 447/485) plus an assistant message carrying a shell `ToolPart` in `status: "running"` (lines 505–523) that **streams output** into the part (lines 530–562, `TERM: "dumb"`).
- On abort, the output gets a bounded `<metadata>User aborted the command</metadata>` trailer (line 531). The part transitions `running → completed` with the captured output (lines 399–427). The takeaway we port: the command **and** its output are recorded **in-band** so the model sees them; output is bounded.
- pi `user_bash`: `!` runs in-context (command + output enter the conversation); `!!` runs excluded-from-context. We port only the in-context `!` for v0.8.

**`/context` meter — `reference-repos/cline/apps/cli/src/tui/components/status-bar.tsx`:** a block context meter of used vs. max input tokens (the same six-cell block meter codypendent already renders in `crates/tui/src/render.rs::context_meter`). Claude Code `/context` is the breakdown-card model: system prompt, tools, messages, free space.

**`#` memory quick-add — Claude Code `#`:** typing `#` then text files the text as a memory inline, no mode change beyond the prefix.

## 3. Current state in codypendent (verified)

### Instructions reach a run (Action 17) — files are NOT read today
- The system prompt is a hardcoded constant: `const SYSTEM_PROMPT: &str = "You are a coding agent. …"` at **`crates/runtime/src/agent.rs:411`**.
- It is injected in `FrameworkModelDriver::to_messages(transcript: &[TurnItem]) -> Vec<Message>` at **`agent.rs:7994`**: `let mut messages = vec![Message::system(SYSTEM_PROMPT)];`, then each `TurnItem` maps to a role message (`Objective`→user, `Assistant`→assistant, `ToolCall`→assistant, `ToolResult`→user, `Steering`→user).
- The only other "instructions" are (a) the run **objective**, seeded as the first transcript item (`agent.rs:2601-2611`: `transcript = run.prior.clone(); transcript.push(TurnItem::Objective(objective))`), and (b) static **mode overlays** — `mode_seed_instruction(mode)` (`agent.rs:218-225`) prepends `PLAN_MODE_INSTRUCTION` / `REVIEW_MODE_INSTRUCTION` / `ASK_MODE_INSTRUCTION` to the objective (`agent.rs:2607-2611`).
- A repo-wide grep for `AGENTS.md` / `CLAUDE.md` finds **zero** runtime reads (only the doc file `crates/runtime/src/AGENTS.md`, not loaded at runtime). `.codypendent/` is used only for workflow manifests (`.codypendent/workflows/*.yaml`). **Contradicts the plan's "instructions already assembled somewhere" assumption — there is no discovery/concat path to extend; we add one.**

### Composer prefix parsing (Actions 18, 20)
- `crates/tui/src/input.rs` is a pure event→`Action` mapper. In `map_composer_key` (`input.rs:449`), printable keys — including `/`, `!`, `#` — map to `Action::InputChar(c)` (`input.rs:498`). The mapper does **not** special-case any prefix; `/` is treated as a literal character (the tests at `input.rs:953-960` assert `/` → `InputChar('/')`).
- The `/` command palette is opened by the **reducer**, not the mapper: `input_char` (`reduce.rs:4727`) opens `Overlay::Palette` when `c == '/' && overlay == None && composer.is_empty() && queue_editing.is_none()` (`reduce.rs:4728-4738`). This is the model to follow — but note `/` fires on the *keystroke* (empty composer), whereas `!`/`#` per this spec fire on **submit** (they carry a payload).
- The composer-submit seam is `submit_prompt` (`reduce.rs:5380`), specifically the `Overlay::None` arm (`reduce.rs:6200`). After queue-edit handling, it computes `let mut text = state.composer.trim().to_owned();` (`reduce.rs:6223`), expands pasted-block markers (`6224-6227`), then routes to `Intent::QueuePrompt` (active run), `Intent::SubmitUserInput` (terminal run), or `Intent::StartRun` (no run) and clears the composer (`6293-6294`). **This trimmed-`text` point is where `!`/`#` are intercepted.**
- `InputMode` (`state.rs:79`) and `Overlay` (`state.rs:216`) are the mode/overlay enums; the palette command table is `PaletteCommand` (`palette.rs:15`) with `PaletteEntry` rows (`palette.rs:80`) and reducer dispatch `run_palette_command` (`reduce.rs:7063-7186`).

### Context / token accounting (Action 19)
- **Estimator (already exists):** `estimate_context_tokens(&[TurnItem]) -> usize` (`agent.rs:399`) and the authoritative `estimate_request_tokens(&[TurnItem], &[ToolDefinition]) -> usize` (`agent.rs:432`, adds the system-prompt cost + each tool's name/description/schema). Constants: `CHARS_PER_TOKEN = 4` (`agent.rs:344`), `PER_ITEM_TOKEN_OVERHEAD = 4` (`agent.rs:354`), `COMPACTION_THRESHOLD_PCT = 80` (`agent.rs:246`).
- **Emit today:** `token_budget_event(run_id, used, limit, last_emitted_pct) -> Option<(EventBody, u16)>` (`agent.rs:460`) returns `EventBody::BudgetWarning { run_id, dimension: BudgetDimension::Tokens, used, limit }`, deduped by pct. Call site `agent.rs:2700-2743` runs **only** when `driver.context_window()` is `Some(limit)` (`agent.rs:2632`) and only crosses the emit when `used > limit * 80 / 100`. When the window is unknown the block is skipped and no `Tokens` event ever fires (honesty rule; the footer stays `—`).
- **Protocol (verified `crates/protocol/src/events.rs`):** `EventBody` is `#[serde(tag = "type")]`, `#[non_exhaustive]`, with a `#[serde(other)] Unknown` fallback. `BudgetWarning { run_id, dimension: BudgetDimension, used: u64, limit: u64 }` (`events.rs:199`) is a threshold **warning** (absolute counts, not a percent, and most runs have no configured budget). `RunUsage { run_id, prompt_tokens?, completion_tokens?, cost_micros? }` (`events.rs:227`) is a **terminal, post-run** measured tally. **No event carries a live context-window percent or a running token estimate.** So `/context` needs a net-new event.
- **TUI projection (already wired):** `EventBody::BudgetWarning{ dimension: Tokens, used, limit }` is folded in `reduce.rs:2255-2280` into `run.context_percent = Some((used*100/limit).min(100) as u16)`. `RunView::context_percent: Option<u16>` (`state.rs:1058`). The status bar reads `status.context_percent` and draws `context_meter` (`render.rs:308`) / `context_severity_color` (`render.rs:327`); the empty case renders `ctx {meter} —` at `render.rs:446` (six-cell `──────`). Tests `budget_warning_projects_context_and_cost` (`reduce.rs:8817`) and `budget_warning_tokens_brings_the_dead_context_footer_alive` (`reduce.rs:8855`) already assert the projection. **The data pipe exists end-to-end; it just fires rarely.**

### Memory write seam (Action 20)
- The curator is the governed write path: `MemoryStore::curate(&self, pool: &SqlitePool, candidate: CandidateMemory) -> Result<Curation, MemoryError>` (`crates/knowledge/src/memory.rs:478`): secret-filter → scope → contradiction/supersession → dedup → provenance (**evidence-free candidates are rejected**) → retention → `insert`. `Curation` (`memory.rs:672`): `Accepted(MemoryRecord)` / `Redacted{reason}` / `Duplicate{existing_id}` / `Superseded{old_id, record}` / `Rejected{reason}`.
- `CandidateMemory` (`memory.rs:638`) fields: `class: MemoryClass`, `scope: Option<Scope>`, `statement: String`, `structured_value: Option<Value>`, `provenance: Vec<EvidenceRef>` (**≥1 required**), `confidence: f32`, `observed_at`, `valid_from: Revision`, `sensitivity: DataClassification`, `retention: Option<RetentionPolicy>`. Plain struct, built literally by callers.
- The agent-facing tool `memory.remember` (`crates/runtime/src/tools/memory.rs`, `NAME = "memory.remember"`) does **not** write directly — it emits a `NoteAppended` text `"memory.propose: <statement>"`. The observer `extract_candidates(&[SessionEvent], scope)` (`crates/knowledge/src/observer.rs:99`) scans notes for `PROPOSE_MARKERS = ["memory.propose:", "memory:"]` (`observer.rs:55`) and builds `CandidateMemory`s; production wiring runs this at run-completion harvest in `crates/codypendentd/src/executor.rs:2020`, then `store.curate(...)` per candidate (`executor.rs:2074`), emitting a `remembered: {statement}` note on `Accepted` (`executor.rs:2080`).
- Existing memory **commands** are read/edit/delete only: `InspectMemory` (`command.rs:624`), `CorrectMemory` (`command.rs:636`), `ForgetMemory` (`command.rs:650`), `ForgetMemoryScope` (`command.rs:659`). **No quick-add command exists.**

### Shared runtime seams (Actions 17, 18)
- **Shell:** `Shell::execute(request: &CommandRequest, path_scope: &PathScope, command_scope: &CommandScope, sink: &dyn ArtifactSink, run_id: RunId) -> Result<ShellOutcome, ToolError>` (`crates/runtime/src/tools/shell.rs:120`). Output is capped at `MAX_CAPTURE_BYTES = 16 MiB` (`tools/mod.rs:411`), full output spilled to an `ArtifactRef` via `spill(...)` (`shell.rs:220`), and a compacted model-facing view built by `salient::compute_stream` into `ShellOutcome.salient` (`shell.rs:223`). Program allow-list, cwd-inside-`path_scope`, denied env names, `env_clear()`, own process group + timeout kill are enforced pre-spawn. Already called outside the tool loop (`RepositoryTest::execute`, `agent.rs:4780`). **The seam exists; a user-initiated caller does not.**
- **Injection pattern (adoption 06):** the steering seam is the template for "attach client-supplied data to a live/launching run." `RunContext.steering: Option<mpsc::UnboundedReceiver<String>>` + `with_steering(...)` builder (`agent.rs:1170/1254`); the executor inserts a sender at launch (`codypendentd/src/executor.rs:1041-1046`) and injects via `steer_run(run_id, text)` (`executor.rs:2649`); `drain_steering` (`agent.rs:3301`) pushes `TurnItem::Steering` and emits `SteeringApplied`. Instruction assembly (Action 17) reuses the *builder-at-launch* half of this pattern.
- **Command/Intent plumbing:** `Intent` (`crates/tui/src/action.rs:721`) is the TUI→harness effect enum (e.g. `StartRun`, `QueueSteering`, `SubmitUserInput`); the harness maps each to a `CommandBody` (`crates/protocol/src/command.rs:60`, same `#[serde(tag="type")]` / `#[non_exhaustive]` / `#[serde(other)] Unknown` convention). New composer effects add one `Intent` + one `CommandBody` each.

### Docs & migrations
- The doc-count/manifest gate (`.github/scripts/check_docs_manifest.py`) requires every file under `docs/` to appear in `docs/MANIFEST.json`'s `files` array. This new file (`docs/adoptions/20-composer-parity.md` relative to `docs/`) must be added there.
- Highest migration is `migrations/0038_approval_patterns.sql`; next free is **0039**. **None of the four actions needs a migration** (events/commands are serialized JSON bodies; the memory write reuses existing tables).

## 4. Design (per-Action)

### Action 17 — Hierarchical instruction files
Add a pure `crates/runtime/src/instructions.rs` module that discovers and concatenates instruction files, mirroring codex's walk:
1. From the run's worktree `cwd`, walk parents until a project-root marker (`.git` or `.codypendent`) is found; that ancestor is the project root. If no marker is found, only `cwd` is considered. **Never walk past the project root.**
2. Global layer (opencode): if `~/.claude/CLAUDE.md` exists, it is the **first** (lowest-precedence) block.
3. Project layer: for each directory from **project root → cwd** (inclusive, in that order), read each of `AGENTS.md`, `CLAUDE.md`, then `.codypendent/instructions.md` if present, appending in that fixed filename order. Root-first ordering means the deepest (most specific, cwd) files appear **last**, so they win by recency in the concatenation — matching codex.
4. Concatenate with a labeled separator; cap total at `MAX_INSTRUCTION_BYTES` (64 KiB) to bound the prompt.

The assembled string is attached to the launching run exactly like steering: a new `RunContext.instructions: Option<String>` field + `with_instructions(...)` builder, set by the codypendentd executor at run launch (it knows the worktree path). It reaches the model by prepending to the system message in `to_messages` (see §5). Re-read at every run launch (no file watcher — the codex `agents_md_manager` cache is deliberately not ported).

### Action 18 — `!` shell prefix
In the `Overlay::None` submit arm, after computing the trimmed `text` and expanding pasted blocks, detect a leading `!`. If `text` starts with `!` and has a non-empty remainder, strip the `!`, push `Intent::RunUserShell { command }`, clear the composer, and return **before** any run-routing branch (a `!cmd` is never a prompt/steer). Record it in composer history like any submission.

The harness maps `Intent::RunUserShell` → `CommandBody::RunUserShell { session_id, command }`. The daemon executes it via `Shell::execute` under the **session's** existing `PathScope` / `CommandScope` / `ArtifactSink` (the same scopes a run's `shell.run` uses — no widening) and records it as a **user-authored turn**: a `NoteAppended` (actor `Human`) echoing `$ <command>`, then a `NoteAppended` carrying the bounded `salient` output (full output spilled to an `ArtifactRef`, exactly like a tool). Because these events are in the ledger, the next `SubmitUserInput` continuation reconstructs them into the run's `prior` context, so the model sees what the user ran (opencode's in-context `!`). The TUI folds these notes into a `TranscriptEntry::User` echo + a foldable `TranscriptEntry::Note` output card (existing rendering; long output already folds).

### Action 19 — `/context` breakdown card
Two halves — make the data flow continuously, then show it.

**Runtime (emit every turn):** at the existing per-turn estimate site (`agent.rs:2700-2743`), when `driver.context_window()` is `Some(window)`, emit a new `EventBody::ContextUsage { run_id, used_tokens, window_tokens, system_tokens, tool_tokens, transcript_tokens }` every turn (deduped by integer percent to avoid ledger spam, reusing the `last_emitted_pct` dedup already there). The three component fields are the existing sub-estimates (`system_tokens` = system-prompt cost incl. Action-17 instructions; `tool_tokens` = sum over tool definitions; `transcript_tokens` = `estimate_context_tokens`). When the window is unknown, emit nothing (unchanged honesty rule). This does **not** replace `BudgetWarning{Tokens}` — that stays as the 80 % compaction warning.

**TUI (project + card):** fold `ContextUsage` into `RunView`: set `context_percent = (used*100/window).min(100)` (same projection as `BudgetWarning{Tokens}`, so the meter comes alive proactively) plus new `RunView::context_breakdown: Option<ContextBreakdown>`. Add `PaletteCommand::Context` + `Action::OpenContext` + `Overlay::Context`, and a `render_context_card` that draws the breakdown (system / tools / transcript / used / free, with the same six-cell meter and severity colors). Spec 18/A4 hides the empty `—`; this action is what fills the meter.

### Action 20 — `#` memory quick-add
Same submit seam as Action 18. If the trimmed `text` starts with `#` **and** the remainder (after stripping `#` and whitespace) is non-empty **and** the composer is a single line (guard against a multi-line markdown message whose first line is a heading — see gotchas), strip the `#`, push `Intent::RememberMemory { text: remainder }`, clear the composer, and return before run-routing.

The harness maps `Intent::RememberMemory` → `CommandBody::RememberMemory { session_id, text }`. The daemon builds a `CandidateMemory` (`class: MemoryClass::Semantic`, `statement: text`, `provenance: vec![user-authored EvidenceRef]` — required or `curate` rejects it, `confidence` high, `scope` = the session's repository scope, `observed_at` = now) and calls `MemoryStore::curate`. On `Accepted`/`Superseded` it emits a `remembered: {statement}` note (identical to the harvest path, `executor.rs:2080/2099`); on `Rejected`/`Redacted` it emits a note explaining why. The TUI shows the resulting note inline — the user never leaves the composer.

## 5. Changes file-by-file (literal skeletons)

### `crates/runtime/src/instructions.rs` (NEW — Action 17)
```rust
//! Hierarchical instruction-file discovery (AGENTS.md / CLAUDE.md / .codypendent),
//! ported from codex `agents_md.rs`: walk cwd → project root, concatenate root
//! first so the most specific (cwd) file wins. Never walk past the project root.
use std::path::{Path, PathBuf};

/// Files read at each directory, in fixed precedence order (later = more specific).
pub const INSTRUCTION_FILENAMES: &[&str] = &["AGENTS.md", "CLAUDE.md"];
/// Markers that identify the project root (traversal stops at the first match).
pub const PROJECT_ROOT_MARKERS: &[&str] = &[".git", ".codypendent"];
const SEPARATOR: &str = "\n\n--- instructions ---\n\n";
/// Cap the concatenation so a stray large file cannot bloat every prompt.
pub const MAX_INSTRUCTION_BYTES: usize = 64 * 1024;

/// Discover and concatenate instructions for a run rooted at `cwd`. Returns
/// `None` when nothing is found (so the caller leaves the system prompt as-is).
#[must_use]
pub fn discover_instructions(cwd: &Path, home: Option<&Path>) -> Option<String> {
    let root = project_root(cwd);
    let mut out = String::new();
    // Global layer first (lowest precedence), opencode-style.
    if let Some(home) = home {
        push_file(&mut out, &home.join(".claude/CLAUDE.md"));
    }
    // Project layer: root → cwd inclusive, so cwd files land last.
    for dir in chain_root_to_cwd(&root, cwd) {
        for name in INSTRUCTION_FILENAMES {
            push_file(&mut out, &dir.join(name));
        }
        push_file(&mut out, &dir.join(".codypendent/instructions.md"));
    }
    if out.is_empty() { None } else { Some(out) }
}

fn project_root(cwd: &Path) -> PathBuf {
    for dir in cwd.ancestors() {
        if PROJECT_ROOT_MARKERS.iter().any(|m| dir.join(m).exists()) {
            return dir.to_path_buf();
        }
    }
    cwd.to_path_buf() // no marker: only cwd is considered
}

/// Directories from project root down to cwd, inclusive, root first.
fn chain_root_to_cwd(root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut chain: Vec<PathBuf> =
        cwd.ancestors().take_while(|d| d.starts_with(root)).map(Path::to_path_buf).collect();
    chain.reverse(); // ancestors() is cwd→root; we want root→cwd
    chain
}

fn push_file(out: &mut String, path: &Path) {
    let Ok(body) = std::fs::read_to_string(path) else { return };
    let body = body.trim();
    if body.is_empty() || out.len().saturating_add(body.len()) > MAX_INSTRUCTION_BYTES { return; }
    if !out.is_empty() { out.push_str(SEPARATOR); }
    out.push_str(body);
}
```

### `crates/runtime/src/agent.rs` (Action 17 + Action 19)
```rust
// Action 17: attach discovered instructions to a launching run (steering pattern).
pub struct RunContext {
    // …existing fields…
    /// Concatenated AGENTS.md/CLAUDE.md/.codypendent instructions, prepended to
    /// the system prompt. `None` = no instruction files found (unchanged prompt).
    pub instructions: Option<String>,
}
impl RunContext {
    #[must_use]
    pub fn with_instructions(mut self, instructions: Option<String>) -> Self {
        self.instructions = instructions;
        self
    }
}

// to_messages gains the assembled instructions (threaded from RunContext via the
// driver). SYSTEM_PROMPT is unchanged; instructions are appended after it.
fn to_messages(transcript: &[TurnItem], instructions: Option<&str>) -> Vec<Message> {
    let system = match instructions {
        Some(extra) if !extra.is_empty() => format!("{SYSTEM_PROMPT}\n\n{extra}"),
        _ => SYSTEM_PROMPT.to_owned(),
    };
    let mut messages = vec![Message::system(system)];
    // …existing per-TurnItem mapping unchanged…
    messages
}

// Action 19: emit live context usage every turn (near agent.rs:2700-2743).
// Only when the window is known (unchanged honesty rule).
fn context_usage_event(
    run_id: RunId,
    used: u64,
    window: u64,
    system_tokens: u64,
    tool_tokens: u64,
    transcript_tokens: u64,
    last_emitted_pct: Option<u16>,
) -> Option<(EventBody, u16)> {
    let pct = (used.saturating_mul(100) / window.max(1)).min(100) as u16;
    if last_emitted_pct == Some(pct) { return None; } // dedup by percent
    Some((
        EventBody::ContextUsage {
            run_id,
            used_tokens: used,
            window_tokens: window,
            system_tokens,
            tool_tokens,
            transcript_tokens,
        },
        pct,
    ))
}
```

### `crates/codypendentd/src/executor.rs` (Actions 17, 18, 20)
```rust
// Action 17: assemble instructions at run launch and attach (mirrors with_steering).
let instructions = codypendent_runtime::instructions::discover_instructions(
    &worktree_cwd,
    dirs::home_dir().as_deref(),
);
ctx = ctx.with_instructions(instructions);

// Action 18: run a user `!cmd` CONFINED in the session worktree, recorded in-band.
async fn run_user_shell(&self, session_id: SessionId, command: String) -> anyhow::Result<()> {
    self.append_note(session_id, Actor::human(), format!("$ {command}")).await?;
    // `/bin/sh -c <cmd>` still supports pipes/redirects, but runs under the
    // platform sandbox (Seatbelt/bwrap) scoped to the session worktree — bounded,
    // time-limited, no network. Fails CLOSED: refused (never run unconfined) when
    // the sandbox is unavailable. See `commands::apply_run_user_shell`.
    let executor = enforcing_executor()?;                       // fail-closed: refuse if unavailable
    let profile  = user_shell_profile(&origin, &self.worktree(session_id)); // rw worktree, no network
    let command  = SandboxCommand::new("/bin/sh", vec!["-c".into(), command], worktree, origin);
    let body = match executor.run(&profile, &command) {
        Ok(outcome) => outcome.stdout.text,                     // sanitized + output-capped by the sandbox
        Err(err)    => format!("shell escape refused: {err}"),  // never run bare
    };
    self.append_note(session_id, Actor::human(), body).await?; // user-authored → seeds next turn's context
    Ok(())
}

// Action 20: file a memory through the shipped curator.
async fn remember_memory(&self, session_id: SessionId, text: String) -> anyhow::Result<()> {
    let candidate = CandidateMemory {
        class: MemoryClass::Semantic,
        scope: Some(self.repository_scope(session_id)),
        statement: text,
        structured_value: None,
        provenance: vec![EvidenceRef::user_authored(session_id)], // ≥1 required by curate
        confidence: 0.9,
        observed_at: Utc::now(),
        valid_from: Revision::now(),
        sensitivity: DataClassification::default(),
        retention: None,
    };
    let note = match self.memory.curate(&self.pool, candidate).await? {
        Curation::Accepted(r) | Curation::Superseded { record: r, .. } =>
            format!("remembered: {}", r.statement),
        Curation::Duplicate { .. } => "already remembered".to_owned(),
        Curation::Redacted { reason } | Curation::Rejected { reason } =>
            format!("not remembered: {reason}"),
    };
    self.append_note(session_id, Actor::system(), note).await
}
```

### `crates/tui/src/reduce.rs` (Actions 18, 20 — the shared submit seam)
```rust
// Inside submit_prompt's `Overlay::None` arm, immediately after:
//     let mut text = state.composer.trim().to_owned();
//     for block in &state.pasted_blocks { text = text.replace(&block.marker, &block.text); }
//     state.pasted_blocks.clear();

// Action 18 — `!cmd`: a user-initiated shell turn, never a prompt.
if let Some(command) = text.strip_prefix('!') {
    let command = command.trim();
    if !command.is_empty() {
        record_submission_history(state, &text); // same dedup as a normal submit
        state.outbox.push(Intent::RunUserShell { command: command.to_owned() });
    }
    state.composer.clear();
    state.composer_cursor = 0;
    return;
}
// Action 20 — `#text`: quick-add a memory. Single-line only (see gotchas).
if let Some(rest) = text.strip_prefix('#') {
    let note = rest.trim();
    if !note.is_empty() && !text.contains('\n') {
        record_submission_history(state, &text);
        state.outbox.push(Intent::RememberMemory { text: note.to_owned() });
        state.composer.clear();
        state.composer_cursor = 0;
        return;
    }
    // else fall through: a multi-line `#`-led message is an ordinary prompt.
}
// …existing run-routing (QueuePrompt / SubmitUserInput / StartRun) unchanged…
```
```rust
// Action 19 — project ContextUsage (add to the EventBody match in `reduce`, near
// the BudgetWarning arm at reduce.rs:2255).
EventBody::ContextUsage { run_id, used_tokens, window_tokens,
    system_tokens, tool_tokens, transcript_tokens } => {
    if let Some(run) = state.run_mut(run_id) {
        run.context_percent =
            Some((used_tokens.saturating_mul(100) / window_tokens.max(1)).min(100) as u16);
        run.context_breakdown = Some(ContextBreakdown {
            used_tokens, window_tokens, system_tokens, tool_tokens, transcript_tokens,
        });
    }
}

// Action 19 — palette dispatch (add to run_palette_command, reduce.rs:7063-7186).
PaletteCommand::Context => {
    if state.selected_run().is_some() { state.overlay = Overlay::Context; }
    else { state.notice = Some(("no active run to inspect".to_owned(), state.tick + 25)); }
}
```

### `crates/tui/src/action.rs` (Actions 18, 19, 20)
```rust
pub enum Action { /* … */ OpenContext /* Action 19 */ }
pub enum Intent {
    // …
    RunUserShell { command: String },   // Action 18
    RememberMemory { text: String },    // Action 20
}
```

### `crates/tui/src/input.rs` (Action 19)
```rust
// In map_palette_key / normal command mapping, no `!`/`#` change is needed — they
// stay literal InputChar (intercepted at submit). `/context` is reached via the
// palette like every other command. (Optional single-key binding: none, matching
// the model picker's palette-only precedent.)
```

### `crates/tui/src/palette.rs` (Action 19)
```rust
pub enum PaletteCommand { /* … */ Context }
// New PaletteEntry row in the `Workspace` group:
PaletteEntry {
    command: PaletteCommand::Context,
    title: "Context breakdown",
    description: "what is occupying the model's context window",
    key: "—",              // palette-only, no single-key binding
    group: "Workspace",
}
```

### `crates/tui/src/state.rs` (Actions 18/20 render, 19 data)
```rust
pub enum Overlay { /* … */ Context /* Action 19 card */ }

/// Action 19: the token accounting behind the `/context` card and the `ctx` meter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBreakdown {
    pub used_tokens: u64,
    pub window_tokens: u64,
    pub system_tokens: u64,     // system prompt incl. Action-17 instructions
    pub tool_tokens: u64,       // tool definitions
    pub transcript_tokens: u64, // conversation so far
}

pub struct RunView { /* … */ pub context_breakdown: Option<ContextBreakdown> }
```

### `crates/tui/src/render.rs` (Action 19)
```rust
// New: render_context_card(frame, area, state, theme) drawn when Overlay::Context.
// Reuses context_meter (render.rs:308) + context_severity_color (render.rs:327);
// lists system / tools / transcript / used / free from RunView::context_breakdown.
// The status-bar `ctx` meter needs no change — it already reads context_percent,
// which now updates every turn.
```

### `crates/protocol/src/command.rs` (Actions 18, 20)
```rust
pub enum CommandBody {
    // …follow the existing #[serde(tag="type")] convention, session_id-keyed…
    RunUserShell { session_id: SessionId, command: String },          // Action 18
    RememberMemory { session_id: SessionId, text: String },           // Action 20
}
```

### `crates/protocol/src/events.rs` (Action 19)
```rust
pub enum EventBody {
    // …
    /// Live context-window usage, emitted per turn while the model's window is
    /// known (deduped by percent). Distinct from BudgetWarning (a threshold
    /// warning) and RunUsage (a terminal measured tally).
    ContextUsage {
        run_id: RunId,
        used_tokens: u64,
        window_tokens: u64,
        system_tokens: u64,
        tool_tokens: u64,
        transcript_tokens: u64,
    },
}
```

### `docs/MANIFEST.json`
Add `"docs/adoptions/20-composer-parity.md"` to the `files` array (run `.github/scripts/check_docs_manifest.py --fix` to regenerate/sort).

## 6. Protocol & persistence

**New commands** (`CommandBody`, `#[serde(tag="type")]`, PascalCase tag = variant name):
```json
{"type":"RunUserShell","session_id":"…","command":"cargo test -q"}
{"type":"RememberMemory","session_id":"…","text":"prefer ripgrep over grep in this repo"}
```

**New event** (`EventBody`, same convention):
```json
{"type":"ContextUsage","run_id":"…","used_tokens":42000,"window_tokens":200000,
 "system_tokens":900,"tool_tokens":3100,"transcript_tokens":38000}
```

Both enums are `#[non_exhaustive]` with a `#[serde(other)] Unknown` fallback, so an older client deserializes `ContextUsage` to `Unknown` and renders nothing (protocol RULE 1) rather than erroring.

**Migrations:** none. Events and commands persist as serialized JSON bodies in existing tables; the `#` quick-add reuses the existing memory tables through `MemoryStore::curate`/`insert`. Next free migration number is **0039** if a future change needs one — this batch does not.

**Secrets:** unchanged. `!cmd` runs under the session's existing `CommandScope`/`PathScope` with `env_clear()` and denied-env enforcement (`shell.rs`); no key is written to a file or log. The memory curator's secret filter (`curate` step 1) redacts credential-shaped statements before persistence.

## 7. Acceptance criteria

1. **Instruction discovery, root-first.** Unit test `discover_instructions`: create `root/.git`, `root/AGENTS.md` ("R"), `root/sub/CLAUDE.md` ("S"); RUN discovery from `root/sub`; EXPECT `Some` with `"R"` appearing before `"S"` (cwd wins by being last).
2. **Never crosses the project root.** With `AGENTS.md` in a directory ABOVE `root/.git`, EXPECT it is NOT included.
3. **Instructions feed the system prompt.** Runtime test: `to_messages(&[], Some("PROJECT RULES"))`'s first message is `Message::system` whose text contains both `SYSTEM_PROMPT` and `"PROJECT RULES"`; with `None`, it equals `SYSTEM_PROMPT` byte-for-byte.
4. **`!cmd` submits a shell intent, not a prompt.** Pure reducer: set `composer = "!cargo test"`, `Action::InputSubmit`; EXPECT `outbox` contains `Intent::RunUserShell { command: "cargo test" }` and NO `StartRun`/`QueuePrompt`/`SubmitUserInput`; composer cleared.
5. **Bare `!` is inert.** `composer = "!"` submit → composer cleared, no `RunUserShell`.
6. **`!cmd` output is recorded in-band and bounded.** Daemon/integration: after `RunUserShell`, the ledger has a `Human` note `"$ …"` and a bounded output note; full output has an `ArtifactRef`; a following `SubmitUserInput` reconstructs the notes into the model's context.
7. **`#text` files a memory.** Pure reducer: `composer = "#use ripgrep"` submit → `outbox` contains `Intent::RememberMemory { text: "use ripgrep" }`, composer cleared, no run intent.
8. **Multi-line `#` heading is a prompt.** `composer = "# Title\nbody"` submit → routes as an ordinary prompt (`StartRun`/`SubmitUserInput`), NOT `RememberMemory`.
9. **`/context` fills the meter.** Pure reducer: fold `EventBody::ContextUsage { used: 100_000, window: 200_000, … }` → `run.context_percent == Some(50)` and `run.context_breakdown` is `Some`. RUN `render_status_line`; EXPECT the `ctx` meter shows filled cells, not `—`.
10. **`/context` card shows the breakdown.** `PaletteCommand::Context` sets `Overlay::Context`; `render_context_card` lists system/tools/transcript/used/free from `ContextBreakdown`.
11. **Green gate.** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`, `.github/scripts/check_docs_manifest.py`, and the doc-test-count gate all pass.

## 8. Tests

**Pure reducer (`crates/tui/src/reduce.rs` `#[cfg(test)]`, matching the ~625-test idiom):**
- `bang_prefix_runs_user_shell_not_a_prompt` (criterion 4), `bare_bang_is_inert` (5).
- `hash_prefix_files_a_memory` (7), `multiline_hash_is_an_ordinary_prompt` (8), `bare_hash_is_inert`.
- `context_usage_projects_percent_and_breakdown` (9) — mirror the existing `budget_warning_tokens_brings_the_dead_context_footer_alive` (`reduce.rs:8855`).
- `context_palette_opens_the_card` (10); `context_palette_needs_a_run` (notice when no run).
- A prefix-precedence test: `!` and `#` are only special at composer start — `"echo !x"` and `"see #note"` submit as ordinary prompts.

**Runtime (`crates/runtime`):**
- `crates/runtime/src/instructions.rs` `#[cfg(test)]`: `discovers_root_to_cwd_in_order` (1), `does_not_cross_project_root` (2), `no_files_returns_none`, `respects_max_bytes`, `global_claude_md_is_lowest_precedence`.
- `agent.rs`: `system_prompt_includes_discovered_instructions` (3); `context_usage_event_dedups_by_percent` and `no_context_usage_when_window_unknown`.

**Daemon/integration:** `user_shell_turn_is_recorded_in_band` (6); `remember_memory_routes_through_curator` (Accepted → `remembered:` note; evidence-free candidate would be rejected — assert the user-authored `EvidenceRef` is attached).

## 9. Gotchas

- **AGENTS.md never crosses the project root** (codex `agents_md.rs` doc lines 8–16). `chain_root_to_cwd` uses `take_while(|d| d.starts_with(root))`; a test must assert a file one directory above the marker is excluded. An unset/empty marker list disables parent traversal (cwd only) — we hardcode `[".git", ".codypendent"]`, so this is only the "no marker anywhere" case.
- **Precedence direction is a trap.** Root-first means cwd is appended **last** and therefore wins in the prompt (later text overrides earlier). If you reverse the chain you silently invert precedence with no test failure unless a test asserts order (criterion 1 does).
- **`!` output must be recorded in-band and bounded/spilled like tool output.** Do not stream raw bytes into the note — reuse `Shell::execute`'s `salient` view (bounded) for the note and keep the full output behind the `ArtifactRef` (`shell.rs:220`). opencode caps and adds an abort trailer; our `MAX_CAPTURE_BYTES` cap already bounds it. The output must be a **user-authored** turn so the model treats it as "what the user ran," not a tool result it chose.
- **`!cmd` runs under the session scopes, never widened.** Reuse the session's `CommandScope`/`PathScope`; a `!` command must still be blocked by the program allow-list and path scope. It is user-initiated, so it does **not** go through the agent approval card — but it inherits the same deny-wins policy scopes (house rule: never weaken deny-wins).
- **`#` must not collide with markdown headings in a prompt.** A user legitimately sends a message like `# Plan\n1. …`. Guard the quick-add to a **single-line** composer (no `\n`); a multi-line `#`-led message falls through to normal prompt routing (criterion 8). This differs from Claude Code's live mode-switch, but keeps the reducer pure and the seam unambiguous.
- **`/context` event may already look like it exists — it does not.** `BudgetWarning{Tokens}` (a threshold warning, ≥80 %, budget-configured runs only) and `RunUsage` (terminal measured tally) both exist and both touch tokens, but neither is a live per-turn context-fill signal. Reusing `BudgetWarning` would spam the ledger and change its documented "warning" semantics — add the dedicated `ContextUsage` event instead. The **projection** side (`context_percent` → meter) is already wired, so only the emit + a breakdown field + the card are new.
- **`estimate_request_tokens` is a heuristic, not a tokenizer** (`CHARS_PER_TOKEN = 4`). The `/context` card must not imply exactness — label it "estimated." When `driver.context_window()` is `None`, emit nothing and let the meter stay `—` (the existing honesty rule; do not fabricate a denominator).
- **`curate` rejects evidence-free candidates.** `CandidateMemory.provenance` must have ≥1 `EvidenceRef` or the `#` quick-add silently rejects. Attach a user-authored evidence ref keyed to the session.
- **`to_messages` signature change ripples.** Adding the `instructions` parameter touches every `to_messages` call site and the driver plumbing; this is the one runtime-invasive edit in Action 17. The pure `discover_instructions` fn itself is trivially testable in isolation.

## 10. Out of scope

- **B4 prompt-template commands** (`.codypendent/commands/*.md` with `$1…$N`), **B1 plan mode**, **B7 web tools**, **B8 deferred-tool loading**, **B9 MCP server**, **B10 CLI JSON contract**, **B11 configurable keymap** — separate Track B items with their own specs.
- **A file watcher for instruction files** (codex `agents_md_manager` cache/watch). We re-read at run launch only; live re-read on file change is a later enhancement.
- **pi's `!!` excluded-from-context shell** — only the in-context `!` ships in v0.8.
- **Live `#`/`!` mode-switch as you type** (Claude Code enters a distinct input mode on the first `!`/`#` keystroke). We intercept at **submit** to keep `input.rs`/`reduce.rs` pure and single-seam; a keystroke-mode variant can layer on later.
- **`AGENTS.override.md` local overrides**, **per-file `.codypendentignore`**, and **user-instruction merging beyond the global `~/.claude/CLAUDE.md`** — codex has more layers; we port the core walk only.
- **Editing/curating memories from the composer** beyond quick-add — the existing `/memory` browser and `CorrectMemory`/`ForgetMemory` commands own that.
