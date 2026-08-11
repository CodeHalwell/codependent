# Universal Remote UI architecture

## Status

This document is the normative architecture for Codypendent's component UI
platform. It covers first-party presentation, third-party UI contributions,
the native terminal renderer, React/TypeScript authoring, graphical clients,
security, compatibility, and developer tooling.

The platform is not an HTML bridge. Components describe semantic interface
trees. A trusted client host decides how those semantics are laid out, focused,
styled, validated, and rendered.

## Invariants

1. The daemon remains the authority for sessions, runs, workflows, artifacts,
   policy, permissions, plugins, and durable state.
2. Rust remains the authority for terminal lifecycle, layout, focus, keyboard
   navigation, mouse hit testing, secret handling, approvals, and intent
   dispatch.
3. Component code cannot perform host I/O. It consumes mediated projections and
   emits declared semantic events.
4. A UI contribution cannot grant a capability to itself or bypass the normal
   command, policy, approval, or sandbox paths.
5. Every interactive mouse path has a keyboard path.
6. Every graphical component has a useful terminal and plain-text fallback.
7. Unknown protocol fields, primitives, contributions, and events fail safely.
8. Component failure degrades one view; it never crashes the TUI or daemon.
9. Themes are semantic. Components never depend on literal terminal colours.
10. Headless operation never depends on a UI plugin being available.

## System boundaries

```text
TypeScript / TSX / React component process
    - component composition and local presentation state
    - mediated projection subscriptions
    - semantic action callbacks
                |
                | UiDocument / UiPatchBatch / UiEvent
                v
Daemon UI extension host
    - signed package lifecycle
    - sandbox and resource budgets
    - contribution registry
    - capability and action validation
                |
                | versioned Remote UI protocol
                v
Client UI host
    - state projection cache
    - layout, focus, input, forms and accessibility
    - theme and client-capability resolution
    - action routing
           /                    \
          v                      v
Ratatui terminal renderer   React DOM / VS Code / Tauri renderer
```

First-party components use the same contracts but run at the `core` trust tier.
They may compose the application shell and invoke internal semantic intents.
Installed UI packages run at the `extension` trust tier and can mount only at
declared public contribution points.

## Protocol model

Remote UI uses its own independently versioned protocol nested inside the
Codypendent daemon protocol.

The core messages are:

- `UiDocument`: a complete, revisioned semantic tree;
- `UiPatchBatch`: an atomic transition from one revision to the next;
- `UiEvent`: an interaction against a target in a specific revision;
- `UiContribution`: a renderer/view registration at a named host slot;
- `UiCapabilities`: primitives and facilities a client can provide;
- `UiLimits`: negotiated hard bounds for trees, text, patches and updates;
- `UiError`: a structured, recoverable failure with a safe fallback.

State and commands cross the worker boundary only through typed mediation:

- `UiProjectionSubscription` asks for an authorized `session`, `run`,
  `artifact`, `command`, `theme`, `viewport`, or capability projection;
- `UiProjectionUpdate` supplies a bounded latest-wins value and optional
  revision, never a database handle, path, socket, token, or callable object;
- `UiActionInvocation` is a revision-bound intent whose action identifier must
  be declared by the package and authorized again by the daemon;
- `UiActionResult` returns a typed success, failure, or cancellation result;
- host-originated cancellation settles one owned in-flight invocation; workers
  cannot cancel guessed host operations.

The wire discriminator is `type`. Every known message carries exactly one
matching typed payload, so a valid-looking message cannot smuggle a second
operation. Worker-to-host and host-to-worker message kinds are separately
allowlisted.

Documents and nodes have stable non-empty identifiers. Events against stale
revisions are rejected instead of being applied to a different control.

Patch batches are atomic and ordered. A client that misses a revision requests
a full document; it never guesses how to repair an incomplete tree.

## Semantic primitives

The shared component library covers these families:

| Family | Primitives |
| --- | --- |
| Layout | box, stack, row, grid, split, spacer, divider, scroll area, virtual list |
| Content | text, markdown, code, diff, image, audio, JSON tree, log viewer |
| Data | list, table, tree, key/value, timeline, graph, chart, sparkline |
| Feedback | badge, progress, spinner, alert, toast, empty state, error boundary |
| Navigation | tabs, breadcrumbs, menu, command list, pagination, link |
| Input | text input, text area, select, multi-select, checkbox, radio, form |
| Actions | button, action menu, toolbar, context menu |

Properties express meaning rather than CSS or terminal coordinates. Layout
sizes are constraints; the client performs definitive measurement and clipping.

