# Adoption 13 — TypeScript Remote UI Authoring SDK (TSX → UiDocument)

**Effort:** M · **Depends on:** nothing (builds on the shipped Remote UI host) · **Reference:** own design; Ink and `react-reconciler` as prior art
**Ported from:** — (original; the alternatives analysis below records why) · **Status:** ⬜ not started

## 1. Summary

Plugin authors should be able to write Remote UI plugins in TSX — React-style
components, hooks, and composition — without codypendent ever embedding a browser,
a DOM, or React DOM. This spec adds a **TypeScript authoring SDK** that compiles a
restricted TSX component model into the **existing, shipped** Remote UI protocol
(`codypendent_protocol::remote_ui::UiDocument`), executed inside the existing
sandboxed UI worker (`crates/ui-host`, `crates/ui-worker-launcher`). Nothing about
the Rust side's trust model changes: the TUI renderer stays a pure projection that
never executes producer-supplied data, and Rust keeps definitive ownership of
layout, focus, theming, and permissions.

The core boundary principle, which this spec makes normative for all future UI
extensibility work:

> **TypeScript components describe semantic terminal UI; Rust decides exactly how
> that UI is laid out, styled, focused, and rendered.**

## 2. The integration landscape (alternatives analysis)

A browser React component cannot be dropped into a native ratatui panel. Ratatui
renders Rust `Widget` implementations into a terminal-cell `Buffer`; there is no
DOM, CSS engine, or JS runtime for React DOM to mount into. The realistic
architectures, and where codypendent stands on each:

| Requirement | Architecture | Verdict for codypendent |
|---|---|---|
| Let plugin authors write TSX for the ratatui interface | Declarative remote-component protocol + TSX compiler | **This spec.** The protocol half already shipped (Phase 6); the SDK is the delta |
| Use MUI/shadcn/browser components unchanged | Tauri + React webview beside a terminal pane | Future companion client (§9); never through the terminal renderer |
| Full custom React renderer targeting ratatui directly | `react-reconciler` custom renderer in-process | **Rejected** — `react-reconciler` is officially experimental with an unstable API; a production plugin SDK cannot absorb that maintenance burden, and it would push layout authority into plugin code |
| Embed an existing Ink application | PTY capture + ANSI→Buffer translation | **Rejected for first-party panels** — two competing focus/cursor/theme systems, nested alt-screen behaviour, resize races. Viable only as whole-external-tool embedding, which rides Adoption 09's PTY machinery, not the Remote UI path |
| Run the ratatui interface in a browser | Ratzilla/WASM | Out of scope; noted as a future web-client option — it is the *opposite* direction and does not help plugin authoring |
| Share one application between TUI and desktop | Rust core with separate ratatui and React clients | **Already the architecture** (daemon owns state; all frontends are clients) — the Tauri client (§9) slots into it |

What a TSX SDK can and cannot reuse from the React ecosystem:

```text
Reusable                          Not reusable directly
────────────────────────────────  ─────────────────────────────
React state and reducers          DOM elements (<div>, <button>)
Context and composition           CSS / Tailwind rendering
Pure TypeScript logic             MUI and shadcn components
Validation schemas                Canvas and SVG
Data-fetching logic               Browser event objects
State machines                    document/window/localStorage
Custom terminal hooks             DOM measurements
```

## 3. Current state in codypendent (verified)

Everything below already exists and is the substrate this spec builds on — the SDK
must target it, not replace it:

- **`crates/protocol/src/remote_ui.rs`** — the versioned Remote UI protocol
  (`UiProtocolVersion::V1`). `UiDocument` trees of named primitives
  (`primitives::{BOX, STACK, ROW, GRID, SPLIT, SPACER, SCROLL_AREA, VIRTUAL_LIST,
  TEXT, MARKDOWN, CODE, DIFF, IMAGE, JSON_TREE, LOG_VIEWER, LIST, TABLE, TREE,
  KEY_VALUE, TIMELINE, GRAPH, CHART, SPARKLINE, BADGE, PROGRESS, SPINNER, ALERT,
  TOAST, EMPTY_STATE, ERROR_BOUNDARY, TABS, BREADCRUMB, MENU, COMMAND_LIST, …}`),
  `UiActionBinding` for interaction, `UiCapabilitySelection` + per-node `requires`
  for capability negotiation, `UiHardLimits`, `UiSemanticRole` for accessibility,
  and fallback survival for unknown primitives. **The protocol is the contract;
  this spec adds no new primitives.**
