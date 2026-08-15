# Master Implementation Plan — Everything to 1.0

**Status:** planning · **Supersedes:** the scattered "what's next" notes in specs 13–15 and ROADMAP Phase 6/7

This is the single sequenced plan for **all** outstanding work: the UI/UX findings from
the v0.7 screenshot review, the Claude Code parity gaps (spec 15 §9), the two design-only
specs (13 TSX SDK, 14 Tauri client), and the remaining roadmap engine slices (Phase 6/7).
It groups everything into four **tracks**, then sequences them into four **releases**
(v0.8 → v1.0), each run exactly like v0.7: spec-first → parallel implementation →
adversarial review → green gate → PR → tag.

---

## The four tracks

### Track A — Make it feel finished (UI/UX)
From the screenshot review. All in `crates/tui` (+ a little `crates/protocol` for `/context`).
Highest *visible* impact; the transcript is the screen users actually stare at.

| ID | Item | Effort |
|----|------|:------:|
| A1 | **Transcript redesign** — per-turn separation + role identity/accent, real markdown heading hierarchy (headings/bold/lists), and syntax-highlighted **tool-call + diff cards** (`+N/−N`). | M |
| A2 | **Unified modal system** — one modal component (consistent max-width, centering, height), a **scrim** dimming the transcript behind every overlay, and fixing the transcript-bleed-through + composer-label-clipping layering bugs. | M |
| A3 | **Empty states** — center + CTA for Docs Studio / Blackboard / Kanban / Journey / Remote-UI-plugins; fix the stray Docs thumbnail artifact; subtle Kanban column backgrounds. | S |
| A4 | **Polish pass** — hide empty `—` fields (status bar `ctx ——`, detail `cost:/context:`), center the splash + inset the version string, left blue-accent selection bar, muted-tier contrast lift (uses the spec 12/A4 semantic tokens), widen truncated list rows vs. sparse detail panes. | S |

### Track B — Daily-driver parity (spec 15 §9)
Compose over machinery that already exists. `/context` (B5) closes the loop with A4's `ctx ——`.

