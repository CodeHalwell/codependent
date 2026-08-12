# workflow-graph-card

A real, signed Remote UI plugin: a **workflow DAG card**. It subscribes to one
workflow run's `workflow` projection and renders the run as a `Graph` through
the first-party [`WorkflowGraphView`](../../../sdk/ui/src/first-party/workflow.tsx),
so the terminal host paints a layered DAG, the VS Code webview draws an SVG
one, and any host that can do neither still gets the node list fallback.

It exists to prove the whole author → package → sign → install → smoke → enable
path end to end in this repository, with real tooling and a real key.

- `src/component.tsx` — the surface. `useWorkflow(runId)` is the only state it
  touches; there is no filesystem, process, network, or daemon handle behind
  the boundary.
- `src/worker.tsx` — the sandboxed stdio worker, contributing to the
  `dashboard-card` and `workflow-inspector` slots.
- `plugin.toml` — the manifest. It requests exactly one capability,
  `workflow-read`, so an installer's permission diff is a single line. Its
  `security.checksum`/`security.signature` are real: written by
  `codypendent-ui package --key`, and re-verified by `test/package.test.ts`
  against the committed `publisher.ed25519.pub`.

Node 22.13+ is required — the development inspector refuses to run a worker
without Node's stable permission controls.

## Author loop

```sh
npm install                 # @codypendent/ui resolves to ../../../sdk/ui
npm run check               # typecheck + vitest + esbuild bundle → dist/worker.mjs
npm run dev                 # persistent workbench with transactional hot reload
```

`npm run dev -- --once` renders a single frame and exits, which is what CI
wants.

## Package and sign

The publisher key is a plain Ed25519 key. Generate a **test** one — never reuse
a real publisher key for an example:

```sh
openssl genpkey -algorithm ed25519 -out publisher.pem
npx codypendent-ui package . --key publisher.pem
```

`package` bundles, validates, writes a deterministic `ustar`+gzip
`workflow-graph-card.cody-ui.tgz`, rewrites `security.checksum` to the artifact
digest, signs the canonical manifest, and writes the raw 32-byte public key to
`publisher.ed25519.pub`. `publisher.pem`, `dist/`, and the `.tgz` are
gitignored; the signed manifest and the public key are committed.

To re-sign an existing artifact without rebuilding:

```sh
npx codypendent-ui sign . workflow-graph-card.cody-ui.tgz --key publisher.pem
```

## Install, smoke test, enable

The daemon refuses a signed plugin from an unknown publisher, so trust the test
publisher's key first (`publisher` in `plugin.toml` is `codypendent-example`):

```sh
codypendent plugin trust add codypendent-example "$(base64 -w0 publisher.ed25519.pub)"
codypendent plugin verify plugin.toml workflow-graph-card.cody-ui.tgz
codypendent plugin install plugin.toml workflow-graph-card.cody-ui.tgz
codypendent plugin smoke-test workflow-graph-card
codypendent plugin enable workflow-graph-card --scope user
codypendent plugin list
```

`install` lands the plugin **disabled** in the content-addressed store.
`smoke-test` boots the worker in the enforcing sandbox and requires every
declared contribution to render before `enable` is permitted; a worker that
cannot negotiate, or that renders nothing for a declared slot, never becomes
enabled.

Without a daemon, the SDK reproduces the smoke step locally — it boots the
bundled worker under Node's permission model and prints what it rendered:

```sh
npx codypendent-ui inspect dist/worker.mjs
```

## Binding a workflow run

The card reads `CODYPENDENT_WORKFLOW_RUN_ID` from the worker environment; a
host passes it when launching an instance for a slot. With no run bound (the
smoke test's situation) the card renders its empty state rather than failing,
which is what keeps the smoke test meaningful: it proves the surface mounts,
negotiates, and subscribes without needing a live workflow.
