#!/usr/bin/env node
import { createHash, createPrivateKey, createPublicKey, sign as edSign } from "node:crypto";
import { spawn } from "node:child_process";
import { copyFile, mkdir, readFile, readdir, rename, stat, writeFile } from "node:fs/promises";
import { realpathSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { DEFAULT_UI_HARD_LIMITS, type UiCapabilities, type UiCapabilitySelection, type UiDocument, type UiJsonValue, type UiWireMessage } from "../protocol.js";
import { REMOTE_UI_SCHEMA_URL } from "../schema.js";
import { validatePatchBatch } from "../validation.js";
import { negotiateCapabilities } from "../capabilities.js";
import { TransactionalHotReload } from "../hot-reload.js";
import { applyPatchBatch } from "../testing.js";
import { decodeUiFrames, UiFrameWriter } from "../worker/framing.js";
import { assertUiWireMessage } from "../worker/wire.js";
import { artifactChecksum, createDeterministicArchive } from "./archive.js";
import { diagnoseDocument, formatDevelopmentDiagnostic, jsonRecord, type DevelopmentDiagnostic, type WorkbenchTarget } from "./diagnostics.js";
import { parseCanonicalManifest, rustSigningDigest, updateSecurityFields, validateUiManifest } from "./manifest.js";
import { createScaffold, type ScaffoldTemplate } from "./scaffold.js";
import { UI_CONFORMANCE_STORY } from "./stories.js";
import {
  DEFAULT_WORKBENCH_OPTIONS,
  actionFixture,
  colorDepth,
  createWorkbenchCapabilities,
  formatNodeTree,
  loadWorkbenchFixture,
  projectionFixture,
  viewport,
  workbenchTheme,
  type WorkbenchOptions,
  type WorkbenchReport,
} from "./workbench.js";

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

function controlStates(message: Extract<UiWireMessage, { type: `host.${string}` | `worker.${string}` }>): Readonly<Record<string, UiJsonValue>> | undefined {
  const control = jsonRecord(message.extensions?.control);
  return jsonRecord(control?.states);
}

function workbenchDiagnostic(
  code: string,
  message: string,
  severity: DevelopmentDiagnostic["severity"] = "warning",
): DevelopmentDiagnostic {
  return { kind: "validation", severity, code, path: "worker", message };
}

/**
 * Runs a worker against a configurable host workbench and returns its complete
 * protocol/placement/a11y report. The candidate is always isolated in a fresh
 * permission-restricted process; callers commit it only after this resolves.
 */
export async function inspectWorker(
  entrypoint: string,
  json: boolean,
  hotReloadGeneration?: number,
  options: WorkbenchOptions = DEFAULT_WORKBENCH_OPTIONS,
  hotReloadState: Readonly<Record<string, UiJsonValue>> = options.fixture?.hotReloadState ?? {},
): Promise<WorkbenchReport> {
  const args = inspectorNodeArguments(entrypoint);
  const resolved = args[args.length - 1] as string;
  const packageRoot = basename(dirname(resolved)) === "dist" ? dirname(dirname(resolved)) : dirname(resolved);
  const encodedHotReloadState = JSON.stringify(hotReloadState);
  if (Buffer.byteLength(encodedHotReloadState, "utf8") > 256 * 1024) throw new Error("workbench hot-reload state exceeds 256 KiB");
  const child = spawn(process.execPath, args, {
    stdio: ["pipe", "pipe", "inherit"],
    env: {
      CODYPENDENT_UI_HMR_GENERATION: String(hotReloadGeneration ?? 0),
      CODYPENDENT_UI_HMR_STATE: encodedHotReloadState,
    },
    cwd: packageRoot,
  });
  const exited = new Promise<{ code: number | null; signal: NodeJS.Signals | null }>((resolveExit) => child.once("exit", (code, signal) => resolveExit({ code, signal })));
  if (child.stdin === null || child.stdout === null) throw new Error("failed to open worker stdio");
  const writer = new UiFrameWriter(async (frame) => await new Promise<void>((resolvePromise, reject) => child.stdin?.write(frame, (error) => error === null || error === undefined ? resolvePromise() : reject(error))));
  let sequence = 0;
  const trace: WorkbenchReport["trace"][number][] = [];
  const send = async (message: UiWireMessage): Promise<void> => {
    trace.push({ direction: "host→worker", type: message.type, detail: message.messageId });
    if (json) process.stdout.write(`> ${JSON.stringify(message)}\n`);
    await writer.write(message);
  };
  const host = createWorkbenchCapabilities(options);
  await send({ type: "capabilities", messageId: `inspector-${++sequence}`, capabilities: host });
  let ready = false;
  let sawSnapshot = false;
  let sawContributions = false;
  let disposed = false;
  let reloadAcknowledged = hotReloadGeneration === undefined;
  let reloadRequested = false;
  let fixtureEventsSent = false;
  const documents = new Map<string, UiDocument>();
  const contributions: WorkbenchReport["contributions"][number][] = [];
  const workerDiagnostics: DevelopmentDiagnostic[] = [];
  const subscriptions: string[] = [];
  const actions: string[] = [];
  const patches: string[] = [];
  const events: string[] = [];
  let exportedState = structuredClone(hotReloadState);
  const maybeAdvance = async (): Promise<void> => {
    if (!sawSnapshot || !sawContributions || disposed) return;
    if (!fixtureEventsSent) {
      fixtureEventsSent = true;
      for (const [index, fixture] of (options.fixture?.events ?? []).entries()) {
        const document = fixture.documentId === undefined
          ? documents.values().next().value as UiDocument | undefined
          : documents.get(fixture.documentId);
        if (document === undefined) {
          workerDiagnostics.push(workbenchDiagnostic("event-document-missing", `Fixture event ${index} targets an unknown document`, "error"));
          continue;
        }
        events.push(`${fixture.type}:${fixture.targetId}`);
        await send({
          type: "event",
          messageId: `inspector-${++sequence}`,
          event: {
            protocolVersion: document.protocolVersion,
            eventId: `workbench-event-${index + 1}`,
            documentId: document.documentId,
            revision: fixture.revision ?? document.revision,
            targetId: fixture.targetId,
            type: fixture.type,
            ...(fixture.payload === undefined ? {} : { payload: fixture.payload }),
            ...(fixture.modifiers === undefined ? {} : { modifiers: fixture.modifiers }),
            timestamp: new Date(0).toISOString(),
          },
        });
      }
    }
    if (hotReloadGeneration !== undefined && !reloadRequested) {
      reloadRequested = true;
      await send({
        type: "hotReload",
        messageId: `inspector-${++sequence}`,
        hotReload: { generation: hotReloadGeneration, changedModules: ["dist/worker.mjs"] },
      });
      return;
    }
    if (reloadAcknowledged) {
      await send({ type: "host.dispose", messageId: `inspector-${++sequence}`, extensions: { control: {} } });
    }
  };
  const timeout = setTimeout(() => child.kill("SIGKILL"), 10_000);
  try {
    for await (const value of decodeUiFrames(child.stdout as unknown as AsyncIterable<Uint8Array>)) {
      assertUiWireMessage(value, ready ? "worker-to-host" : "handshake");
      trace.push({ direction: "worker→host", type: value.type, detail: value.messageId });
      if (json) process.stdout.write(`< ${JSON.stringify(value)}\n`);
      if (value.type === "capabilities") await send({ type: "capabilitySelection", messageId: `inspector-${++sequence}`, selection: selection(host, value.capabilities) });
      else if (value.type === "worker.ready") {
        ready = true;
        await send({ type: "theme", messageId: `inspector-${++sequence}`, theme: options.theme });
      }
      else if (value.type === "snapshot") {
        sawSnapshot = true;
        documents.set(value.snapshot.document.documentId, value.snapshot.document);
        if (!json) {
          process.stdout.write(`\n[${options.target}:${options.point} ${options.viewport.width}x${options.viewport.height} ${options.theme.id}] ${value.snapshot.document.documentId}@${value.snapshot.document.revision}\n`);
          process.stdout.write(`${formatNodeTree(value.snapshot.document.root).join("\n")}\n`);
        }
        await maybeAdvance();
      }
      else if (value.type === "patchBatch") {
        patches.push(`${value.patchBatch.documentId}:${value.patchBatch.baseRevision}→${value.patchBatch.revision}`);
        const current = documents.get(value.patchBatch.documentId);
        if (current === undefined) {
          workerDiagnostics.push(workbenchDiagnostic("patch-without-snapshot", `Patch received for unknown document ${value.patchBatch.documentId}`));
          await send({ type: "resync", messageId: `inspector-${++sequence}`, resync: { documentId: value.patchBatch.documentId } });
        } else {
          try { documents.set(current.documentId, applyPatchBatch(current, value.patchBatch)); }
          catch (cause) {
            workerDiagnostics.push(workbenchDiagnostic("invalid-patch", cause instanceof Error ? cause.message : String(cause), "error"));
            await send({ type: "resync", messageId: `inspector-${++sequence}`, resync: { documentId: value.patchBatch.documentId, knownRevision: current.revision } });
          }
        }
      }
      else if (value.type === "contributions") {
        sawContributions = true;
        contributions.splice(0, contributions.length, ...value.contributions);
        for (const contribution of value.contributions) {
          if (contribution.point !== options.point) {
            workerDiagnostics.push({
              kind: "fallback", severity: "info", code: "point-not-mounted", path: contribution.id,
              message: `${contribution.point} is not mounted in the selected ${options.point} workbench point`,
              suggestion: `Pass --point ${contribution.point} to inspect this placement.`,
            });
          }
        }
        await maybeAdvance();
      }
      else if (value.type === "worker.reloaded") {
        reloadAcknowledged = true;
        exportedState = controlStates(value) ?? exportedState;
      }
      else if (value.type === "subscription") {
        subscriptions.push(`${value.subscription.kind}:${value.subscription.resourceId ?? ""}`);
        const fixture = projectionFixture(options.fixture, value);
        await send({
          type: "projection",
          messageId: `inspector-${++sequence}`,
          projection: { subscriptionId: value.subscription.subscriptionId, ...(fixture ?? { removed: true }) },
        });
      }
      else if (value.type === "unsubscribe") subscriptions.push(`unsubscribe:${value.unsubscription.subscriptionId}`);
      else if (value.type === "action") {
        actions.push(value.action.actionId);
        const fixture = actionFixture(options.fixture, value);
        await send({
          type: "actionResult",
          messageId: `inspector-${++sequence}`,
          actionResult: { invocationId: value.action.invocationId, ...(fixture ?? { status: "cancelled" }) },
        });
      }
      else if (value.type === "cancelAction") actions.push(`cancel:${value.cancellation.invocationId}`);
      else if (value.type === "event") events.push(`${value.event.type}:${value.event.targetId}`);
      else if (value.type === "error") workerDiagnostics.push(workbenchDiagnostic(value.error.code, value.error.message, value.error.recoverable === false ? "error" : "warning"));
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
  if (!disposed) throw new Error("worker did not acknowledge clean workbench disposal");
  if (!ready || !sawSnapshot) throw new Error("worker did not complete handshake and emit a snapshot");
  if (!sawContributions) throw new Error("worker did not advertise its contribution placement");
  const finalDocuments = [...documents.values()];
  const diagnostics = [...workerDiagnostics, ...finalDocuments.flatMap((document) => diagnoseDocument(document, options.target))];
  if (!json) {
    process.stdout.write(`\nWorkbench diagnostics (${diagnostics.length}):\n`);
    diagnostics.forEach((diagnostic) => process.stdout.write(`  ${formatDevelopmentDiagnostic(diagnostic)}\n`));
    process.stdout.write(`Protocol: ${trace.length} messages; ${patches.length} patches; ${subscriptions.length} subscriptions; ${actions.length} actions.\n`);
  }
  if (diagnostics.some((diagnostic) => diagnostic.severity === "error")) {
    throw new Error(`workbench rejected ${diagnostics.filter((diagnostic) => diagnostic.severity === "error").length} error diagnostic(s)`);
  }
  return {
    target: options.target,
    point: options.point,
    documents: finalDocuments,
    contributions,
    diagnostics,
    trace,
    subscriptions,
    actions,
    patches,
    events,
    hotReloadState: exportedState,
  };
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

function workbenchTarget(value: string | undefined): WorkbenchTarget {
  if (value === undefined) return DEFAULT_WORKBENCH_OPTIONS.target;
  if (["terminal", "vscode", "web", "test"].includes(value)) return value as WorkbenchTarget;
  throw new Error("--target must be terminal, vscode, web, or test");
}

async function workbenchOptions(args: string[]): Promise<WorkbenchOptions> {
  const fixturePath = option(args, "--fixture");
  const fixture = fixturePath === "conformance"
    ? UI_CONFORMANCE_STORY
    : fixturePath === undefined ? undefined : await loadWorkbenchFixture(fixturePath);
  const target = option(args, "--target") === undefined && fixture !== undefined
    ? fixture.target
    : workbenchTarget(option(args, "--target"));
  const point = option(args, "--point") ?? fixture?.point ?? DEFAULT_WORKBENCH_OPTIONS.point;
  return {
    target,
    point,
    viewport: viewport(option(args, "--viewport")),
    colorDepth: colorDepth(option(args, "--color-depth")),
    theme: workbenchTheme(option(args, "--theme")),
    ...(fixture === undefined ? {} : { fixture }),
  };
}

async function dev(root: string, once: boolean, options: WorkbenchOptions): Promise<void> {
  let previous = "";
  let active: TransactionalHotReload<WorkbenchReport> | undefined;
  do {
    const current = await fingerprint(join(root, "src"));
    if (current !== previous) {
      previous = current;
      const prepare = async (state: Readonly<Record<string, UiJsonValue>>, generation: number): Promise<WorkbenchReport> => {
        await runCommand("npm", ["run", "build"], root);
        await validateProject(root);
        return await inspectWorker(join(root, "dist/worker.mjs"), false, generation, options, state);
      };
      if (active === undefined) {
        const generation = 1;
        try {
          const report = await prepare(options.fixture?.hotReloadState ?? {}, generation);
          active = new TransactionalHotReload({ value: report, states: report.hotReloadState }, generation);
          process.stdout.write(`Workbench committed generation ${generation}. Watching for changes…\n`);
        } catch (cause) {
          if (once) throw cause;
          process.stderr.write(`Workbench candidate ${generation} rejected: ${cause instanceof Error ? cause.message : String(cause)}\n`);
        }
      } else {
        const result = await active.reload(async ({ generation, states }) => {
          const report = await prepare(states, generation);
          return { value: report, states: report.hotReloadState };
        });
        if (result.committed) process.stdout.write(`Workbench committed generation ${result.generation}. Watching for changes…\n`);
        else process.stderr.write(`Workbench rolled back generation ${result.generation}: ${result.reason}. Last-valid generation ${active.generation} remains active.\n`);
        if (once && !result.committed) throw new Error(result.reason);
      }
    }
    if (!once) await new Promise((resolvePromise) => setTimeout(resolvePromise, 350));
  } while (!once);
}

async function validateJson(path: string, target: WorkbenchTarget): Promise<void> {
  const value = JSON.parse(await readFile(resolve(path), "utf8")) as unknown;
  if (value !== null && typeof value === "object" && "type" in value) assertUiWireMessage(value, "handshake");
  else if (value !== null && typeof value === "object" && "root" in value) {
    const diagnostics = diagnoseDocument(value as UiDocument, target);
    diagnostics.forEach((diagnostic) => process.stdout.write(`${formatDevelopmentDiagnostic(diagnostic)}\n`));
    if (diagnostics.some((diagnostic) => diagnostic.severity === "error")) {
      throw new Error(`document has ${diagnostics.filter((diagnostic) => diagnostic.severity === "error").length} error diagnostic(s)`);
    }
  } else if (value !== null && typeof value === "object" && "patches" in value) {
    const result = validatePatchBatch(value as never); if (!result.valid) throw new Error(result.issues.map((issue) => `${issue.path}: ${issue.message}`).join("\n"));
  } else if (value !== null && typeof value === "object" && "eventId" in value) {
    assertUiWireMessage({ type: "event", messageId: "cli-event-validation", event: value } as unknown, "handshake");
  } else throw new Error("JSON is not a remote UI document, patch batch, or wire message");
}

function usage(): never {
  throw new Error("usage: codypendent-ui <create|validate|validate-json|build|test|dev|workbench|inspect|schema|package|sign> [path] [--template pure|react] [--target terminal|vscode|web|test] [--point point] [--viewport WIDTHxHEIGHT] [--theme dark|light|highContrast|monochrome] [--color-depth monochrome|ansi16|ansi256|trueColor] [--fixture fixture.json|conformance] [--key publisher.pem] [--json]");
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
    case "validate-json": await validateJson(values[0] ?? usage(), workbenchTarget(option(args, "--target"))); process.stdout.write("Remote UI JSON is valid.\n"); break;
    case "build": await runCommand("npm", ["run", "build"], root); await validateProject(root); break;
    case "test": await runCommand("npm", ["test"], root); break;
    case "dev":
    case "workbench": await dev(root, has(args, "--once"), await workbenchOptions(args)); break;
    case "inspect": await inspectWorker(values[0] ?? join(process.cwd(), "dist/worker.mjs"), has(args, "--json"), undefined, await workbenchOptions(args)); break;
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
