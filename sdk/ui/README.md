# @codypendent/ui

The governed, renderer-independent UI SDK for Codypendent. It lets first-party features and plugins author terminal, VS Code, desktop, and web surfaces with semantic TypeScript components while Rust remains responsible for authority, layout, focus, terminal correctness, and command validation.

The package contains two deliberately separate runtimes:

- `@codypendent/ui` is a zero-React pure TSX runtime. Components immediately produce serializable `UiNode` trees.
- `@codypendent/ui/react` is an isolated React 19 custom renderer. It supports hooks, context, reconciliation, keyed updates, error boundaries, Suspense-compatible host behavior, and emits the same protocol.
- `@codypendent/ui/worker` is the production framed-stdio bootstrap, mediated projection/action bridge, lifecycle state machine, and pure TSX surface adapter. `/worker/react` adds the React surface without making React mandatory for pure workers.

Full package-author workflows are documented in [authoring](docs/authoring.md), [browser React migration](docs/migrating-browser-react.md), and [debugging](docs/debugging.md). The `codypendent-ui`/`create-codypendent-ui` binaries provide scaffolding, validation, build/test, watched hot reload, semantic-tree/protocol inspection, schema export, and deterministic checksum/sign/package commands.

Importing the main package never loads React or `react-reconciler`. Plugins can therefore use the main entry from their `shared` and `terminal` modules, and reserve `/react` for a `web` entry or components that need React lifecycle.

## Install and verify

```bash
npm install
npm run check
```

React integration is intentionally pinned to the compatible pair `react >=19.0.0 <19.1.0` and `react-reconciler >=0.31.0 <0.32.0`. They are optional peers for consumers that only use pure TSX.

## Pure TSX

Configure TypeScript:

```json
{
  "compilerOptions": {
    "jsx": "react-jsx",
    "jsxImportSource": "@codypendent/ui"
  }
}
```

Then author semantic components without a browser or React:

```tsx
import { Badge, Button, Stack, Table, Text, createDocument } from "@codypendent/ui";

interface TestReportData {
  passed: number;
  failed: number;
  rows: Array<{ test: string; duration: number; result: string }>;
}

function TestReport({ report }: { report: TestReportData }) {
  return (
    <Stack gap="sm" accessibleLabel="Test results">
      <Text role="heading">Tests</Text>
      <Badge tone={report.failed ? "critical" : "positive"} status={`${report.failed} failed`} />
      <Table columns={["test", "duration", "result"]} rows={report.rows} />
      <Button action="tests.openFailures" label="Open failures" shortcut="o" />
    </Stack>
  );
}

const document = createDocument(<TestReport report={data} />, {
  documentId: "tool-result:test-42",
});
```

`Stack`, `Row`, `Box`, `Panel`-style layouts and every domain primitive are semantic: they do not contain CSS, ANSI escapes, filesystem handles, callbacks, or sockets. A host chooses the appropriate visual representation.

## Protocol

The public contract includes:

- `UiNode` and `UiDocument` snapshots;
- atomic, revision-checked `UiPatchBatch` updates;
- revision-bound `UiEvent` input;
- rich `UiCapabilities`, including the daemon's eight canonical capability flags;
- runtime/host messages for snapshots, patches, resync, errors, disposal, viewport changes, and hot reload;
- namespaced extension primitives with safe fallback nodes.

Patch operations cover root replacement, keyed insert/remove/replace/move, prop updates, and text updates. `createDocument` materializes deterministic IDs for anonymous pure nodes. `diffDocuments` uses `Map`/`Set` indexes for stable keyed reconciliation. Hosts must reject events and patch batches whose revision is stale.

The cross-language fixture at `test/fixtures/ui-document.json` is the canonical JSON example for Rust/client conformance tests.

## Semantic primitives

The typed library spans the full platform:

- Layout: `Box`, `Stack`, `Row`, `Grid`, `Split`, `Spacer`, `ScrollArea`, `VirtualList`
- Content: `Text`, `Markdown`, `Code`, `Diff`, `Image`, `Audio`, `JsonTree`, `LogViewer`
- Data: `List`, `Table`, `Tree`, `KeyValue`, `Timeline`, `Graph`, `Chart`, `Sparkline`
- Feedback: `Badge`, `Progress`, `Spinner`, `Alert`, `Toast`, `EmptyState`, `ErrorBoundary`
- Navigation: `Tabs`, `Breadcrumb`, `Menu`, `CommandList`, `Pagination`, `Link`, `Details`
- Input/actions: `TextInput`, `TextArea`, `Select`, `MultiSelect`, `Checkbox`, `Radio`, `Form`, `Button`, `ActionMenu`, `Toolbar`, `ContextMenu`
- Domain: tool, artifact, approval, agent, workflow, patch, test, permission, trace, and cost cards

Props favor explicit variants such as `tone`, `role`, `mode`, and `direction`. Layout composition happens through `children`, not renderer callbacks.

## React renderer

React primitives are imported from the isolated adapter:

```tsx
import { useState } from "react";
import { Button, Panel, Text, createReactUiRoot } from "@codypendent/ui/react";

function Counter() {
  const [count, setCount] = useState(0);
  return (
    <Panel.Root id="counter" accessibleLabel="Counter">
      <Panel.Header><Text role="heading">Count: {count}</Text></Panel.Header>
      <Panel.Body>
        <Button
          id="increment"
          label="Increment"
          onPress={() => setCount((current) => current + 1)}
        />
      </Panel.Body>
    </Panel.Root>
  );
}

const root = createReactUiRoot({
  documentId: "panel:counter",
  onMessage(message) {
    // Send only this validated protocol message to the trusted host.
    transport.send(message);
  },
});

root.render(<Counter />);
```

The first commit emits a snapshot; subsequent commits emit minimal patches. React keys preserve host node IDs through reorder. Handler functions never cross the process boundary: the renderer reserves a sorted `props.eventHandlers` array so the host can validate and route only declared events. `dispatch` only invokes a registered handler when protocol major, document ID, revision, target, and event type match. A worker-local handler cannot overlap a host-mediated `action`/`eventBindings` declaration on the same event.

## Mediated state and hooks

`UiProvider` is the only component that knows the host's `UiProjectionStore`. Its contracts are split into state, actions, and metadata so independent consumers do not subscribe to unrelated state:

```tsx
<UiProvider state={projectionStore} actions={commandActions} meta={providerMeta}>
  <RunInspector runId={runId} />
</UiProvider>
```

Available hooks include `useSession`, `useRun`, `useArtifact`, `useCommand`, `useTheme`, `useViewport`, `useCapabilities`, and `useUiMeta`. The bridge exposes projections and declared command invocation only—never raw filesystem, process, network, database, or secret access. `useTransientViewport` uses a ref for high-frequency reads that should not trigger rendering.

`HotReloadStateStore` and `useHotReloadState` preserve JSON-safe local state across module replacement. The `HotReloadMessage` prepare/state/apply/rollback protocol supports coordinated reload and rollback.

## Contributions

Contributions are registered by both contribution point and renderer discriminator:

```ts
const registration = registerContribution({
  id: "acme.trace-view",
  point: "artifact-renderer",
  renderer: "application/vnd.acme.trace+json",
  target: "web",
  render: ({ data }) => renderInteractiveTrace(data),
  terminalFallback: ({ data }) => renderTraceTable(data),
});

registration.dispose();
```

Supported points use the manifest's canonical kebab-case names and cover sidebars, panels, status, commands, composer accessories, message/tool/artifact renderers, workflow/blackboard/docs/code graph inspectors, settings/setup/forms/wizards, dashboards, trace spans, menus, quick picks, and notifications. `UI_CONTRIBUTION_POINTS` and `UI_HOST_CAPABILITIES` are the audited public offer lists.

A web-only registration is a discriminated union that cannot type-check without `terminalFallback`. `ContributionRegistry` and inline fallback functions are an in-process composition convenience only. In a packaged worker manifest, `fallback_renderer` must name the `renderer` of another declared contribution at the same point whose target is `terminal` or `shared`; the host launches that verified surface and never synthesizes a placeholder. Resolution is deterministic by priority then ID.

## Capabilities and accessibility

Use `projectDocument` to resolve `requires`, `TerminalOnly`, `WebOnly`, unsupported custom primitives, media, and diff capabilities. Unsupported nodes use their explicit fallback or deterministic accessible text. `negotiateCapabilities` computes the common supported protocol and render surface.

Accessibility support includes:

- `accessibleLabel` and `keyboardAction` helpers;
- `auditAccessibility` for missing labels, alt text/transcripts, mouse-only controls, color-only meaning, and duplicate focus order;
- `toAccessibleText` for screen-reader/log projections;
- keyboard, screen-reader, reduced-motion, monochrome, Unicode, color-depth, and terminal-graphics capability signals.

## Validation and testing

`validateDocument`, `validatePatchBatch`, and `assertValidDocument` enforce bounded depth, node count, children, text, props, patch count, document bytes, IDs, unique IDs, finite acyclic JSON, and monotonic revisions.

The deterministic test renderer is available through `@codypendent/ui/testing`:

```ts
import { renderForTest } from "@codypendent/ui/testing";

const view = renderForTest(component, { documentId: "test" });
expect(view.find("approve")).toBeDefined();
expect(view.dispatch("approve", "action").revision).toBe(0);
expect(view.toJSON()).toMatchSnapshot();
```

It also applies patch batches with stale-revision checks, records deterministic event IDs, and exposes stable canonical JSON for snapshots.

## Trust boundary

This SDK describes presentation. It does not grant authority. The trusted host must still:

- validate every snapshot, patch, event, action, and declared capability;
- enforce plugin CPU, memory, update-rate, and process isolation limits;
- own approvals, permission decisions, policy state, secret entry, focus, hit testing, and global navigation;
- sanitize text/control sequences and reject undeclared commands;
- replace crashed or unsupported components with safe host-owned error/text cards.

In particular, rendering a button does not authorize its `action`; it only requests a revision-bound command from the host.