- **`crates/tui/src/remote_ui/`** — the native renderer. Its module doc is the law:
  *"deliberately a pure projection… performs no I/O and never executes producer
  supplied data."* It consumes a validated `UiDocument` + `TerminalUiCapabilities`
  + semantic theme tokens, paints a `Buffer`, and returns interaction metadata
  (hit-test rects → action bindings). Layout, focus traversal, theming, clipping,
  Unicode cell-width handling, and accessibility projection all live here
  (`layout.rs`, `paint.rs`, `text.rs`, `accessibility.rs`, `codec.rs`).
- **`crates/ui-host/`** — worker runtime (`runtime.rs`, `session.rs`,
  `registry.rs`, `store.rs`, `framing.rs`) and **`crates/ui-worker-launcher/`** —
  the sandboxed worker process. Workers produce documents and receive actions over
  the framed protocol.
- **Trust machinery** — signed plugin manifests, permission diffs shown verbatim,
  host-owned approve/enable/revoke overlays
  (`crates/tui/src/state.rs` `Overlay::UiPlugins`,
  `Overlay::ConfirmUiPluginApprove/Enable/Revoke`;
  `crates/daemon/src/remote_ui_plugins.rs`). Plugin code can never draw or
  intercept its own trust controls.

What does **not** exist: any authoring experience. Today a plugin author must emit
`UiDocument` JSON by hand. That is the gap.

## 4. Design

### 4.1 Shape

```text
TypeScript plugin (TSX)
    │  @codypendent/remote-ui SDK: JSX runtime + hooks + diffing
    │  emits a validated UiDocument (JSON) per revision
    ▼
UI worker process (existing sandbox, crates/ui-worker-launcher)
    │  framed protocol (crates/ui-host/src/framing.rs)
    ▼
Rust host (crates/ui-host)
    │  schema validation, hard limits, permission enforcement
    ▼
Native ratatui renderer (crates/tui/src/remote_ui) — pure projection
```

The SDK is a **worker-side library only**. No Rust code learns anything about
React; the host continues to see exactly the documents it sees today. This means
the SDK can version independently of the daemon, and non-TSX producers (any
language that can emit the JSON) remain first-class.

### 4.2 The authoring model

Components are functions over a restricted primitive set that mirrors the protocol
one-to-one. No HTML elements exist in the JSX namespace:

```tsx
import { Panel, Row, Text, Badge, Button } from "@codypendent/remote-ui";

interface AgentCardProps {
  agent: { id: string; name: string; status: "idle" | "running" | "failed" };
}

export function AgentCard({ agent }: AgentCardProps) {
  return (
    <Panel id={`agent:${agent.id}`} title={agent.name}>
      <Row gap={1}>
        <Text>Status:</Text>
        <Badge
          tone={
            agent.status === "running" ? "positive"
            : agent.status === "failed" ? "critical"
            : "muted"
          }
        >
          {agent.status}
        </Badge>
      </Row>
      <Button action="open-agent" payload={{ agentId: agent.id }}>
        Open
      </Button>
    </Panel>
  );
}
```

`Panel`, `Row`, `Badge`, `Button` compile to the existing protocol primitives
(`BOX` with title, `ROW`, `BADGE`, and an action-bearing node with a
`UiActionBinding`). The SDK's JSX runtime is a **custom minimal reconciler** (a
few hundred lines: create-element, a hooks store keyed by tree position, and a
re-render loop) — **not** `react-reconciler`. Rationale: the component model we
need (pure render functions + `useState`/`useReducer`/`useContext`/`useMemo`) is
small, and pinning a production SDK to an API React itself documents as unstable
is the maintenance trap this design avoids. The SDK may later add a
`react-reconciler` adapter as an optional package if authors demand full React;
the wire contract does not change either way.

### 4.3 Event flow

Bidirectional, mapped onto the existing action-binding system:

```text
Keyboard/mouse event
        │
        ▼
Ratatui focus + hit-test (crates/tui/src/remote_ui — interaction metadata)
        │
        ▼
UiAction { binding, payload } → daemon → worker (existing framed protocol)
        │
        ▼
SDK dispatches to the component's action handler
        │
        ▼
Component state update → re-render → new UiDocument revision
        │
        ▼
Host validates → TUI repaints (pure projection)
```

