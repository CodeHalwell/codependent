# Agent report: Remote-UI SDK / ui-host / VS Code

## Remote-UI capability verdict for DAG viewer + kanban

**Verdict: the protocol can carry both today; the renderers cannot draw a spatial DAG, and drag-drop doesn't exist. Kanban is achievable now by composition; DAG needs renderer work, not protocol work.**

**What the vocabulary has.** 49 built-in primitives + 10 domain cards in one canonical list mirrored Rust/TS (`crates/protocol/src/remote_ui.rs:118-181`, `sdk/ui/src/protocol.ts:104-121`): layout (Box/Stack/Row/Grid/Split/Spacer/ScrollArea/VirtualList/Portal), data (List/Table/Tree/KeyValue/Timeline/Graph/Chart/Sparkline), full input/action families, domain cards incl. WorkflowNode/TraceView/CostView. A typed **`Graph` primitive already exists** with nodes/edges/direction props (`sdk/ui/src/primitives.ts` GraphProps); `UiData`/`structured_data` carries arbitrary JSON without protocol bumps.

**Where it breaks down:**
- **TUI Graph is an adjacency-text list**, not a diagram (`crates/tui/src/remote_ui/paint.rs:1115-1147`). Worse, it looks for `edges`/`targets` *inside each node object*, while the SDK sends `edges` as a separate top-level array of `{from,to}` — **SDK-authored edges are silently dropped in the terminal** (paint.rs:1125-1129; codec only lifts `nodes`, codec.rs:220).
- **VS Code Graph is two `<ul>` lists** with no layout/SVG (renderer.tsx:414-416). Chart gets an SVG polyline; TUI Chart is unicode bars/sparklines.
- **No positional/geometry primitives**: `UiLayout` is flex+grid semantics only (remote_ui.rs:225-270); no x/y, no connector, no canvas. TUI layout engine does real flex + fr/percent grid with narrow collapse (`remote_ui/layout.rs:74-233`).
- **No drag-drop**: UI_EVENT_TYPES = action/press/change/submit/focus/blur/select/expand/collapse/navigate/scroll/key/custom (protocol.ts:214-219).

**Kanban gap analysis.** Columns = Row of Stack/Box or Grid; cards = Box/domain cards with Button/ActionMenu; selection via select events — all render today in both hosts with keyboard focus. **Missing:** (1) pointer drag-drop (S: add a drop event or emulate via ActionMenu); (2) **a `blackboard` projection kind — daemon serves only workflow|session|run|artifact|context|command** (`server.rs:2574-2704`; capability map `ui-host/src/runtime.rs:2474-2483`), even though the DB table, `ReadBlackboard` socket command, and a `blackboard-renderer` slot in all three hosts already exist (`daemon/src/remote_ui.rs:41`, `tui/src/remote_ui_host.rs:29`, `slot-registry.ts:30`); (3) **mediated write actions — the daemon's action allowlist is exactly `run.pause|run.resume|run.cancel`** (`server.rs:3195-3204`) so "move card" cannot execute daemon-side.

**Cheapest paths.** DAG: keep the Graph contract, upgrade both renderers (TUI: layered topo layout + box-drawing edges; web: SVG dagre-style) — no protocol change; fallback machinery handles old hosts. Kanban: composition + one projection kind + 3-5 allowlisted `blackboard.*` commands.

## First-party component inventory

All in `sdk/ui/src/first-party/` — **a React library over the SDK, with tests, mounted nowhere in the product** (no Core-trust producer ever launched; all daemon producers register as Extension trust, `daemon/src/remote_ui.rs:324,526`):

- **workflow.tsx — the DAG surface already exists**: `WorkflowGraphView` (WorkflowGraphNode{kind,status,agentId} + WorkflowGraphEdge{from,to,condition} → Graph primitive with List fallback + WorkflowNode card list with Inspect buttons, workflow.tsx:44-89), `WorkflowTimeline`, `WorkflowNodeInspector`. Data plumbing exists end-to-end (useWorkflow hook → workflow projection → live daemon subscribe). Missing: (a) a shipped worker that mounts it, (b) real graph paint.
- **management.tsx**: AgentManagement, SkillManagement/PluginManagement, IntegrationManagement. No board.
- **intelligence.tsx**: MemoryKnowledgeSearch (the vector top-k UI), ModelRoutingView, CostQuotaView.
- **observability.tsx**: TraceExplorer, LogsExplorer, MetricsDashboard.
- **foundation.tsx**: SurfaceFrame, IntentButton, StatusBadge, VirtualizedCollection, coreOnlyData.
- Also conversation, execution (RunProgress, ToolCallLifecycle, ApprovalReview), artifacts (7 viewers), git (4), system (5), shell (ApplicationShell, CommandPalette, NavigationRail).
- **No kanban/board component anywhere.** Only blackboard UI is the native ratatui overlay.

## End-to-end plugin path: works vs fixture-only

