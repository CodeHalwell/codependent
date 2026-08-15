# Adoption 14 — Tauri React Companion Client

**Effort:** L (standalone app; incremental milestones below) · **Depends on:** 12/A6 (protocol schema export) strongly recommended first; 13 (shared `UiDocument` React renderer) optional
**Ported from:** — (original; architecture follows the product boundary in `README.md`) · **Status:** ⬜ not started

## 1. Summary

A desktop client built with Tauri + React that attaches to `codypendentd` exactly
the way the TUI does — a second first-class frontend, not a replacement. This is
where genuinely graphical UI lives: real CSS, shadcn/MUI components, smooth
animation, charts, document preview, drag-and-drop, and rich diff views that a
terminal cell grid cannot express. The terminal client stays the fast,
keyboard-first surface; the desktop client is the visual one. Both are thin
projections of the same daemon state, so nothing forks: sessions started in one
are attachable from the other, and approvals/policy remain daemon-enforced no
matter which surface answers them.

```text
                       ┌───────────────────────┐
                       │   codypendentd core   │
                       │ sessions · runs ·     │
                       │ policy · knowledge    │
                       └───────────┬───────────┘
                    typed commands / events / state
                   ┌───────────────┴───────────────┐
         ┌─────────▼──────────┐          ┌─────────▼─────────┐
         │ Ratatui TUI client │          │ Tauri React client │
         │ keyboard-first     │          │ visual-first       │
         └────────────────────┘          └───────────────────┘
```

## 2. Why Tauri (and why not React-in-the-terminal)

Recorded fully in [13 §2](13-remote-ui-authoring-sdk.md): browser components
cannot render inside ratatui — there is no DOM/CSS/JS layer in a terminal buffer.
Tauri is the sanctioned home for them because:

- Frontend is plain HTML/JS/CSS (any React stack works unchanged — shadcn,
  Tailwind, MUI, Recharts, Monaco).
- Application logic can stay in **Rust**: the Tauri shell links the existing
  `codypendent-protocol` crate and the client connection code, so the wire codec
  is shared with the TUI rather than hand-duplicated in TypeScript (until A6's
  generated SDK makes a pure-TS client equally safe).
- Commands/events/channels give a typed Rust↔webview bridge that maps naturally
  onto the daemon's command/event protocol.

## 3. Current state in codypendent (verified)

- The product boundary already declares this client class: `README.md` lists
  "Ratatui TUI and additional clients (IDE/CLI/Web)" and "the backend owns
  intelligence and session state; all frontends are clients".
- `crates/protocol/` is the complete client contract (commands, events, remote-UI
  documents); `crates/cli/src/client.rs`/`connection.rs` hold the reference
  client implementation to reuse from the Tauri shell.
- The Remote UI protocol is renderer-independent by design
  (`crates/tui/src/remote_ui/mod.rs`: the terminal renderer is "a pure
  projection") — a React `UiDocument` renderer is explicitly anticipated.
- The roadmap already flags the cost of hand-duplicated wire codecs (the VS Code
  extension) — this spec must not add a second duplicate; hence the Rust-side
  bridge (§4) or A6-generated types.

## 4. Design

### 4.1 Process & transport

The Tauri app's Rust side owns the daemon connection (same Unix socket +
framing as the CLI/TUI). The webview never touches the socket. Bridge shape:

- Tauri **commands** wrap protocol commands (`start_run`, `steer`, `approve`,
  `fork_session`, …) — thin, typed, one per protocol command actually used.
- A Tauri **channel/event stream** forwards daemon events to the webview as
  JSON, tagged with the protocol's own event names — the React store mirrors
  daemon state the way the TUI reducer does (one store, events in, projections
  out; opencode's `sync.tsx` batched-flush pattern is the reference for keeping
  render churn low).
- Reconnect/replay uses the same last-seen-sequence resume the CLI stream uses.

### 4.2 Surface layout (v1)

