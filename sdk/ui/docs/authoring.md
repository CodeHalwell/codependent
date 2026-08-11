# Remote UI package authoring

Remote UI packages are precompiled semantic component workers. They run out of process, exchange u32-big-endian length-framed JSON over stdio, and receive only mediated projections and action results. They do not receive a daemon client, filesystem handle, environment, socket, secret store, process launcher, or approval authority.

## Create a package

```sh
npm exec --package=@codypendent/ui -- create-codypendent-ui my-panel --template react
# or: --template pure
cd my-panel
npm install
npm run check
```

Both templates include a complete `ui-component` manifest, semantic TSX, deterministic golden tests, one shared cross-host worker entrypoint, requested host capabilities, and explicit resource ceilings. `[resources].wall_seconds = 3600` allows a long interactive session while the host still enforces heartbeats, message rate, memory, CPU, output, and process isolation. The generated package requires Node 22.13 or newer because the development inspector refuses to execute component code without stable permission controls.

A signed `native-process` package may also include `[ui]`. Its native runtime and UI worker are separate launch boundaries: UI code receives only the UI manifest's mediated grants and resource ceiling, never the native process's filesystem, network, secret, or subprocess authority. Standalone UI packages should continue to use `kind = "ui-component"` and omit `[runtime]` and top-level `[capabilities]`.

## Commands

```sh
codypendent-ui validate .
codypendent-ui build .
codypendent-ui test .
codypendent-ui workbench . --target vscode --point artifact-renderer --viewport 100x30 --theme highContrast
codypendent-ui dev . --target terminal --point panel --viewport 80x24 --color-depth ansi256
codypendent-ui inspect dist/worker.mjs --target web --point panel --fixture story.json
codypendent-ui inspect dist/worker.mjs --json  # framed protocol log
codypendent-ui schema --output remote-ui.schema.json
codypendent-ui validate-json fixture.json
codypendent-ui package . --key publisher-ed25519.pem
codypendent-ui sign . artifact.cody-ui.tgz --key publisher-ed25519.pem
```

`dev` and `workbench` are persistent watch flows. Every rebuilt bundle is started as an isolated candidate, negotiated, themed, supplied with inert projection/action fixtures, rendered, accessibility-audited, and inspected before it can replace the last-valid generation. A failed build, handshake, initial render, patch, or accessibility audit rolls back without discarding the previously committed report/state. Generated projects use `workbenchHotReloadState` and `useHotReloadState`; only JSON-safe values are copied into a candidate. Arbitrary module state is intentionally not transferred.

Select `terminal`, `vscode`, `web`, or `test` as the host target. The workbench reports the full semantic tree including props, requirements and fallbacks; contribution placement; ignored target-specific dimensions and spacing; unknown theme tokens; missing fallbacks; accessibility findings; patches; events; projection subscriptions; action invocations; and the complete host/worker protocol trace. `--fixture conformance` uses the SDK's shared loading/empty/error/long-content story. A JSON fixture can provide `projections`, `actions`, revision-bound `events`, and `hotReloadState` without executing fixture code.

```json
{
  "id": "artifact.loaded",
  "title": "Loaded artifact",
  "target": "vscode",
  "point": "artifact-renderer",
  "projections": {
    "artifact:artifact-42": { "revision": 3, "value": { "id": "artifact-42", "mediaType": "application/json", "revision": 3, "value": { "ok": true } } }
  },
  "actions": {
    "artifact.retry": { "status": "succeeded", "value": { "retried": true } }
  },
  "events": [
    { "documentId": "main", "targetId": "retry", "type": "press" }
  ],
  "hotReloadState": { "expanded": true }
}
```

The inspector starts Node with permission controls: package-read only; filesystem writes, network, child processes, worker threads, and native addons remain denied. It requires Node 22.13+. This is a development seat belt; installed components still run through Codypendent's OS sandbox and verified-package supervisor.

Packaging produces a deterministic sorted ustar+gzip artifact with stable ownership, modes, and timestamps. Only `dist`, `assets`, `package.json`, README, and license files enter it; source, manifests, VCS data, dependencies, symlinks, private keys, and prior artifacts are excluded. Per-file, file-count, and total-byte ceilings fail closed. The tool writes a `sha256:<hex>` artifact checksum and, when given a key, an Ed25519 signature over the exact domain-separated canonical manifest digest enforced by the Rust installer. The raw 32-byte public key is emitted as `publisher.ed25519.pub` for trusted-publisher registration.

Tooling mirrors the installer's Remote UI ceilings: 2,048 MiB declared memory, 300 CPU seconds, 3,600 wall seconds, 64 MiB output, 10,000 files, 20,000 total archive entries, 10,000 directories, 64 MiB per file, 256 MiB uncompressed, and 10 MiB compressed. A package outside those limits fails during validation/packaging rather than later during install.

## State and commands

React components mount inside `UiProvider`. `useSession`, `useRun`, `useArtifact`, `useTheme`, `useViewport`, and `useCapabilities` subscribe to latest-wins JSON projections. `useCommand` emits a revision-bound action intent and resolves only when the host returns its matching result. Abort signals send cancellation only for an invocation actually created by the bridge. Rendering a button or command descriptor grants no authority.

Every subscription and invocation is bounded and identified. Stale projection revisions are ignored; stale UI events are rejected; patch revisions advance exactly one; resync sends an authoritative snapshot. Hot reload acknowledges `worker.reloaded` and follows with snapshots so clients never retain a half-updated tree.

React callbacks remain inside the sandboxed worker. The renderer serializes only a reserved, sorted `props.eventHandlers` list, allowing the host to validate a revision-current event before forwarding it. Use `onPress`/`onChange` for local state and omit the corresponding `action`/`changeAction`; declaring both is rejected as ambiguous. `useCommand(...).execute()` is permitted only synchronously within a forwarded event context, so render/effect code cannot invoke a command on mount.

Manifest contributions are explicitly mapped to surfaces in the worker bootstrap:

```ts
await runStdioUiWorker({
  pluginId: "acme.trace",
  capabilityOffer,
  surfaces: [createReactUiSurface({ documentId: "main", render: () => <TracePanel /> })],
  contributions: [{
    id: "acme.trace.panel",
    point: "panel",
    renderer: "acme.TracePanel",
    documentId: "main",
  }],
});
```

After capability negotiation the runtime sends one producer-scoped atomic registration set. On surface removal or shutdown it replaces that set, using an empty set to unregister everything before disposing documents. IDs and points are still bound to the verified manifest by the trusted host; metadata is inert and grants no authority.

## Golden testing

Use `renderForTest`, `stableUiJson`, deterministic event IDs, `applyPatchBatch`, and the React renderer's message capture. `UI_CONFORMANCE_STORY` is the shared structural fixture consumed by the SDK and VS Code DOM suite across narrow/wide viewports, dark/light/high-contrast/monochrome themes, loading, empty, error, interaction, and long-content states. Keep semantic-tree goldens alongside host structural or screenshot tests; a DOM-only screenshot cannot prove the terminal fallback and a terminal snapshot cannot prove ARIA/focus behavior.

The checked-in JSON Schema is the canonical transport contract. `test/fixtures/ui-document.json` is parsed by both Rust and TypeScript tests, while `signing-plugin.toml` pins the cross-language signing digest. `DEFAULT_UI_LIMITS` derives its overlapping ceilings from `DEFAULT_UI_HARD_LIMITS`; authors should not copy numeric limits into fixtures or docs.
