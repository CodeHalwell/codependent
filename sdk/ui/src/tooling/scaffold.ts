import { mkdir, readdir, writeFile } from "node:fs/promises";
import { basename, join, resolve } from "node:path";

export type ScaffoldTemplate = "pure" | "react";

function packageName(name: string): string { return name.toLowerCase().replace(/[^a-z0-9._-]+/gu, "-").replace(/^-+|-+$/gu, "") || "codypendent-ui"; }
function rendererName(name: string): string { return name.split(/[^A-Za-z0-9]+/u).filter(Boolean).map((part) => `${part[0]?.toUpperCase() ?? ""}${part.slice(1)}`).join("") || "Component"; }

function manifest(id: string, renderer: string): string { return `schema_version = 1
id = ${JSON.stringify(id)}
name = ${JSON.stringify(renderer)}
version = "0.1.0"
kind = "ui-component"
publisher = "replace-with-publisher-id"
scopes = ["user", "organization", "repository"]

[ui]
schema_version = 1
requested_capabilities = ["artifact-read", "context-read", "run-read"]

[ui.compatibility]
protocol = ">=1.0,<2.0"
sdk = "^1.0"

[ui.entrypoints]
shared = "dist/worker.mjs"

[[ui.contributions]]
id = ${JSON.stringify(`${id}.panel`)}
point = "panel"
renderer = ${JSON.stringify(`${id}.${renderer}`)}
targets = ["shared"]

[resources]
memory_mb = 256
cpu_seconds = 300
wall_seconds = 3600
maximum_output_mb = 32

[security]
checksum = "sha256:${"0".repeat(64)}"
signature = "set-during-packaging"
sandbox_profile = "ui-component"

[update]
channel = "stable"
permission_change_requires_approval = true
`; }

function pureComponent(renderer: string): string { return `import { Badge, Button, KeyValue, Stack, Text, type UiNode } from "@codypendent/ui";

export interface ${renderer}Props { title?: string; count?: number; }

export function ${renderer}({ title = "${renderer}", count = 0 }: ${renderer}Props): UiNode {
  return (
    <Stack id="root" gap="sm" accessibleLabel={title} fallback={<Text value={title + " is unavailable"} />}>
      <Text id="heading" role="heading">{title}</Text>
      <Badge id="status" tone="positive" message={String(count) + " local updates"} accessibleLabel={String(count) + " local updates"} />
      <KeyValue id="details" entries={{ runtime: "sandboxed stdio" }} />
      <Button id="local" localEvents={["press"]} label="Local update" shortcut="r" />
    </Stack>
  );
}
`; }

function reactComponent(renderer: string): string { return `import { useState } from "react";
import { Text as semanticText } from "@codypendent/ui";
import { Badge, Button, Panel, Stack, Text, useViewport } from "@codypendent/ui/react";

export function ${renderer}() {
  const [count, setCount] = useState(0);
  const viewport = useViewport();
  return (
    <Panel.Root id="root" accessibleLabel="${renderer}" fallback={semanticText({ value: "${renderer} is unavailable" })}>
      <Panel.Header><Text id="heading" role="heading">${renderer}</Text></Panel.Header>
      <Panel.Body>
        <Stack gap="sm">
          <Badge id="status" tone="positive" message={String(count) + " local updates"} accessibleLabel={String(count) + " local updates"} />
          <Text>{"Viewport: " + viewport.width + " × " + viewport.height}</Text>
          <Button id="local" label="Local update" onPress={() => setCount((current) => current + 1)} />
        </Stack>
      </Panel.Body>
    </Panel.Root>
  );
}
`; }

function pureWorker(id: string, renderer: string): string { return `import { defaultWorkerCapabilities, createPureUiSurface, runStdioUiWorker } from "@codypendent/ui/worker";
import { ${renderer} } from "./component.js";

let count = 0;

await runStdioUiWorker({
  pluginId: ${JSON.stringify(id)},
  capabilityOffer: defaultWorkerCapabilities({
    capabilities: ["artifact-read", "context-read", "run-read"],
    contributionPoints: ["panel"],
  }),
  surfaces: [createPureUiSurface({
    documentId: "main",
    render: () => <${renderer} count={count} />,
    onEvent: (event) => {
      if (event.targetId !== "local" || event.type !== "press") return false;
      count += 1;
      return true;
    },
  })],
  contributions: [{ id: ${JSON.stringify(`${id}.panel`)}, point: "panel", renderer: ${JSON.stringify(`${id}.${renderer}`)}, documentId: "main" }],
});
`; }