The host-side division of responsibility is already implemented and this spec
re-states it as RULES for the SDK (what the SDK must **not** attempt):

```text
Rust owns (never the plugin):        Plugin supplies:
─────────────────────────────        ─────────────────────────────
Layout & terminal-width math         Content and structure
Focus traversal                      Semantic intent (roles, tones)
Keyboard shortcuts                   Action names + payloads
Mouse hit testing                    Capability requirements
Theme resolution                     Fallback content
Unicode cell-width handling
Clipping and scrolling
Permission prompts
Command dispatch
Accessibility metadata
```

**RULES**

1. The SDK MUST emit only protocol-known primitives plus namespaced custom
   primitives with `fallback` populated, exactly as
   `crates/protocol/src/remote_ui.rs` specifies. Unknown-primitive survival is a
   protocol guarantee, not an SDK escape hatch.
2. The SDK MUST NOT expose absolute coordinates, pixel/cell sizes, or focus
   ordering to components. Layout hints are limited to what the protocol already
   models (gap, direction, weight, constraints).
3. The SDK MUST validate the emitted document against the protocol schema and the
   host-advertised `UiHardLimits` **before** sending; an over-limit render is an
   SDK-side error surfaced to the plugin author, never a malformed frame.
4. Action handlers MUST be pure with respect to the UI: they return
   state updates and/or host commands; they never mutate the document directly.
5. Trust surfaces remain host-owned. The SDK exposes no API that can draw over,
   restyle, or intercept approval/enable/revoke flows.

## 5. Changes, file by file

### `sdk/remote-ui/` (new — TypeScript package `@codypendent/remote-ui`)

- `src/jsx-runtime.ts` — `jsx`/`jsxs`/`Fragment` for the automatic JSX transform;
  element records are `{ type: PrimitiveName | ComponentFn, props, children }`.
- `src/primitives.ts` — typed component wrappers for every protocol primitive
  (generated where possible; see §6 schema export). Prop types mirror the Rust
  serde shapes.
- `src/hooks.ts` — `useState`, `useReducer`, `useContext`, `useMemo`, plus
  terminal-specific `useCapabilities()` (the host's `UiCapabilitySelection`) and
  `useTheme()` (semantic token names only — never resolved colors).
- `src/render.ts` — the reconciler: render → element tree → `UiDocument` encode →
  schema + hard-limit validation → frame to stdout. Re-render scheduling batches
  state updates within a tick; document revisions are monotonic.
- `src/actions.ts` — `defineAction(name, handler)` registry; incoming
  `UiAction` frames dispatch here.
- `src/worker.ts` — the entry glue for `crates/ui-worker-launcher`'s framed
  stdio protocol (mirror `crates/ui-host/src/framing.rs` exactly: frame layout,
  length prefixes, and version handshake).
- `test/` — golden tests: TSX fixture components → expected `UiDocument` JSON.

### `crates/ui-host/src/registry.rs`

No protocol change. Add a manifest capability string (e.g. `sdk: "tsx@1"`) so the
host can report SDK provenance in the plugin detail rail — informational only.

### `docs/docs/05-skills-tools-and-plugins.md`

Add the authoring-SDK section and the boundary principle; cross-link this spec.

### Example plugin: `examples/remote-ui-tsx/`

A complete worked example (the `AgentCard` above plus a `PullRequestPanel` with a
`Stack`/`Heading`/`Button` composition) that builds with `npm build` and loads
through the normal signed-manifest install flow.

## 6. Protocol & persistence

**No wire-protocol changes.** The SDK targets `UiProtocolVersion::V1` as shipped.

One supporting deliverable (shared with Adoption 12/A6): export the remote-UI
schema from `crates/protocol` as JSON Schema so `src/primitives.ts` prop types are
**generated**, not hand-mirrored — the same schema-export seam the generated
client SDK work needs. Until A6 lands, hand-written types with a golden-JSON
compatibility test are acceptable.

## 7. Acceptance criteria

1. A TSX plugin using only the SDK (no hand-written JSON) renders in the TUI
   through the existing worker → host → renderer path, with focus, theming, and
   accessibility handled entirely host-side.
2. RUN `npm test` in `sdk/remote-ui` — EXPECT golden-document tests pass,
   including one fixture per protocol primitive category (layout, content,
   structured data, feedback, navigation, input).
3. An action round-trip works end to end: activating a `Button` in the TUI
   delivers the action to the component handler, whose state update produces a new
   revision that repaints — with no Rust code change beyond §5.
4. A document exceeding `UiHardLimits` fails **in the SDK** with an author-facing
   error; the host never receives the frame (assert via a host-side test that no
   oversized frame arrives).
5. A component emitting a namespaced custom primitive with `fallback` renders the
   fallback on a host that doesn't know the primitive (existing renderer
   behaviour, exercised from the SDK side).