| ID | Item | Effort | Notes |
|----|------|:------:|-------|
| B1 | **Plan mode as composition** — plan profile (writes denied except a plan file) + `plan_enter`/`plan_exit` bridge tools + synthetic approval, over the existing policy engine. Pairs with the shipped mode picker. | M | highest-value feature |
| B2 | **Hierarchical instruction files** — `.codypendent/` + `AGENTS.md`/`CLAUDE.md` discovery cwd→root, concatenated in order. | S | |
| B3 | **`!` shell prefix** — user-run commands recorded as transcript turns (opencode's `shell()` shape). | S | |
| B4 | **Prompt-template commands** — `.codypendent/commands/*.md` with `$1…$N`/`$ARGUMENTS` substitution. | M | |
| B5 | **`/context` breakdown card** — surface the token accounting the runtime already computes; feeds A4's footer meter. | S | |
| B6 | **`#` memory quick-add** — composer prefix routes to the memory observer. | S | |
| B7 | **Policy-gated web tools** — WebFetch/WebSearch behind network-egress approvals. | M | |
| B8 | **Deferred-tool loading** — ToolSearch-style lazy schemas for large rosters. | M | needs MCP client |
| B9 | **MCP server mode** — expose the daemon to other agents. | S | needs MCP client |
| B10 | **One-shot + JSON-stream CLI contract** — verify, stabilize, document. | S | |
| B11 | **Configurable keymap** — codex `RuntimeKeymap` port (config-resolved, conflict-validated, hints reflect real bindings). | M | |

### Track C — The graphical client
The ceiling for "customisable and really professional-looking," and the direct answer to
the React question. Specs already written.

| ID | Item | Effort | Notes |
|----|------|:------:|-------|
| C1 | **Spec 13 — TSX Remote UI authoring SDK** — React-style authoring compiling to the shipped `UiDocument` protocol. | M | benefits from spec 12/A6 schema export |
| C2 | **Spec 14 — Tauri React companion client** — desktop UI as a sibling daemon frontend, milestones 1–6. | L | milestone 5 (shared React `UiDocument` renderer) needs C1 |

### Track D — Finish the engine (roadmap Phase 6/7)
Completes the stated architecture; flips the two 🟡 phases to ✅. Least user-visible, most infra.

| ID | Item | Effort |
|----|------|:------:|
| D1 | **6.2/6.3/6.4** — `wasmtime` component runtime + WASM plugin SDK, brokered-secrets host, and full OS sandbox enforcement consuming `SandboxProfile` (macOS Seatbelt ✅ exists; Linux bwrap+seccomp / Windows AppContainer to wire). | L |
| D2 | **6.5/6.7** — client capture (TUI clipboard/voice, IDE drag-drop → input model) + the agentic `setup` assistant under a restricted profile. | M |
| D3 | **Phase 7** — a live *measured* routing run + real shadow/canary measurement paths (router seam + promotion gate already exist). | M |

---

## Dependency graph (what must precede what)

```
A4 ──uses──> spec 12/A4 semantic tokens (shipped)
B5 ──feeds──> A4 footer meter            (do B5 and A4 together)
B1 ──over──> policy engine (shipped)
B8, B9 ──need──> MCP client (Track B, v0.9)
C2 milestone 5 ──needs──> C1
C1 ──benefits from──> spec 12/A6 schema export (partially shipped)
D1 ──consumes──> SandboxProfile (shipped, STEP 6.1)
Everything else is independent.
```

Only three hard edges: **B8/B9 after MCP client**, **C2-m5 after C1**, **C2 after (recommended) A6 schema export**. Everything else can be scheduled freely.

---

## The release sequence

Four releases to 1.0. Each is one v0.7-style cycle (spec → implement → review → gate → PR → tag).

| Release | Theme | Contents | Rough size |
|---------|-------|----------|:----------:|
| **v0.8 — "Feels finished"** | The app users stare at looks professional, plus daily-driver parity | A1, A2, A3, A4 + B1, B2, B3, B5, B6, B10 | large |
| **v0.9 — "Extensible & connected"** | Tool ecosystem + authoring | MCP client, then B8, B9, B4, B7, B11, C1 | large |
| **v0.10 — "The graphical client"** | The beautiful desktop UI | C2 (Tauri milestones 1–6, m5 uses C1) | large |
| **v1.0 — "Complete the architecture"** | Flip the 🟡 phases green | D1, D2, D3 | large |

Rationale for the order: **A before everything** because the screenshots show the biggest gap
is polish, and it's cheap; **the cheap half of B rides with A** (they share the composer and
`/context`); **MCP leads v0.9** because it unlocks B8/B9 and is the last big parity gap; **C is
its own release** because a desktop app is a distinct surface with its own build/CI; **D is
last** because it's infra that doesn't change what the user sees day-to-day, and WASM (D1) is the
single largest chunk.

---

## Per-release detail

### v0.8 — "Feels finished"

**New specs to write first** (the UI work has no specs yet; §9 items are one-liners that need expanding):
- `17-transcript-and-modal-redesign.md` — covers A1 + A2 (the two structural UI changes), with vt100 snapshot acceptance criteria.
- `18-tui-polish-and-empty-states.md` — A3 + A4 (S-tier, batched).
- `19-plan-mode.md` — B1, the composition design (profile + bridge tools + synthetic message).
- `20-composer-parity.md` — B2 + B3 + B5 + B6 (instruction files, `!`, `/context`, `#`) as one composer/session batch.
- `21-cli-contract.md` — B10 (mostly verify + document the existing seam).

**Exit criteria:** transcript has turn separation + heading hierarchy + tool/diff cards; every overlay dims the background via a shared modal; empty states are centered with CTAs; `ctx`/`cost` empty fields hidden and `/context` populates the meter; plan mode denies writes except the plan file and round-trips through `plan_exit`; `!cmd`/`#memory` work in the composer; `AGENTS.md`/`CLAUDE.md` load cwd→root; `codypendent --json` is a documented stable contract. Full gate + doc-count/extension gates green.

### v0.9 — "Extensible & connected"

**New specs:**
- `22-mcp-client.md` — the MCP client + OAuth + prewarm (Phase 6 plugin-manager slice), the prerequisite for B8/B9.
- `23-deferred-tools-and-mcp-server.md` — B8 + B9 (lazy schemas; expose daemon as MCP server).
- `24-prompt-template-commands.md` — B4.
- `25-web-tools.md` — B7 (network-egress-gated WebFetch/WebSearch; codex per-host approvals as the bar).
- `26-configurable-keymap.md` — B11.
- Spec **13** (TSX SDK) is already written — implement as-is; land the spec 12/A6 schema export first if not already, so `13`'s prop types are generated.