function reactWorker(id: string, renderer: string): string { return `import { defaultWorkerCapabilities, runStdioUiWorker } from "@codypendent/ui/worker";
import { createReactUiSurface } from "@codypendent/ui/worker/react";
import { ${renderer} } from "./component.js";

await runStdioUiWorker({
  pluginId: ${JSON.stringify(id)},
  capabilityOffer: defaultWorkerCapabilities({
    capabilities: ["artifact-read", "context-read", "run-read"],
    contributionPoints: ["panel"],
  }),
  surfaces: [createReactUiSurface({ documentId: "main", strictMode: true, render: () => <${renderer} /> })],
  contributions: [{ id: ${JSON.stringify(`${id}.panel`)}, point: "panel", renderer: ${JSON.stringify(`${id}.${renderer}`)}, documentId: "main" }],
});
`; }

function packageJson(id: string, template: ScaffoldTemplate): string {
  return `${JSON.stringify({
    name: id, version: "0.1.0", private: true, type: "module",
    scripts: {
      build: "esbuild src/worker.tsx --bundle --platform=node --format=esm --target=node20 --outfile=dist/worker.mjs",
      typecheck: "tsc --noEmit", test: "vitest run", check: "npm run typecheck && npm test && npm run build",
      dev: "codypendent-ui dev", validate: "codypendent-ui validate", package: "codypendent-ui package",
    },
    dependencies: { "@codypendent/ui": "^1.0.0", ...(template === "react" ? { react: "19.0.0", "react-reconciler": "0.31.0" } : {}) },
    devDependencies: { "@types/node": "^22.13.4", ...(template === "react" ? { "@types/react": "19.0.12" } : {}), esbuild: "^0.25.0", typescript: "^5.7.3", vitest: "^4.1.10" },
    engines: { node: ">=20" },
  }, null, 2)}\n`;
}

function tsconfig(template: ScaffoldTemplate): string { return `${JSON.stringify({ compilerOptions: {
  target: "ES2022", lib: ["ES2022", "DOM"], module: "ESNext", moduleResolution: "Bundler", strict: true,
  noUncheckedIndexedAccess: true, exactOptionalPropertyTypes: true, jsx: "react-jsx",
  jsxImportSource: template === "pure" ? "@codypendent/ui" : "react", skipLibCheck: true,
}, include: ["src", "test"] }, null, 2)}\n`; }

function testSource(renderer: string, template: ScaffoldTemplate): string {
  return template === "pure" ? `import { describe, expect, it } from "vitest";
import { renderForTest } from "@codypendent/ui/testing";
import { ${renderer} } from "../src/component.js";

describe("${renderer}", () => {
  it("has deterministic accessible semantics", () => {
    const view = renderForTest(<${renderer} />, { documentId: "golden" });
    expect(view.find("heading")).toBeDefined();
    expect(view.toJSON()).toMatchSnapshot();
  });
});
` : `import { describe, expect, it } from "vitest";
import { createElement } from "react";
import { UiProvider, createReactUiRoot } from "@codypendent/ui/react";
import { MediatedUiBridge } from "@codypendent/ui/worker";
import { ${renderer} } from "../src/component.js";

describe("${renderer}", () => {
  it("emits a deterministic semantic snapshot", () => {
    const messages: unknown[] = [];
    const bridge = new MediatedUiBridge(async () => undefined);
    const root = createReactUiRoot({ documentId: "golden", onMessage: (message) => messages.push(message) });
    root.render(createElement(UiProvider, { state: bridge, actions: bridge, meta: bridge.meta, children: createElement(${renderer}) }));
    expect(messages[0]).toMatchSnapshot();
  });
});
`;
}

export async function createScaffold(directory: string, template: ScaffoldTemplate): Promise<string> {
  const target = resolve(directory);
  await mkdir(target, { recursive: true });
  if ((await readdir(target)).length > 0) throw new Error(`refusing to scaffold into non-empty directory: ${target}`);
  const id = packageName(basename(target));
  const renderer = rendererName(id);
  await mkdir(join(target, "src"), { recursive: true });
  await mkdir(join(target, "test"), { recursive: true });
  const files: Record<string, string> = {
    "package.json": packageJson(id, template), "tsconfig.json": tsconfig(template), "plugin.toml": manifest(id, renderer),
    ".gitignore": "node_modules/\ndist/\n*.cody-ui.tgz\n*.pem\n*.key\n",
    "src/component.tsx": template === "pure" ? pureComponent(renderer) : reactComponent(renderer),
    "src/worker.tsx": template === "pure" ? pureWorker(id, renderer) : reactWorker(id, renderer),
    "test/component.test.tsx": testSource(renderer, template),
    "README.md": `# ${renderer}\n\nSandboxed Codypendent semantic UI component.\n\n\`npm run check\` validates, tests, and bundles. \`npm run dev\` rebuilds and runs the protocol inspector. \`npm run package -- --key publisher.pem\` creates a deterministic signed artifact.\n`,
  };
  await Promise.all(Object.entries(files).map(([path, contents]) => writeFile(join(target, path), contents, { encoding: "utf8", flag: "wx" })));
  return target;
}