```text
┌─────────────────────────────────────────────────────────┐
│ Title bar · command palette · session tabs              │
├───────────────────────┬─────────────────────────────────┤
│ Navigation (sessions, │ Transcript view (React):        │
│ workflows, docs,      │ streaming markdown, rich diffs, │
│ board, skills)        │ tool cards, approval cards      │
├───────────────────────┤─────────────────────────────────┤
│ Run status · budgets  │ Composer (queue/steer parity)   │
└───────────────────────┴─────────────────────────────────┘
```

v1 scope: transcript + composer + approvals + session list. Everything else
(workflow DAG, docs studio, kanban, dashboards) arrives as later milestones —
each is just another projection of daemon state the TUI already proves out.

### 4.3 Plugin parity via the shared Remote UI contract

With spec 13's SDK in place, a React `UiDocument` renderer
(`@codypendent/remote-ui-react`) projects the same plugin documents into styled
React components — one plugin, two surfaces, zero plugin changes. Trust rules
carry over unchanged: the desktop client renders host-owned approval surfaces
itself; plugin documents can never draw them.

### 4.4 What stays daemon-side (unchanged)

Policy, approvals, budgets, worktrees, ledger — the desktop client gets no
privileged path. An approval answered in the desktop client is the same durable
approval record the TUI would have written.

## 5. Milestones

| # | Milestone | Delivers |
|---|---|---|
| 1 | Shell + connection | Tauri app connects, lists sessions, streams events into a store |
| 2 | Transcript + composer | Read/write parity for a basic run: streaming markdown, tool cards, submit/steer |
| 3 | Approvals | Approval cards with the daemon's diff/command metadata; parity with TUI semantics |
| 4 | Rich views | Monaco-grade diffs, image rendering, per-run cost/budget panels |
| 5 | Remote UI React renderer | Plugin `UiDocument`s render natively (shared contract with spec 13) |
| 6 | Visual exclusives | Workflow DAG (interactive), docs preview, kanban drag-and-drop |

## 6. Acceptance criteria

1. The desktop client and TUI attach to the same daemon simultaneously; a run
   started in one streams live in the other, including approvals answered from
   either side (single durable approval record — verify in the ledger).
2. No hand-written TypeScript wire codec: every frame crossing the webview
   boundary is produced/parsed by the shared Rust protocol crate or by A6's
   generated types.
3. Disconnect/reconnect resumes from the last-seen sequence without dropping or
   duplicating transcript entries.
4. (Milestone 5) The spec-13 example plugin renders in both surfaces from one
   artifact, with host-owned trust surfaces intact in both.
5. The TUI test suite and daemon behaviour are unchanged — this is purely
   additive; CI proves it by running the existing gates untouched.

## 7. Gotchas

1. **Don't let the clients drift.** Every feature the desktop client adds must be
   a projection of daemon state, never client-local state — otherwise sessions
   stop being surface-portable. The TUI's "the TUI does no I/O" discipline is the
   model; the React store must obey the same rule.
2. **Event floods.** A streaming run emits deltas faster than React should
   render; batch event application per animation frame (opencode batches SSE
   flushes for exactly this reason).
3. **Webview security posture.** The webview must have no filesystem/network
   capability beyond the Tauri command bridge; all I/O rides the daemon protocol
   so policy still mediates everything.
4. **Two renderers, one theme identity.** Map the TUI's semantic theme tokens to
   CSS variables from the start, or the surfaces will diverge visually and
   plugin documents will look foreign in one of them.
5. **Don't gate the terminal roadmap on this.** The TUI remains the primary
   surface; desktop milestones are additive and independently shippable.

## 8. Out of scope

- Web deployment of the client (Ratzilla/WASM or hosted web) — future option.
- Mobile.
- Any daemon-side feature work — this client consumes existing protocol only.
- Replacing the VS Code/Zed integrations — those remain separate clients.
