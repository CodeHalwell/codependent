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

Both templates include a complete `ui-component` manifest, semantic TSX, deterministic golden tests, a bundled worker entrypoint, terminal/web targets, fallbacks, requested host capabilities, and explicit resource ceilings. `[resources].wall_seconds = 3600` allows a long interactive session while the host still enforces heartbeats, message rate, memory, CPU, output, and process isolation.

## Commands

```sh
codypendent-ui validate .
codypendent-ui build .
codypendent-ui test .
codypendent-ui dev .               # poll/watch, rebuild, hot-reload protocol check, tree inspector
codypendent-ui inspect dist/worker.mjs
codypendent-ui inspect dist/worker.mjs --json  # framed protocol log
codypendent-ui schema --output remote-ui.schema.json
codypendent-ui validate-json fixture.json
codypendent-ui package . --key publisher-ed25519.pem
codypendent-ui sign . artifact.cody-ui.tgz --key publisher-ed25519.pem
```

The inspector starts Node with permission controls: package-read only; filesystem writes, network, child processes, worker threads, and native addons remain denied. It refuses Node versions without permission controls. This is a development seat belt; installed components still run through Codypendent's OS sandbox and verified-package supervisor.

Packaging produces a deterministic sorted ustar+gzip artifact with stable ownership, modes, and timestamps. Only `dist`, `assets`, `package.json`, README, and license files enter it; source, manifests, VCS data, dependencies, symlinks, private keys, and prior artifacts are excluded. Per-file, file-count, and total-byte ceilings fail closed. The tool writes a `sha256:<hex>` artifact checksum and, when given a key, an Ed25519 signature over the exact domain-separated canonical manifest digest enforced by the Rust installer. The raw 32-byte public key is emitted as `publisher.ed25519.pub` for trusted-publisher registration.

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

Use `renderForTest`, `stableUiJson`, deterministic event IDs, `applyPatchBatch`, and the React renderer's message capture. The checked-in JSON Schema is the canonical transport contract. `test/fixtures/ui-document.json` is parsed by both Rust and TypeScript tests, while `signing-plugin.toml` pins the cross-language signing digest.
