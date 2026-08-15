# Adoption Specs — Index

This directory holds the full implementation specifications for the feature-adoption
plan derived from deep reviews of four coding agents/harnesses vendored under
`reference-repos/` (git-ignored):

| Repo | What it is | What we take from it |
|---|---|---|
| `reference-repos/codex` | OpenAI's codex CLI (Rust + ratatui, ~100 crates) | Backtrack/fork UX, unified exec, streaming pipeline, terminal integration, testing discipline |
| `reference-repos/opencode` | The open-source AI coding agent (bun/TS + Solid TUI) | Edit cascade, question tool, retry policy, arity permissions, LSP feedback, fork data-model |
| `reference-repos/cline` | Cline's CLI (`apps/cli`, Bun + OpenTUI/React) | Private-ref checkpoints, server-side prompt queue, hook file conventions, tuistory testing |
| `reference-repos/pi` | Pi agent harness (bun/TS) | Extension event-model vocabulary, supply-chain checklist (reference only — see rejections) |

The four review documents that ground these specs were produced by dedicated review
agents and the fit analysis lives in the git history of this plan; the specs are
self-contained — an implementation agent does not need the reviews to execute them.

## How to use these specs

Each numbered spec follows the house build-guide conventions
([`../build/00-how-to-use-this-guide.md`](../build/00-how-to-use-this-guide.md) §3):
literal code is normative for names and behaviour, RULES are MUST-level, migrations
are append-only, the deny-wins policy engine is never weakened, no `unsafe`. Every
spec has the same skeleton: Summary → Reference implementation → Current state
(verified against the code) → Design → Changes file-by-file → Protocol & persistence
→ Acceptance criteria → Tests → Gotchas → Out of scope.

A spec is DONE only when every acceptance criterion passes and its tests are green
under the standard gate (`cargo fmt --check`, `cargo clippy -- -D warnings`,
`cargo test`).

## Execution order

Ordered by dependency and impact-per-effort. Independent specs may be taken out of
order, but 04 → 05 is a hard dependency.

| Order | Spec | Effort | Status | Depends on | Delivers |
|---|---|---|---|---|---|
| 1 | [01 — Provider retry policy](01-provider-retry-policy.md) | S | ✅ Completed | — | Upgrades the existing fixed-schedule retry loop: typed retryable classification, retry-after honoring, jittered backoff, live retry status in the TUI |
| 2 | [02 — Edit replacer cascade](02-edit-replacer-cascade.md) | S | ✅ Completed | — | Nine-stage fuzzy matching in `workspace.edit_file` with safety valves |
| 3 | [03 — Question tool](03-question-tool.md) | S | ✅ Completed | — | Structured mid-run questions to the operator, parked durably like approvals; reject-with-feedback |
| 4 | [04 — Filesystem checkpoints](04-filesystem-checkpoints.md) | M | ✅ Completed | — | Per-turn worktree checkpoints as stash-commits under `refs/codypendent/checkpoints/`, transactional restore |
| 5 | [05 — Session fork + backtrack](05-session-fork-backtrack.md) | M | ✅ Completed | 04 | STEP 5.6: `ForkSession`, Esc-Esc transcript backtrack, composer refill |
| 6 | [06 — Prompt queue + steering](06-prompt-queue-steering.md) | M | ✅ Completed | — | Daemon-owned prompt queue: Enter queues while running, Tab edits, Enter promotes to steer |
| 7 | [07 — Arity permissions](07-arity-permissions.md) | M | ✅ Completed | — | "Always allow" learns `git checkout *` rules, not literal commands; command scanning for external paths |
| 8 | [08 — Hook engine](08-hook-engine.md) | M | ✅ Completed | — | Discovery + sandboxed execution of `.codypendent/hooks/` scripts feeding the existing verdict lattice |
| 9 | [09 — Unified exec (PTY)](09-unified-exec-pty.md) | L | ✅ Completed | — | Daemon-owned persistent PTY sessions with yield-based reads (dev servers, REPLs, debuggers) |
| 10 | [10 — LSP diagnostics feedback](10-lsp-diagnostics-feedback.md) | L | ✅ Completed | — | Live rust-analyzer/pyright; post-edit compiler diagnostics appended to tool output |
| 11 | [11 — UI polish suite](11-ui-polish-suite.md) | S–M each | ✅ Completed | varies | Session picker, context meter, paste intelligence, @-mentions, streaming pipeline, system theme, hyperlinks, notifications |
| 12 | [12 — Architecture & testing](12-architecture-and-testing.md) | S–M each | ✅ Completed | — | vt100 snapshot tests, PTY e2e harness, provider VCR, clippy enforcement, protocol schema export |
| 13 | [13 — Remote UI authoring SDK](13-remote-ui-authoring-sdk.md) | M | ✅ Completed | — | TSX authoring layer compiling to the shipped `UiDocument` protocol; alternatives analysis (React-in-ratatui) recorded |
| 14 | [14 — Tauri React companion client](14-tauri-react-companion-client.md) | L | ✅ Completed (Arch/Spec) | 12/A6 rec. | Desktop client: real React/shadcn UI as a sibling daemon frontend; shared Remote UI renderer for plugin parity |
| 15 | [15 — Claude Code comparison](15-claude-code-comparison.md) | ref | ✅ Completed (Ref/Gap) | — | Feature-by-feature comparison against Claude Code; net-new gap guidance (plan mode, instruction files, `!` prefix, prompt-template commands, web tools, …) |
| 16 | [16 — Master implementation plan](16-master-implementation-plan.md) | plan | 🗺️ Planning | — | Everything-to-1.0: four tracks (UI, parity, graphical client, engine) sequenced into v0.8→v1.0 releases with dependencies + specs-to-write |
| — | **v0.8 "Feels finished" — 21 actions across specs 17–21** | | | | |
| 17 | [17 — Transcript & modal redesign](17-transcript-and-modal-redesign.md) | M | ⬜ v0.8 | — | Actions 1–7: turn separation + role accent, heading ladder, tool-call + diff cards, unified modal, scrim, layering/composer-clip fixes |
| 18 | [18 — TUI polish & empty states](18-tui-polish-and-empty-states.md) | S | ⬜ v0.8 | 17 | Actions 8–14: centered empty states + CTAs, kanban backgrounds, hide empty `—`, splash inset, selection accent, contrast lift, list-width balance |
| 19 | [19 — Plan mode](19-plan-mode.md) | M | ⬜ v0.8 | — | Actions 15–16: plan profile (writes denied except the plan file) + `plan_enter`/`plan_exit` bridge tools over the shipped question tool + prompt queue |
| 20 | [20 — Composer parity](20-composer-parity.md) | M | ⬜ v0.8 | — | Actions 17–20: hierarchical `AGENTS.md`/`CLAUDE.md` instruction files, `!` shell prefix, `/context` card + live meter, `#` memory quick-add |
| 21 | [21 — CLI JSON-stream contract](21-cli-contract.md) | S | ⬜ v0.8 | — | Action 21: pin + document the existing `run --jsonl` / `attach --events jsonl` NDJSON contract with a framing/schema contract test |

