# Adoption 15 — Claude Code Feature Comparison

**Effort:** — (reference document, not an implementation spec) · **Depends on:** nothing
**Reference:** Claude Code (Anthropic's CLI/agent harness) — not vendored; compared from product knowledge as of 2026 · **Status:** reference

## 1. Why this document

The four vendored repos (codex, opencode, cline, pi) were reviewed from source.
Claude Code is the fifth reference — arguably the category-defining harness, and
the one being used to build codypendent — but it is closed-source, so this is a
feature-inventory comparison rather than a code review. Each row states what
Claude Code does, where codypendent stands, and the guidance: **covered** (by an
existing spec or shipped feature), **adopt** (new gap worth closing), or **skip**
(with reason). Rows marked adopt feed the guidance list in §9.

A note on posture: codypendent should not chase Claude Code feature-for-feature.
Its differentiators — the persistent daemon, deny-wins policy engine, durable
declarative workflows, the knowledge fabric/code graph, Docs Studio, and the
signed-manifest Remote UI plugin host — are things Claude Code does *not* have
(§8). The comparison is for finding genuine gaps, not for convergence.

## 2. Sessions & conversation

| Claude Code feature | What it does | codypendent status | Guidance |
|---|---|---|---|
| `--continue` / `--resume` with picker | Resume the last or any prior session; picker shows sessions with previews | Sessions are durable but `attach` needs a session id; no picker | **Covered** — spec 11/S1 (session picker) |
| Rewind / checkpoints (double-Esc) | Restore conversation and/or files to an earlier point; checkpoints taken automatically | No filesystem undo | **Covered** — specs 04 + 05 |
| Session forking | Branch a session from an earlier point | STEP 5.6 open | **Covered** — spec 05 |
| Message queueing while working | Messages typed mid-run queue and deliver at the next safe point | Steering exists; no visible editable queue | **Covered** — spec 06 |
| Auto-compaction + `/compact` | Summarize history when context fills; manual trigger; microcompaction of old tool results | Mid-run observation folding exists (the "prune" tier, artifact-backed) | **Partially covered** — full LLM-summary tier deliberately deferred (index, rejected list); revisit when a real exhaustion case appears |
| `/context` | Visual breakdown of what is occupying the context window | Nothing user-facing | **Adopt (S)** — a `/context` palette entry rendering the runtime's existing per-run token accounting as a breakdown card; pairs with spec 11/S2's footer meter |
| Session naming/search | `/rename`, searchable resume list | Ledger has run metadata | Fold into spec 11/S1 |

## 3. Memory & instructions

| Claude Code feature | What it does | codypendent status | Guidance |
|---|---|---|---|
| `CLAUDE.md` hierarchy | Global → project → subdirectory instruction files, loaded by proximity | Repo config via `.codypendent/`; knowledge fabric holds memories | **Adopt (S)** — hierarchical instruction-file discovery (walk cwd→root, concatenate in order — codex's `agents_md.rs` is the open-source reference); read `AGENTS.md`/`CLAUDE.md` too, matching opencode's ecosystem pragmatism |
| Auto-memory | Agent-maintained memory directory with an index (`MEMORY.md`) loaded each session; one fact per file with provenance | **Shipped and richer** — memory fabric with provenance, curation, Journey review (`crates/knowledge`) | Keep; the one stealable idea is codex's usage-citation retention feedback (already noted in index) |
| `#` quick-add memory | Type `#` prefix to file a memory without leaving the flow | No composer affordance | **Adopt (S)** — composer prefix that routes to the existing memory observer; trivial once the composer work (spec 06/11) lands |
| Output styles | Swap the system-prompt persona/verbosity profile | Agent profiles exist (`agent.toml`, roles) | **Covered in spirit** — expose profile switching in the palette; no new machinery |

## 4. Extensibility

| Claude Code feature | What it does | codypendent status | Guidance |
|---|---|---|---|
| Skills (`SKILL.md` + frontmatter, progressive disclosure) | Descriptions in prompt; body loaded on invoke; user/project/plugin scopes | **Shipped** — Skill Studio, registry, versioning, tests, sandboxed scripts | Keep; codypendent's is more governed. Consider reading `.claude/skills/` for interop (opencode does) |
| Slash commands (`.claude/commands/*.md`) | Markdown prompt templates with `$1…$N`/`$ARGUMENTS` substitution | Palette has built-ins; no user-defined prompt commands | **Adopt (M)** — user command files under `.codypendent/commands/` with bash-style arg substitution (pi's `prompt-templates.ts` is the reference implementation) |
| Hooks (settings.json, PreToolUse/PostToolUse/Stop/…) | Shell commands on lifecycle events; can block/modify | Verdict lattice shipped, discovery missing | **Covered** — spec 08 |
| MCP client (+ OAuth, deferred tools, ToolSearch) | Connect external tool servers; lazy schema loading for large rosters | MCP planned in Phase 6 plugin manager | Fold into Phase 6; **the deferred-tool/ToolSearch pattern is worth copying** when tool rosters grow (pi's native deferred loading is the open-source reference) |
| MCP server mode | The harness exposes *itself* as an MCP server | Not planned | **Adopt (S, later)** — cheap daemon surface once MCP client lands; lets other agents drive codypendent |
| Subagents (`.claude/agents/*.md`, Task tool, forks, background) | Declarative agent types with model/tool overrides; parallel background execution; fork-with-context | Council + workflow agent nodes + role→profile enforcement shipped | **Covered differently** — codypendent's workflow engine is the stronger substrate. Gap: a lightweight ad-hoc "spawn one helper now" path outside declarative workflows — consider exposing council/workflow machinery as a single `delegate` tool (opencode's task tool shape, incl. background + in-band result injection) |
| Plugins/marketplace | Installable plugin bundles (commands, agents, MCP servers) | Remote UI plugins shipped; marketplace is an **explicit non-goal** | **Skip** marketplace (build-guide rule 11); the packaging format idea (one manifest for skills+commands+themes, pi's `pi` manifest) is worth borrowing for skill packages |
| Agent SDK | Programmatic harness embedding (TS/Python) | Protocol crate is the equivalent seam | **Covered** — spec 12/A6 (schema export + generated clients) is the path |

## 5. Execution & safety

| Claude Code feature | What it does | codypendent status | Guidance |
|---|---|---|---|
| Permission modes + allowlists | Ask/accept-edits/plan/bypass modes; per-tool allow rules in settings | Deny-wins policy engine + durable approvals — **stronger** | Keep; spec 07 (arity learning) closes the "always allow learns a rule" gap |
| Plan mode | Read-only exploration mode; presents a plan for approval before editing | Modes exist per agent profile, but no first-class plan/act cycle | **Adopt (M)** — opencode's composition is the reference: a plan profile whose ruleset denies writes except a plan file, plus bridge tools (`plan_enter`/`plan_exit` using spec 03's question tool) and a synthetic approval message. Mostly configuration over the existing policy engine |
| Background tasks + Monitor | Long commands run detached, harness re-invoked on completion; log monitoring with until-conditions | One-shot shell w/ artifact spill | **Covered** — spec 09 (unified exec) is the superset; add a completion-notification event so a finished background process wakes the run |
| Sandboxed execution | OS-level sandboxing of bash where available | **Shipped** — Seatbelt enforcement v1, bubblewrap generator | Keep; ahead of Claude Code on Linux story once bwrap wiring lands |
| Git worktree isolation | `EnterWorktree` for isolated feature work | **Shipped** — per-run isolated worktrees | Covered |
| WebSearch / WebFetch | Built-in web tools | Not present | **Adopt (M, gated)** — as policy-gated tools (network egress is an approval class already conceptually present in the sandbox profile work; codex's per-host network approvals are the reference bar) |
| Browser automation (Chrome extension) | Drive the user's browser | Explicit non-goal (desktop computer-use) | **Skip** (build-guide rule 11) |
| Vision/multimodal input | Paste images; read PDFs/notebooks | Phase 6 multimodal input model shipped; client capture open | Fold into Phase 6 client-capture work; spec 11/M1 covers image paste plumbing in the composer |

## 6. UI/UX

| Claude Code feature | What it does | codypendent status | Guidance |
|---|---|---|---|
| `@`-file mentions with fuzzy search | Popup completion for files | Missing | **Covered** — spec 11/M2 |
| `!` bash prefix | Run a shell command in-session; output enters context | Missing | **Adopt (S)** — composer prefix routing to the existing shell tool as a user-initiated, transcript-recorded run (opencode's `shell()` records it as a session turn — copy that shape) |
| AskUserQuestion | Structured multi-choice questions from the agent | Missing | **Covered** — spec 03 |
| Todo list rendering | Agent-maintained task list rendered live | Blackboard + workflow views exist; no lightweight per-run todo | **Skip for now** — the workflow graph covers the structured case; revisit if single-run UX demands it |
| Vim mode / keybindings.json | Modal editing; user-remappable keys | Fixed keymap | **Adopt (M, later)** — codex's `RuntimeKeymap` (config-resolved, conflict-validated, hint rendering reflects actual bindings) is the port target; vim mode only if demanded |
| Custom statusline | User-scriptable status line | Contextual footer shipped | **Skip** — footer already adapts; scriptability is low-value here |
| Themes | Light/dark/ANSI | **Shipped and richer** — theme packs, live preview | Keep; spec 11/M4 adds system-palette synthesis |
| Spinner/verb customization, tips | Personality touches | Minimal | Optional polish; nothing to spec |
| Desktop/web/IDE surfaces | CLI + desktop app + claude.ai/code + VS Code/JetBrains | TUI + VS Code/Zed (Phase 3); desktop = spec 14 | **Covered** — spec 14 |
| Terminal setup helpers (`/terminal-setup`, Shift+Enter) | Configures terminal keybindings | Missing | Fold into spec 11/S4 terminal integration |

## 7. Automation & headless

| Claude Code feature | What it does | codypendent status | Guidance |
|---|---|---|---|
| Print mode (`-p`) + `--output-format stream-json` | One-shot scripted runs; typed NDJSON event stream | `crates/cli` has run/stream commands over the protocol | **Partially covered** — verify the CLI exposes a stable one-shot + JSON-stream contract and document it as such (cline's tested `--json \| jq` contract is the bar); small spec-less task |
| GitHub Actions integration | `@claude` in PRs/issues triggers runs in CI | Phase 3 GitHub automation shipped (webhooks, PR flows) | **Covered differently**; a packaged Action is a distribution task, not a feature gap |
| Scheduled/cloud agents | Cron-style cloud routines | Out of scope (local-first); cron rejected in index | **Skip** — workflows + external cron cover it |
| OTEL telemetry | Metrics/traces export | Eval/observability crates exist | Fold into existing observability roadmap |

## 8. What codypendent has that Claude Code does not

For calibration — these are the moats; adoptions must not erode them:

- **Persistent daemon with durable, replayable sessions** (event-sourced ledger,
  crash recovery) — Claude Code sessions are per-process JSONL.
- **Deny-wins policy engine with durable, auditable approvals** — richer than
  permission modes; approvals survive restarts and carry evidence.
- **Declarative multi-agent workflows** with checkpoints, budgets, blackboard,
  per-run worktrees, and role→profile enforcement by policy.
- **Knowledge fabric**: provenance-carrying memory, curated Journey learnings,
  revision-aware code graph, semantic retrieval.
- **Docs Studio** — CRDT collaborative documents with approval-gated publication.
- **Governed plugin UI** — signed manifests, permission diffs, host-owned trust
  controls (Claude Code plugins have no comparable trust surface).
- **Model routing/eval substrate** — router seam, graders, promotion persistence.

## 9. Net-new guidance (gaps not covered by specs 01–14)

Ranked; each is small enough to not need a full spec yet — file issues from here:

1. **Plan mode as composition** (M) — plan profile + bridge tools + synthetic
   approval message over the existing policy engine (§5). Highest-value gap.
2. **Hierarchical instruction files** (S) — `.codypendent/` + `AGENTS.md`/
   `CLAUDE.md` discovery cwd→root (§3).
3. **`!` shell prefix** (S) — user-run commands recorded as transcript turns (§6).
4. **User prompt-template commands** (M) — `.codypendent/commands/*.md` with arg
   substitution (§4).
5. **`/context` breakdown card** (S) — surface existing token accounting (§2).
6. **`#` memory quick-add** (S) — composer prefix → memory observer (§3).
7. **Web tools, policy-gated** (M) — WebFetch/WebSearch behind network-egress
   approvals (§5).
8. **Deferred-tool loading** (M, when MCP lands) — ToolSearch-style lazy schemas
   to keep large rosters out of context (§4).
9. **MCP server mode** (S, after MCP client) — expose the daemon to other agents
   (§4).
10. **One-shot + JSON-stream CLI contract** (S) — verify, stabilize, document (§7).
11. **Configurable keymap** (M, later) — codex `RuntimeKeymap` port (§6).

Explicitly skipped from Claude Code: plugin marketplace, browser automation,
cloud/scheduled agents, scriptable statusline, todo-list widget — each for the
reason given in its row.