Domain cards such as tool calls, approvals, patches, workflow nodes, tests,
traces and permission diffs are library components composed from these
primitives, not additional privileged wire nodes.

## Contribution points

The public registry supports the same signed set in every host: `sidebar`,
`panel`, `status-item`, `command`, `command-palette`, `composer-accessory`,
`message-renderer`, `tool-renderer`, `artifact-renderer`,
`workflow-inspector`, `blackboard-renderer`, `document-block`,
`code-graph-node`, `settings-section`, `setup-step`, `form`, `wizard`,
`dashboard-card`, `trace-span-renderer`, `context-menu`, `quick-pick`, and
`notification`. Terminal and graphical hosts advertise a point only when its
host-owned adapter is installed; a signed contribution is never silently
degraded into a generic panel.

The following are core-only: terminal lifecycle, authoritative approval chrome,
secret entry, policy state, global focus traps, emergency detach/cancel controls,
and the surface that reports a plugin's own trust or permissions.

## React and TypeScript runtime

`@codypendent/ui` provides a JSX runtime whose elements produce serializable
semantic nodes. `@codypendent/ui/react` provides a pinned custom React renderer
supporting function components, hooks, context, reducers, memoisation, error
boundaries, keyed reconciliation and batched updates. Named contribution slots
are registered through the SDK contribution registry, keeping React portals and
host ownership separate.

The reconciler adapter is isolated behind Codypendent-owned interfaces and a
conformance suite. Packages declare compatible UI protocol and SDK ranges; they
do not depend on the host's internal reconciler version.

Component processes receive only mediated hooks, including session/run/event
projections, artifact handles, theme, viewport and declared commands. There is
no raw filesystem, socket, database, environment, secret-store, or daemon API.

### Sandboxed worker lifecycle

The daemon launches only an installed, checksum-verified UI entrypoint. A
standalone package uses `kind = "ui-component"`; a signed native package may
carry a separately sandboxed `[ui]` worker, but that worker inherits none of
the native process's filesystem, network, secret, or subprocess authority. The
canonical package root and entrypoint are resolved before launch; symlink or
traversal escapes are rejected. Precompiled JavaScript runs in a dedicated Node
process lowered through the platform sandbox with a clean environment, the
package root as its working directory and only the package plus host-owned,
sealed bundled-Node dependency roots as implicit read grants, no brokered
secrets, no ambient network and no undeclared subprocess authority. JavaScript
is never evaluated in the daemon process.

Worker stdin and stdout are reserved for big-endian-u32-length-prefixed JSON
`UiWireMessage` frames. The host validates the absolute frame cap and a
per-worker byte token bucket from the four-byte header before allocation or
parsing, then validates every message against negotiated semantic limits. The
manifest output budget is a lifetime aggregate across framed stdout payloads
and stderr, not merely an in-memory diagnostic limit. Stderr additionally has
its own sustained-rate/burst bucket; exceeding either budget kills the process
group. Retained diagnostics remain independently bounded, control-stripped,
origin-labelled, and redacted.

Startup is a bounded capabilities offer, intersection selection and explicit
readiness handshake. A ready worker is subject to total-message, message-rate,
wall-clock and heartbeat limits. Graceful disposal has a short acknowledgement
deadline followed by deterministic process-group termination and reaping.
Unexpected exits, heartbeat loss and protocol violations feed exponential
restart backoff and a per-plugin rolling-window circuit breaker. Resync requests
recover rejected/missing revisions from full snapshots; development hot reload
is generation-numbered and names the changed compiled modules.

## Client host responsibilities

The client host owns:

- tree and patch validation;
- terminal-width and Unicode cell measurement;
- responsive flex/grid constraint resolution;
- clipping, wrapping, scrolling and virtualisation;
- focus traversal and focus restoration;
- keyboard shortcuts and mouse hit testing;
- local form buffers and secret redaction;
- theme-token resolution and colour-depth fallback;
- accessibility labels and textual representation;
- event revisioning, debouncing and dispatch;
- component error isolation and fallback rendering.

Terminal input state remains local so typing does not require a round trip per
keystroke. Components receive controlled changes at a bounded/debounced rate and
semantic submit/change events according to the control contract.

Password, token, credential, private-key, and other secret-like inputs are
invalid remote documents. Secret entry is trusted host chrome and returns only
an opaque handle or decision; plaintext never enters a component tree, patch,
event, worker, or persisted renderer snapshot. A submit event includes only
fields in its Form subtree. Ordinary clicks and changes never sweep unrelated
form buffers.

