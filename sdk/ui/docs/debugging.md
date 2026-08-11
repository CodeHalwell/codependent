# Remote UI debugging

`codypendent-ui dev` watches source fingerprints, rebuilds the deterministic bundle, starts it under Node permission controls, performs capability negotiation, exercises `hotReload`/`worker.reloaded`, prints the semantic tree, and shuts down cleanly. `inspect --json` prints every decoded inbound and outbound envelope; secrets and environment variables are never supplied to the worker.

Common failures:

- `expected capabilities`: stdout contains logging or the worker did not start with `runStdioUiWorker`. Write diagnostics to stderr; stdout is protocol-only.
- `ambiguous payload`: a known wire message contains more than its matching typed payload. Do not combine event/action/projection fields.
- `stale patch`: a patch base revision differs from the host document. Respond to `resync` with the current full snapshot.
- `unoffered capability`: the worker or host selected a primitive, contribution point, image protocol, command API, or resource ceiling absent from either offer. Update the manifest and expect a permission review.
- `removed projection cannot carry a value`: omit `value` (or use JSON null) when `removed` is true.
- `failed action requires a structured error`: include stable error code/message/recovery data; never serialize raw stack traces or secrets.
- `worker-local handler conflicts`: remove the matching `action`, `changeAction`, or `eventBindings` entry. A single gesture is either forwarded to local React state or translated to a host-mediated action, never ambiguously both.
- `contributionOwner`: every atomic contribution replacement is owned by `pluginId`; each `extensionId` must match it. Shutdown uses the same owner with an empty list.
- `heartbeat timeout` or rate failure: coalesce render state, virtualize large lists, avoid handler-only commits, and keep logs on bounded stderr.
- inspector permission error: bundle dependencies and assets under the package root. The inspector intentionally denies network, writes, child processes, worker threads, and native addons.

Validate a captured frame or document with `codypendent-ui validate-json capture.json`. Export the exact installed schema with `codypendent-ui schema`. For visual problems, inspect semantic IDs/props first, then compare terminal, VS Code, and web capability projections; renderers own layout, focus, clipping, Unicode width, and input routing.
