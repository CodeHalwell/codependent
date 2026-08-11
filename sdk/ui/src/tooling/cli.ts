#!/usr/bin/env node
import { createHash, createPrivateKey, createPublicKey, sign as edSign } from "node:crypto";
import { spawn } from "node:child_process";
import { copyFile, mkdir, readFile, readdir, rename, stat, writeFile } from "node:fs/promises";
import { realpathSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { DEFAULT_UI_HARD_LIMITS, MINIMAL_TERMINAL_CAPABILITIES, UI_CONTRIBUTION_POINTS, UI_HOST_CAPABILITIES, type UiCapabilities, type UiCapabilitySelection, type UiWireMessage } from "../protocol.js";
import { REMOTE_UI_SCHEMA_URL } from "../schema.js";
import { validateDocument, validatePatchBatch } from "../validation.js";
import { negotiateCapabilities } from "../capabilities.js";
import { decodeUiFrames, UiFrameWriter } from "../worker/framing.js";
import { assertUiWireMessage } from "../worker/wire.js";
import { artifactChecksum, createDeterministicArchive } from "./archive.js";
import { parseCanonicalManifest, rustSigningDigest, updateSecurityFields, validateUiManifest } from "./manifest.js";
import { createScaffold, type ScaffoldTemplate } from "./scaffold.js";

function option(args: string[], name: string): string | undefined {
  const index = args.indexOf(name);
  return index < 0 ? undefined : args[index + 1];
}
function has(args: string[], name: string): boolean { return args.includes(name); }
function positional(args: string[]): string[] {
  const result: string[] = [];
  for (let index = 0; index < args.length; index += 1) {
    if (args[index]?.startsWith("--")) { if (!["--json", "--once", "--skip-build"].includes(args[index] as string)) index += 1; }
    else if (args[index] !== undefined) result.push(args[index] as string);
  }
  return result;
}

async function atomicWrite(path: string, contents: Uint8Array | string): Promise<void> {
  const temporary = `${path}.${process.pid}.tmp`;
  await writeFile(temporary, contents, { mode: 0o600 });
  await rename(temporary, path);
}

async function runCommand(command: string, args: string[], cwd: string): Promise<void> {
  await new Promise<void>((resolvePromise, reject) => {
    const child = spawn(command, args, { cwd, stdio: "inherit", env: process.env });
    child.once("error", reject);
    child.once("exit", (code, signal) => code === 0 ? resolvePromise() : reject(new Error(`${command} exited ${code ?? signal ?? "unknown"}`)));
  });
}

async function signArtifact(root: string, artifactPath: string, keyPath?: string): Promise<void> {
  const manifestPath = join(root, "plugin.toml");
  const artifact = await readFile(artifactPath);
  let source = await readFile(manifestPath, "utf8");
  source = updateSecurityFields(source, artifactChecksum(artifact), "set-during-packaging");
  if (keyPath !== undefined) {
    const privateKey = createPrivateKey(await readFile(resolve(keyPath)));
    if (privateKey.asymmetricKeyType !== "ed25519") throw new Error("publisher key must be an Ed25519 private key in PEM/DER form");
    const signature = edSign(null, rustSigningDigest(parseCanonicalManifest(source)), privateKey).toString("base64");
    source = updateSecurityFields(source, artifactChecksum(artifact), signature);
    const publicDer = createPublicKey(privateKey).export({ format: "der", type: "spki" });
    await atomicWrite(join(root, "publisher.ed25519.pub"), publicDer.subarray(publicDer.byteLength - 32));
  }
  await atomicWrite(manifestPath, source);
}

export async function validateProject(root: string): Promise<void> {
  const source = await readFile(join(root, "plugin.toml"), "utf8");
  const manifest = validateUiManifest(source);
  const ui = manifest.ui as Record<string, unknown>;
  const entrypoints = ui.entrypoints as Record<string, string | null>;
  const present = await Promise.all(Object.values(entrypoints).filter((entry): entry is string => entry !== null).map(async (entry) => {
    try { return (await stat(join(root, entry))).isFile(); } catch { return false; }
  }));
  if (present.some((value) => !value)) throw new Error("one or more declared UI entrypoints are missing; rebuild the project");
  const packageJson = JSON.parse(await readFile(join(root, "package.json"), "utf8")) as { dependencies?: Record<string, string> };
  if (packageJson.dependencies?.["@codypendent/ui"] === undefined) throw new Error("package.json must depend on @codypendent/ui");
}

function selection(host: UiCapabilities, worker: UiCapabilities): UiCapabilitySelection {
  const common = negotiateCapabilities(host, worker);
  const colorDepth = { monochrome: 1, ansi16: 4, ansi256: 8, trueColor: 24 }[common.colorDepth];
  return {
    protocolVersion: common.protocolVersions[0] as { major: number; minor: number },
    primitives: common.primitives === "*" ? ["*"] : common.primitives,
    capabilities: common.capabilities ?? [], contributionPoints: common.contributionPoints ?? [],
    imageProtocols: common.terminalGraphics ?? [], colorDepth,
    unicode: common.daemon.unicode, mouse: common.daemon.mouse, screenReader: common.screenReader,
    viewport: common.viewport, limits: common.limits ?? DEFAULT_UI_HARD_LIMITS,
  };
}

function printTree(node: unknown, indent = ""): void {
  if (node === null || typeof node !== "object") return;
  const value = node as { kind?: string; type?: string; id?: string; text?: string; children?: unknown[] };
  process.stdout.write(value.kind === "text" ? `${indent}text#${value.id ?? "?"} ${JSON.stringify(value.text)}\n` : `${indent}${value.type ?? value.kind}#${value.id ?? "?"}\n`);
  value.children?.forEach((child) => printTree(child, `${indent}  `));
}

export function inspectorNodeArguments(entrypoint: string): string[] {
  const candidate = resolve(entrypoint);
  let resolved = candidate;
  try { resolved = realpathSync(candidate); } catch { /* validation reports a missing entrypoint later */ }
  const [major = 0, minor = 0] = process.versions.node.split(".").map((part) => Number.parseInt(part, 10));
  if (major < 22 || (major === 22 && minor < 13)) throw new Error("worker inspection requires Node 22.13+ stable permission controls; refusing unrestricted execution");
  const packageRoot = basename(dirname(resolved)) === "dist" ? dirname(dirname(resolved)) : dirname(resolved);
  return [
    "--permission", `--allow-fs-read=${packageRoot}`, "--no-addons", "--disable-proto=delete",
    "--unhandled-rejections=strict", resolved,
  ];
}

export async function inspectWorker(entrypoint: string, json: boolean, hotReloadGeneration?: number): Promise<void> {
  const args = inspectorNodeArguments(entrypoint);
  const resolved = args[args.length - 1] as string;
  const packageRoot = basename(dirname(resolved)) === "dist" ? dirname(dirname(resolved)) : dirname(resolved);
  const child = spawn(process.execPath, args, { stdio: ["pipe", "pipe", "inherit"], env: {}, cwd: packageRoot });
  const exited = new Promise<{ code: number | null; signal: NodeJS.Signals | null }>((resolveExit) => child.once("exit", (code, signal) => resolveExit({ code, signal })));
  if (child.stdin === null || child.stdout === null) throw new Error("failed to open worker stdio");
  const writer = new UiFrameWriter(async (frame) => await new Promise<void>((resolvePromise, reject) => child.stdin?.write(frame, (error) => error === null || error === undefined ? resolvePromise() : reject(error))));
  let sequence = 0;
  const send = async (message: UiWireMessage): Promise<void> => { if (json) process.stdout.write(`> ${JSON.stringify(message)}\n`); await writer.write(message); };
  const host: UiCapabilities = {
    ...MINIMAL_TERMINAL_CAPABILITIES, client: "test", primitives: "*", colorDepth: "trueColor", media: ["image", "audio", "video"],
    capabilities: UI_HOST_CAPABILITIES,
    contributionPoints: UI_CONTRIBUTION_POINTS, limits: DEFAULT_UI_HARD_LIMITS,
    daemon: { rich_text: true, image_display: true, audio_capture: false, editor_mutations: false, diff_view: true, mouse: true, unicode: true, true_color: true },
  };
  await send({ type: "capabilities", messageId: `inspector-${++sequence}`, capabilities: host });
  let ready = false;
  let sawSnapshot = false;
  let disposed = false;
  let reloadAcknowledged = hotReloadGeneration === undefined;
  const timeout = setTimeout(() => child.kill("SIGKILL"), 10_000);
  try {
    for await (const value of decodeUiFrames(child.stdout as unknown as AsyncIterable<Uint8Array>)) {
      assertUiWireMessage(value, ready ? "worker-to-host" : "handshake");
      if (json) process.stdout.write(`< ${JSON.stringify(value)}\n`);
      if (value.type === "capabilities") await send({ type: "capabilitySelection", messageId: `inspector-${++sequence}`, selection: selection(host, value.capabilities) });
      else if (value.type === "worker.ready") ready = true;
      else if (value.type === "snapshot") {
        const firstSnapshot = !sawSnapshot;
        sawSnapshot = true;
        if (!json) printTree(value.snapshot.document.root);
        if (firstSnapshot && hotReloadGeneration !== undefined) {
          await send({ type: "hotReload", messageId: `inspector-${++sequence}`, hotReload: { generation: hotReloadGeneration, changedModules: ["dist/worker.mjs"] } });
        } else if (reloadAcknowledged) {
          await send({ type: "host.dispose", messageId: `inspector-${++sequence}`, extensions: { control: {} } });
        }
      }
      else if (value.type === "worker.reloaded") { reloadAcknowledged = true; }
      else if (value.type === "subscription") await send({ type: "projection", messageId: `inspector-${++sequence}`, projection: { subscriptionId: value.subscription.subscriptionId, removed: true } });
      else if (value.type === "action") await send({ type: "actionResult", messageId: `inspector-${++sequence}`, actionResult: { invocationId: value.action.invocationId, status: "cancelled" } });
      else if (value.type === "worker.disposed") { disposed = true; break; }
    }
  } finally {
    clearTimeout(timeout);
    await writer.close();
    child.stdin.end();
    if (!disposed && !child.killed) child.kill();
  }
  if (disposed) {
    let exitTimer: ReturnType<typeof setTimeout> | undefined;
    const status = await Promise.race([
      exited,
      new Promise<{ code: null; signal: NodeJS.Signals }>((resolveExit) => { exitTimer = setTimeout(() => {
        if (!child.killed) child.kill("SIGKILL");
        resolveExit({ code: null, signal: "SIGKILL" });
      }, 2_000); }),
    ]);
    if (exitTimer !== undefined) clearTimeout(exitTimer);
    if (status.code !== 0) throw new Error(`worker exited unsuccessfully (${status.code ?? status.signal ?? "unknown"}) after disposal`);
  }
  if (!ready || !sawSnapshot) throw new Error("worker did not complete handshake and emit a snapshot");
}

async function fingerprint(directory: string): Promise<string> {
  const hash = createHash("sha256");
  const walk = async (path: string): Promise<void> => {
    for (const entry of (await readdir(path, { withFileTypes: true })).sort((left, right) => left.name.localeCompare(right.name))) {
      const child = join(path, entry.name);
      if (entry.isDirectory()) await walk(child);
      else if (entry.isFile()) { const info = await stat(child); hash.update(child).update(String(info.mtimeMs)).update(String(info.size)); }
    }
  };
  await walk(directory);
  return hash.digest("hex");
}

async function dev(root: string, once: boolean): Promise<void> {
  let previous = "";
  let generation = 0;
  do {
    const current = await fingerprint(join(root, "src"));
    if (current !== previous) {
      previous = current;
      generation += 1;
      await runCommand("npm", ["run", "build"], root);
      await validateProject(root);
      await inspectWorker(join(root, "dist/worker.mjs"), false, generation);
      process.stdout.write("UI worker rebuilt, validated, and inspected.\n");
    }
    if (!once) await new Promise((resolvePromise) => setTimeout(resolvePromise, 350));
  } while (!once);
}

async function validateJson(path: string): Promise<void> {
  const value = JSON.parse(await readFile(resolve(path), "utf8")) as unknown;
  if (value !== null && typeof value === "object" && "type" in value) assertUiWireMessage(value, "handshake");
  else if (value !== null && typeof value === "object" && "root" in value) {
    const result = validateDocument(value as never); if (!result.valid) throw new Error(result.issues.map((issue) => `${issue.path}: ${issue.message}`).join("\n"));
  } else if (value !== null && typeof value === "object" && "patches" in value) {
    const result = validatePatchBatch(value as never); if (!result.valid) throw new Error(result.issues.map((issue) => `${issue.path}: ${issue.message}`).join("\n"));
  } else if (value !== null && typeof value === "object" && "eventId" in value) {
    assertUiWireMessage({ type: "event", messageId: "cli-event-validation", event: value } as unknown, "handshake");
  } else throw new Error("JSON is not a remote UI document, patch batch, or wire message");
}

function usage(): never {
  throw new Error("usage: codypendent-ui <create|validate|validate-json|build|test|dev|inspect|schema|package|sign> [path] [--template pure|react] [--key publisher.pem] [--json]");
}

export async function main(argv = process.argv.slice(2)): Promise<void> {
  const [command, ...args] = argv;
  if (command === undefined || command === "help" || command === "--help") usage();
  const values = positional(args);
  const root = resolve(values[0] ?? ".");
  switch (command) {
    case "create": {
      const template = option(args, "--template") ?? "react";
      if (template !== "pure" && template !== "react") throw new Error("--template must be pure or react");
      process.stdout.write(`Created ${await createScaffold(values[0] ?? "codypendent-ui", template as ScaffoldTemplate)}\n`); break;
    }
    case "validate": await validateProject(root); process.stdout.write("Manifest and package are valid.\n"); break;
    case "validate-json": await validateJson(values[0] ?? usage()); process.stdout.write("Remote UI JSON is valid.\n"); break;
    case "build": await runCommand("npm", ["run", "build"], root); await validateProject(root); break;
    case "test": await runCommand("npm", ["test"], root); break;
    case "dev": await dev(root, has(args, "--once")); break;
    case "inspect": await inspectWorker(values[0] ?? join(process.cwd(), "dist/worker.mjs"), has(args, "--json")); break;
    case "schema": {
      const output = resolve(option(args, "--output") ?? values[0] ?? "remote-ui.schema.json");
      await mkdir(dirname(output), { recursive: true }); await copyFile(fileURLToPath(REMOTE_UI_SCHEMA_URL), output); process.stdout.write(`${output}\n`); break;
    }
    case "package": {
      if (!has(args, "--skip-build")) await runCommand("npm", ["run", "build"], root);
      await validateProject(root);
      const output = resolve(option(args, "--output") ?? join(root, `${basename(root)}.cody-ui.tgz`));
      await atomicWrite(output, await createDeterministicArchive(root));
      await signArtifact(root, output, option(args, "--key"));
      process.stdout.write(`${output}\n`); break;
    }
    case "sign": {
      const artifact = resolve(values[1] ?? option(args, "--artifact") ?? `${basename(root)}.cody-ui.tgz`);
      const key = option(args, "--key"); if (key === undefined) throw new Error("sign requires --key <ed25519-private.pem>");
      await signArtifact(root, artifact, key); process.stdout.write("Manifest checksum and Ed25519 signature updated.\n"); break;
    }
    default: usage();
  }
}

const invokedPath = process.argv[1];
let isMain = false;
if (invokedPath !== undefined) {
  try { isMain = realpathSync(invokedPath) === fileURLToPath(import.meta.url); } catch { isMain = resolve(invokedPath) === fileURLToPath(import.meta.url); }
}
if (isMain && invokedPath !== undefined) {
  const invoked = basename(invokedPath);
  const args = invoked.startsWith("create-codypendent-ui") ? ["create", ...process.argv.slice(2)] : process.argv.slice(2);
  main(args).catch((cause) => { process.stderr.write(`codypendent-ui: ${cause instanceof Error ? cause.message : String(cause)}\n`); process.exitCode = 1; });
}
