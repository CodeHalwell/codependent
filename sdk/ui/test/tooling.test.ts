import { mkdtemp, mkdir, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { artifactChecksum, createDeterministicArchive } from "../src/tooling/archive.js";
import { parseCanonicalManifest, rustSigningDigest, validateUiManifest } from "../src/tooling/manifest.js";
import { createScaffold } from "../src/tooling/scaffold.js";
import { inspectorNodeArguments, validateProject } from "../src/tooling/cli.js";

const signingFixture = new URL("./fixtures/signing-plugin.toml", import.meta.url);

describe("package-author tooling", () => {
  it("scaffolds complete pure and React packages with one-hour worker lifetime", async () => {
    for (const template of ["pure", "react"] as const) {
      const parent = await mkdtemp(join(tmpdir(), `codypendent-${template}-`));
      const target = join(parent, "example-ui");
      await createScaffold(target, template);
      const source = await readFile(join(target, "plugin.toml"), "utf8");
      const manifest = validateUiManifest(source);
      expect((manifest.resources as { wall_seconds: number }).wall_seconds).toBe(3600);
      expect(((manifest.ui as { entrypoints: { shared: string } }).entrypoints.shared)).toBe("dist/worker.mjs");
      expect(await readFile(join(target, "src/worker.tsx"), "utf8")).toContain("runStdioUiWorker");
      await expect(validateProject(target)).rejects.toThrow("entrypoints are missing");
    }
  });

  it("matches the Rust canonical signing digest", async () => {
    const source = await readFile(signingFixture, "utf8");
    const digest = Buffer.from(rustSigningDigest(parseCanonicalManifest(source))).toString("hex");
    expect(digest).toBe("87571e872de61f5317cae66468aadb16b993b92b31a273c0704cd27ab6167717");
  });

  it("rejects contribution target lists that combine shared and concrete workers", () => {
    const source = `schema_version = 1
id = "acme.mixed"
name = "Mixed"
version = "1.0.0"
kind = "ui-component"
publisher = "acme"
scopes = ["user"]
[ui]
schema_version = 1
requested_capabilities = []
[ui.compatibility]
protocol = ">=1.0,<2.0"
sdk = "^1.0"
[ui.entrypoints]
shared = "dist/worker.mjs"
[[ui.contributions]]
id = "acme.mixed.panel"
point = "panel"
renderer = "acme.Mixed"
targets = ["shared", "terminal"]
[resources]
memory_mb = 128
cpu_seconds = 60
wall_seconds = 300
maximum_output_mb = 8
[security]
checksum = "sha256:${"0".repeat(64)}"
signature = "replace"
sandbox_profile = "ui-component"
[update]
channel = "stable"
permission_change_requires_approval = true
`;
    expect(() => validateUiManifest(source)).toThrow("cannot combine shared");
  });

  it("creates byte-identical archives and excludes source, manifests, and keys", async () => {
    const root = await mkdtemp(join(tmpdir(), "codypendent-archive-"));
    await mkdir(join(root, "dist")); await mkdir(join(root, "src"));
    await writeFile(join(root, "dist/worker.mjs"), "export {};\n");
    await writeFile(join(root, "src/secret.ts"), "not packaged\n");
    await writeFile(join(root, "publisher.pem"), "secret\n");
    await writeFile(join(root, "plugin.toml"), "manifest\n");
    await writeFile(join(root, "package.json"), "{}\n");
    const first = await createDeterministicArchive(root);
    const second = await createDeterministicArchive(root);
    expect(first).toEqual(second);
    expect(artifactChecksum(first)).toMatch(/^sha256:[a-f0-9]{64}$/u);
  });

  it("never inspects a bundle with unrestricted Node authority", () => {
    const args = inspectorNodeArguments("/tmp/example/dist/worker.mjs");
    expect(args).toContain("--no-addons");
    expect(args).toContain("--permission");
    expect(args).toContain("--allow-fs-read=/tmp/example");
    expect(args.some((argument) => argument.startsWith("--allow-fs-write") || argument.startsWith("--allow-net") || argument.startsWith("--allow-child-process") || argument.startsWith("--allow-worker"))).toBe(false);
  });
});