**Exit criteria:** MCP servers connect (with OAuth) and their tools are callable under policy; large rosters load deferred; the daemon answers as an MCP server; `.codypendent/commands/*.md` become slash commands; web tools work only behind approvals; keymap is user-configurable with conflict validation; a TSX example plugin renders through the existing Remote UI host.

### v0.10 — "The graphical client"

**Specs:** **14** (Tauri) is already written. Execute its milestones 1–6. Milestone 5 (shared
React `UiDocument` renderer) consumes C1 from v0.9.

**Exit criteria:** desktop client attaches to the same daemon as the TUI; a run started in one
streams live in the other incl. approvals answered from either side (single ledger record);
disconnect/reconnect resumes from last-seen sequence; the C1 example plugin renders in both
surfaces from one artifact; the TUI + daemon test suites are unchanged (purely additive).

### v1.0 — "Complete the architecture"

**New specs:**
- `27-wasm-runtime-and-os-sandbox.md` — D1 (the big one: wasmtime component runtime + WASM SDK + brokered secrets + Linux/Windows OS enforcement consuming `SandboxProfile`).
- `28-client-capture-and-setup-assistant.md` — D2.
- `29-measured-routing.md` — D3 (live measured routing + shadow/canary).

**Exit criteria:** a WASM plugin runs under enforced OS confinement and cannot touch undeclared
path/network; secrets are brokered, never handed to plugin code; client capture (clipboard/voice/
drag-drop) feeds the input model; the setup assistant proposes but never silently changes; a live
measured routing run drives promotion through the ADR-010 human gate. ROADMAP Phases 6 and 7 flip
to ✅.

---

## Methodology (same as v0.7 — it worked)

For each release:
1. **Write the specs first** (list above), one file per feature under `docs/docs/adoptions/`,
   following the house template (Summary → Reference → Current state (verified) → Design →
   Changes file-by-file → Protocol & persistence → Acceptance → Tests → Gotchas → Out of scope).
   Fork one spec-writer agent per 2–3 specs so they run in parallel; each verifies claims against
   the real code (this is where v0.7 caught the dropped-steering bug and the always-failing fork
   insert).
2. **Implement** with fork agents partitioned by **disjoint file ownership** (no two agents touch
   the same file), each adding a regression test per change and running the gate on its crates.
3. **Adversarial review** the merged diff (`/code-review high` + Codex on the PR) — green tests
   are necessary, not sufficient; v0.7's review found a policy bypass and cross-user leaks the
   tests missed. Verify each finding against the code before fixing.
4. **Green the full gate**: `cargo fmt`, `cargo clippy --workspace --all-targets -D warnings`,
   `cargo test --workspace`, **plus** the `doc-counts` job (test-count markers + `docs/MANIFEST.json`
   via `check_docs_manifest.py --fix`) and the `extension` job (protocol-vector partition +
   vitest markers) — the two gates local `cargo test` never checks. See the `codypendent-doc-gates`
   note.
5. **Ship**: release commit (bump the single-source workspace version), PR, address review, tag
   `v0.x` at the merge commit to fire the release build. Push via the owner account, restore after.

---

## Risks & sequencing notes

- **Migration numbers collide across parallel spec-writers** — assign them centrally per release
  (as v0.7 did: 0034–0038). Next free is `0039`.
- **The doc-count/extension gates bite on every release** — adding tests drifts the ROADMAP markers
  and adding wire vectors breaks the extension partition. Budget a fix commit each time.
- **WASM (D1) is the largest single unit** — it may warrant splitting into its own point releases
  (v1.0-beta.N) rather than one drop.
- **Tauri (C2) introduces a second build target + CI** — the release workflow currently builds only
  the Rust binaries; adding a desktop build is part of that release's scope.
- **Track A is worth doing first even though it's not a "feature"** — the screenshots show the app's
  perceived quality is capped by polish, not capability; fixing that raises the value of every other
  track's work.

---

## One-glance summary

- **v0.8** = the screenshot review + cheap parity → *looks and feels professional.*
- **v0.9** = MCP + tool ecosystem + TSX authoring → *extensible.*
- **v0.10** = Tauri desktop client → *beautiful, graphical, the React answer.*
- **v1.0** = WASM + OS sandbox + capture + measured routing → *architecture complete.*

Recommended first move: write specs `17`–`21` and run the v0.8 cycle, leading with the transcript
redesign (A1).