**Working, verified:** authoring (pure-TSX runtime, React 19 reconciler, snapshot/patch batches, worker state machine, mediated bridge); tooling (`codypendent-ui create|validate|build|test|dev|workbench|inspect|schema|package|sign`; transactional hot-reload dev workbench in permission-restricted Node; deterministic tgz + Ed25519 signing); install/trust (HMAC-sealed records, content-addressed artifacts, **smoke test boots the worker in the enforcing sandbox and requires every declared contribution to render before enabling** remote_ui_plugins.rs:312-371, permission-diff updates, publisher revocation cascade); runtime (seal re-check pre-spawn, rlimit launcher, watchdog, heartbeats, circuit breakers, admission quotas); broker (namespacing, broker-attested identity chrome, one-shot interaction tokens, replay dedupe, quarantine); TUI paint (all 49 primitives + domain cards natively; all 22 slots mounted with test); VS Code (real client: socket discovery, envelope framing, strict-CSP webview, DOM renderer for every primitive with error boundaries + recovery, state persistence + resync; 770-line client tests + 1243-line protocol-vector tests); projection mediation real for 6 kinds.

**Fixture-only / not shipped:** first-party surfaces (library + catalogue example only; core-only slots never populated); no example UI plugin in-repo (word-count is wasm-component without [ui]); production hot reload (host has UiWorker::hot_reload but daemon never sends it, remote_ui.rs:964); renderer subscription messages rejected (projections worker-only); VS Code mediated-wire has no in-repo consumer.

## Bugs & broken wiring (severity)

1. **HIGH (outcome 5): TUI drops SDK Graph edges** (paint.rs:1122-1129 reads per-node `edges`; SDK emits top-level `edges: [{from,to}]`) → terminal shows disconnected node labels. VS Code shows edges as raw text list.
2. **HIGH (outcomes 5/10): action allowlist is 3 commands** (`run.pause/resume/cancel`, server.rs:3195-3204). Every first-party intent (workflow.run, conversation.send, board mutations) un-executable through mediation.
3. **MEDIUM: worker/host rate-limit mismatch.** SDK worker default 240/s + **1000** burst (worker/runtime.ts:365-366); host kills at 240/s + **120** burst (ui-host/runtime.rs:797-798) → legitimate patch bursts trigger MessageRateExceeded + worker kill.
4. **MEDIUM: Grid `columns` contract drift.** VS Code reads integer count (renderer.tsx:294); TUI reads typed Vec<UiDimension>; SDK LayoutProps declares neither.
5. **LOW: Split ratio/direction ignored in TUI** (equal-width only, paint.rs:376-396); VS Code honors ratio.
6. **LOW: TUI measure() recursion per child per frame, no cache** (paint.rs:1804) — contradicts "cache measurement" claim in remote-ui.md:271.
7. **LOW: doc drift** — remote-ui.md:109 lists a `divider` primitive that doesn't exist; table omits Portal.

## Prioritized opportunities (S/M/L, impact)

1. **(S, huge) Fix TUI Graph edge sourcing + minimal layered rendering.** Accept top-level `edges`, topo-sort by direction, box-drawing connectors. Instantly makes WorkflowGraphView a real terminal DAG viewer.
2. **(S, huge) Extend the action allowlist table-driven** (remote_ui_command, server.rs:3171): add workflow.* and blackboard.* mapped to existing CommandBody variants. This single function is the bottleneck between "UI renders" and "UI does things."
3. **(S, high) Add `blackboard` projection kind**: one arm in run_remote_ui_projection + capability name + a useBlackboard hook. Everything else exists.
4. **(M, huge) Ship the first Core-trust producer.** Bundle a signed first-party worker (workflow inspector + blackboard board) launched like plugin workers but with RegistrationTrust::Core. Converts the whole first-party library from fixture to product.
5. **(M, high) SVG graph renderer in the VS Code webview**; reuse for code-graph-node slot.
6. **(M, medium) KanbanBoard first-party component** — Row of VirtualList columns + cards + ActionMenu move intents; keyboard-first; add a `drop` UiEventType later (string-open wire).
7. **(S, medium) Align rate/burst defaults; add columns to LayoutProps.**
8. **(L, medium) In-product hot reload**: daemon file-watch driving UiWorker::hot_reload (worker + protocol sides done).
9. **(S, medium) Ship a real example UI plugin** (.cody-ui.tgz + signed manifest) proving install→smoke→enable in-repo.

## Extra ideas

- The workbench (`codypendent-ui dev --fixture`) lets a DAG viewer be developed entirely offline against fixtures before daemon work lands.
- **Agent-authored boards**: UiData.items is open JSON; blackboard already stores typed items → agent writes via existing blackboard.* tools → daemon projection → same plugin surface for humans. No new storage.
- `code-graph-node` + `dashboard-card` slots already advertised in all hosts — ship the DAG viewer as a dashboard card first (smallest risk) before owning workflow-inspector.
- Broker's aggregate-viewport min-intersection means author responsive-first; TUI narrow collapse gives a free "list mode".
- Promote WorkflowNode's data payload into Graph items so the TUI domain-card fallback renders rich node cards when the graph can't be drawn — fidelity ladder: diagram → adjacency list → card list → text.
