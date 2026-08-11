# Remote UI debugging

`codypendent-ui dev` (also exposed as `workbench`) keeps a last-valid component report alive while it watches source fingerprints. Each rebuild is an untrusted candidate: it starts under Node permission controls, negotiates the selected target/point/viewport/theme/colour depth, receives inert fixtures, exercises `hotReload`/`worker.reloaded`, validates snapshots and patches, runs `auditAccessibility`, and commits only after clean disposal. Candidate failure prints the rollback reason and preserves the last committed JSON-safe hot-reload state. `inspect --json` prints every decoded inbound and outbound envelope; the only environment supplied to the worker is bounded workbench generation/state data.

For example:

```sh
codypendent-ui workbench . --target vscode --point workflow-inspector --viewport 72x32 --theme dark --fixture story.json
codypendent-ui inspect dist/worker.mjs --target terminal --point panel --viewport 80x24 --theme monochrome
```

The report identifies ignored dimensions/tokens instead of silently pretending every host applied them. It also shows requirements/fallback trees, actual contribution placement, subscription/action fixture traffic, patch revisions, accessibility issues and the ordered protocol trace. Use `--fixture conformance` to compare against the SDK/VS shared structural story.

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
- `point-not-mounted`: the worker declared a different contribution point than the selected workbench. Re-run with the reported `--point`; the tool does not fold every contribution into a generic panel.
- `ignored-dimension`, `ignored-spacing`, or `unknown-theme-token`: the selected host intentionally fell back to its native layout/theme default. Prefer portable semantic variants or inspect the concrete target that owns the specialization.
- candidate rolled back: the previous generation remains the reference output. Fix the first build, protocol, validation, initial-render, patch, or accessibility error; state will be offered again to the next candidate.

Validate a captured frame or document with `codypendent-ui validate-json capture.json --target vscode`; document validation automatically includes the accessibility and target-DX audit. Export the exact installed schema with `codypendent-ui schema`. For visual problems, inspect semantic IDs/props first, then compare terminal, VS Code, and web capability projections; renderers own layout, focus, clipping, Unicode width, and input routing. In VS Code each document has an independent host error boundary with Retry, confirmed Disable, and Report actions, so one extension surface should fail without replacing healthy siblings.