## Graphical targets

Packages may publish `shared`, `terminal`, and `web` entrypoints.

- `shared` uses Codypendent primitives and renders everywhere.
- `terminal` may specialise for terminal client capabilities.
- `web` may use DOM React, CSS, SVG, canvas, Monaco, drag-and-drop, and rich media
  inside a CSP-restricted graphical extension surface.

A web-only contribution must name the renderer of a second, signed
terminal/shared contribution at the same point. The fallback is a real semantic
surface with its own verified target and document—not a host-generated label or
an attempt to execute DOM code in the native TUI.

## Artifact rendering

Artifact renderers are resolved by a deterministic registry using:

1. exact renderer identifier requested by trusted first-party state;
2. schema identifier and version;
3. media type;
4. tool/provenance match;
5. generic structured or text fallback.

Renderers receive bounded artifact projections or brokered page/range handles,
never unrestricted paths. Large logs, tables, images and traces are paged or
streamed under client limits.

## Security and governance

UI metadata, entrypoints, contributions and capability requests are part of the
signed plugin manifest. Any privilege-relevant expansion is shown in the update
permission diff and requires approval.

The host enforces:

- safe package-relative entrypoints with no traversal;
- declared public contribution points only;
- declared event and command identifiers only;
- action schema and capability checks at invocation time;
- process filesystem/network/secret/subprocess grants;
- CPU, memory, wall-time and output budgets;
- maximum tree depth, node count, text, patch batch and update rate;
- control-sequence and untrusted-content sanitisation;
- CSP/iframe isolation for graphical components;
- crash loops, timeout and protocol-violation quarantine.

Extensions may add an approval explanation but cannot replace or obscure the
host's action digest, risk, affected resources, decision controls or scope.
Every extension mount also carries immutable broker-attested producer chrome;
document props cannot claim core identity or hide the extension boundary.

## Performance and recovery

Clients coalesce compatible patch batches, virtualise large collections, cache
measurement, and skip unchanged subtrees. Plugin updates are frame-budgeted and
may be throttled independently from first-party state.

On a component crash or deadline:

1. retain the last valid document when safe;
2. show a host-rendered error boundary;
3. expose restart/disable/report actions;
4. restart only within a bounded crash policy;
5. quarantine repeated protocol or resource violations.

Hot reload preserves keyed component and host form state when the new document
is compatible. A protocol, package, or contribution change requiring new
permissions uses the normal controlled restart and approval path.

## Accessibility

Every focusable node has an accessible name and deterministic order. State is
never communicated only by colour. The host provides monochrome, high contrast,
reduced-motion, Unicode-safe and narrow-terminal representations. Graphical
renderers map the same semantics to appropriate ARIA roles. A screen-reader or
headless client can request a structured textual representation of any view.

## Developer tooling

The SDK and CLI provide scaffolding, development, component stories, fixtures,
interaction tests, packaging, signing and publishing. The persistent workbench
preflights each permission-restricted rebuild transactionally and preserves only
explicit JSON-safe state. It supports selectable host target, contribution
point, viewport, theme and colour depth; inert projection/action fixtures;
automatic accessibility, fallback, token and target-layout diagnostics; full
props/requirement/fallback trees; and event, patch, action, subscription and
ordered protocol traces. Graphical hosts consume the same structural story for
DOM/screenshot conformance while terminal hosts retain semantic goldens.

VS Code's advertised point list is derived from its concrete slot registry.
Every public point has a dedicated region/overlay adapter, deterministic order,
focus lifecycle and semantic label/role. Documents are independently isolated;
host-rendered Retry, confirmed Disable and Report controls recover one failed
surface without replacing healthy siblings.

Protocol types are generated from one canonical contract and checked with
cross-language golden vectors. Test coverage includes deterministic render
snapshots, patch/property tests, stale-event rejection, malicious tree limits,
capability fallbacks, terminal-size matrices, theme matrices, sandbox denials,
crash recovery and compatibility fixtures.

## First-party adoption

The component platform is the normal presentation architecture, not a parallel
plugin-only layer. The existing reducer and intent model remains authoritative,
while first-party components progressively own the shell, conversation entries,
artifact views, approvals, workflow/blackboard views, Docs Studio, Skills and
Plugin Studios, code intelligence, memory/context, model routing, Git/GitHub,
observability/evaluation, setup and multimodal presentation.

Rust fallbacks remain available for boot, fatal errors, approval authority,
plugin quarantine, and operation when the component runtime is unavailable.