6. The trust flow is unchanged: installing the example plugin still walks the
   signed-manifest + permission-diff + host-owned confirm path; no SDK API can
   reach those overlays (compile-time: no such exports exist; test: grep the SDK's
   public surface).
7. Existing non-TSX producers still work: the `crates/ui-host` test suite passes
   unmodified.

## 8. Tests

- `sdk/remote-ui/test/golden.test.ts` — fixture TSX → exact `UiDocument` JSON.
- `sdk/remote-ui/test/hooks.test.ts` — state updates batch into one revision;
  context propagates; memoization stable across re-renders.
- `sdk/remote-ui/test/limits.test.ts` — over-limit and unknown-primitive cases.
- `crates/ui-host` — one new integration test loading the example plugin's built
  worker and asserting document validity + action round-trip (follow the existing
  two-client convergence test idioms in the workspace).
- TUI reducer tests: none needed — the renderer path is unchanged.

## 9. Companion client (recorded direction, not in scope)

For genuinely graphical needs (MUI/shadcn, charts, document preview, drag-and-drop)
the sanctioned path is a **Tauri React client speaking the same daemon protocol** —
a sibling frontend, per the product boundary in `README.md` ("all frontends are
clients"):

```text
                       ┌───────────────────────┐
                       │   codypendentd core   │
                       └───────────┬───────────┘
                    typed commands / events / state
                   ┌───────────────┴───────────────┐
         ┌─────────▼──────────┐          ┌─────────▼─────────┐
         │ Ratatui TUI client │          │ Tauri React client │
         │ (this repo, today) │          │ (future, separate) │
         └────────────────────┘          └───────────────────┘
```

Because the Remote UI contract is renderer-independent, the same plugin
`UiDocument`s can be projected by a future React renderer in that client — one
plugin, two surfaces. That is the payoff for keeping the SDK on the protocol
rather than on ratatui.

## 10. Gotchas

1. **Do not reach for `react-reconciler` first.** It is documented by React as
   experimental with a less-stable API than React DOM. The minimal reconciler in
   §4.2 covers the required model; an adapter can come later without wire changes.
2. **Hooks keyed by tree position** (the minimal-reconciler approach) break if a
   component's children reorder without keys — the SDK must implement `key` props
   from day one, not as a follow-up.
3. **The worker sandbox has no DOM and must stay that way.** Authors will import
   browser-flavoured utility packages that touch `document`/`window` at module
   scope; the SDK's bundler config should fail such builds loudly rather than
   shipping a worker that crashes at load.
4. **Framing drift.** `src/worker.ts` mirrors `crates/ui-host/src/framing.rs` by
   hand until A6's schema export exists — pin it with a cross-language golden
   frame test or the first framing change will break every TSX plugin silently.
5. **Revision floods.** A `setState` inside an interval can emit revisions faster
   than the terminal repaints. The SDK must coalesce (one in-flight revision;
   latest-wins) — the host's hard limits are a backstop, not a rate limiter.
6. **Text is not cells.** Authors will assume `string.length` == columns. All
   width concerns stay host-side (`crates/tui/src/remote_ui/text.rs`); the SDK
   must not offer any width/truncation API, or authors will build layout on it.
7. **PTY embedding is a different feature.** Embedding Ink or other terminal apps
   is whole-tool embedding via Adoption 09's PTY machinery with all its focus/
   cursor/alt-screen caveats — never a Remote UI mechanism. Keep the two paths
   from growing into each other.

## 11. Out of scope

- Any change to the Remote UI wire protocol or the ratatui renderer.
- The Tauri companion client itself (§9 records the direction only).
- Ratzilla/WASM browser rendering of the TUI.
- A `react-reconciler`-based full-React adapter (possible later; not v1).
- Sandboxing changes — the existing worker launcher's confinement is assumed.