Items in 11 and 12 are independent of each other and of the numbered specs; slot
them opportunistically alongside whichever crate is already open.

## Findings from spec-writing (read before implementing)

Verifying the plan against the code surfaced corrections now baked into the specs:

- **Steering text is currently dropped.** `apply_queue_steering`
  (`crates/daemon/src/commands.rs`) journals a marker but discards the text;
  `RunContext::with_steering` has no production caller. Spec 06 fixes this as its
  core work — treat it as a live bug, not a feature gap.
- **Retry already exists** in `crates/runtime/src/agent.rs` (fixed 1s/2s/4s, max
  3, with a streamed-text veto). Spec 01 upgrades that loop; the veto is kept —
  it is stronger than the reference behaviour.
- **`shell.run` takes structured `program`+`args`** and never re-parses shell
  strings, so spec 07 needs no shell lexer (zero new dependencies).
- **One-shot shell does not run under Seatbelt** — the Seatbelt/bwrap executor
  confines plugins; shell enforcement is CommandScope/PathScope/env gates. Spec
  09 defines "same enforcement" accordingly and keeps OS confinement of PTY
  sessions out of scope.
- **Worktrees are deleted at run end** (`WorktreeReleaseGuard`), so PTY sessions
  get a terminate-under-worktree-root hook (spec 09) and checkpoints are keyed
  `(run, ordinal)` rather than per-session turns (spec 04).
- **Dormant wire surface becomes live**: `ApprovalScope::Pattern`/`Repository`
  and `RunState::WaitingForUserInput` exist with DB mappings but zero producers —
  specs 07 and 03 are their first users.
- **Migration numbers** are assigned here to avoid collisions: `0034_questions`
  (03), `0035_run_checkpoints` (04), `0036_session_forks` (05),
  `0037_pending_prompts` (06), `0038_approval_patterns` (07). Renumber to the
  next free number if the sequence has moved by the time a spec lands.

## Rejected features (do not build)

Recorded here so future contributors do not re-litigate them:

- **Shadow-git snapshot repo** (opencode) — spec 04's private-refs design fits the
  existing real-git per-run worktrees; two snapshot mechanisms would be worse than
  either.
- **Code mode** (codex V8 / opencode interpreter — model-authored JS orchestrating
  tools) — embeds a JS runtime into a Rust local-first daemon; the planned wasmtime
  component runtime (Phase 6) is the sanctioned path for executable extensions.
- **pi's TS extension system** — wrong trust model; codypendent's plugin identity is
  signed manifests + capability diffs + sandbox profiles. Pi's *event-model
  vocabulary* remains a useful design reference for the Phase 6 hook/plugin API.
- **Scrollback-native rendering** (codex/pi) — a rewrite of the alt-screen shell and
  its pure-reducer test suite, not an adoption.
- **Chat connectors, live shares, cloud tasks** — out of scope for a local-first
  product with no server component.
- **Cron scheduling** (cline) — the durable workflow engine is the automation
  substrate; a second scheduler is scope creep.
- **Codex's two-phase memory pipeline** — duplicates the memory observer/curator +
  journey system; only the usage-citation retention idea may be folded into the
  existing curator later.
- **Doom-loop permission** (opencode) — already covered structurally by
  `MAX_CONSECUTIVE_IDENTICAL_CALLS` in `crates/runtime/src/agent.rs`.
- **ACP implementation** — shipped in Phase 3.6 (`crates/cli/src/acp.rs`).
- **Full LLM-summarization compaction tier** — deferred until a real
  context-exhaustion case demands it; must then follow the "compaction as an in-band
  ledger event" shape.
- **Terminal pets / mascots** — the effort budget belongs to the composer.
- **React-in-ratatui via `react-reconciler`, and Ink-app embedding for first-party
  panels** — see the alternatives analysis in
  [13 — Remote UI authoring SDK](13-remote-ui-authoring-sdk.md) §2; the declarative
  Remote UI protocol (shipped) + TSX SDK (spec 13) is the sanctioned path, with a
  Tauri companion client recorded as the future home for real browser components.
- **Remote model-catalog overlays** (pi) — adds a network dependency to a local-first
  product for little gain.
