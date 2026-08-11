import { parse as parseToml } from "smol-toml";
import { createHash } from "node:crypto";
import { UI_CONTRIBUTION_POINTS, UI_HOST_CAPABILITIES } from "../protocol.js";

type Data = Record<string, unknown>;

const UI_CAPABILITIES = new Set<string>(UI_HOST_CAPABILITIES);
const PUBLIC_POINTS = new Set<string>(UI_CONTRIBUTION_POINTS);
const CORE_POINTS = new Set(["approval-frame", "approval-actions", "secret-entry", "policy-state", "terminal-lifecycle"]);

function object(value: unknown, name: string): Data {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error(`${name} must be a table`);
  return value as Data;
}
function string(value: unknown, name: string, fallback = ""): string {
  if (value === undefined) return fallback;
  if (typeof value !== "string") throw new Error(`${name} must be a string`);
  return value;
}
function integer(value: unknown, name: string, fallback: number): number {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || (value as number) < 0) throw new Error(`${name} must be a non-negative integer`);
  return value as number;
}
function boolean(value: unknown, name: string, fallback: boolean): boolean {
  if (value === undefined) return fallback;
  if (typeof value !== "boolean") throw new Error(`${name} must be a boolean`);
  return value;
}
function strings(value: unknown, name: string): string[] {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.some((item) => typeof item !== "string")) throw new Error(`${name} must be an array of strings`);
  return value as string[];
}

function relativeEntrypoint(value: string, name: string): string | null {
  if (value === "") return null;
  if (value.startsWith("/") || value.includes("\\") || value.includes("%") || value.includes(":") || value.split("/").some((part) => part === "" || part === "." || part === "..")) {
    throw new Error(`${name} must be a normalized package-relative path`);
  }
  return value;
}

/** Normalize in the exact Rust serde field order/default shape used by signing_digest. */
export function parseCanonicalManifest(source: string): Data {
  const root = object(parseToml(source), "manifest");
  const runtime = object(root.runtime ?? {}, "runtime");
  const capabilities = object(root.capabilities ?? {}, "capabilities");
  const resources = object(root.resources ?? {}, "resources");
  const security = object(root.security ?? {}, "security");
  const update = object(root.update ?? {}, "update");
  const normalized: Data = {
    schema_version: integer(root.schema_version, "schema_version", 0),
    id: string(root.id, "id"), name: string(root.name, "name"), version: string(root.version, "version"),
    kind: string(root.kind, "kind"), publisher: string(root.publisher, "publisher"), scopes: strings(root.scopes, "scopes"),
    runtime: {
      command: string(runtime.command, "runtime.command"), protocol: string(runtime.protocol, "runtime.protocol"),
      working_directory: string(runtime.working_directory, "runtime.working_directory"),
    },
    capabilities: {
      filesystem_read: strings(capabilities.filesystem_read, "capabilities.filesystem_read"),
      filesystem_write: strings(capabilities.filesystem_write, "capabilities.filesystem_write"),
      network: strings(capabilities.network, "capabilities.network"), secrets: strings(capabilities.secrets, "capabilities.secrets"),
      subprocess: boolean(capabilities.subprocess, "capabilities.subprocess", false),
    },
    resources: {
      memory_mb: integer(resources.memory_mb, "resources.memory_mb", 128),
      cpu_seconds: integer(resources.cpu_seconds, "resources.cpu_seconds", 30),
      wall_seconds: integer(resources.wall_seconds, "resources.wall_seconds", 60),
      maximum_output_mb: integer(resources.maximum_output_mb, "resources.maximum_output_mb", 8),
    },
    security: {
      checksum: string(security.checksum, "security.checksum"), signature: string(security.signature, "security.signature"),
      sandbox_profile: string(security.sandbox_profile, "security.sandbox_profile"),
    },
    update: {
      channel: string(update.channel, "update.channel"),
      permission_change_requires_approval: boolean(update.permission_change_requires_approval, "update.permission_change_requires_approval", true),
    },
  };
  if (root.ui !== undefined) {
    const ui = object(root.ui, "ui");
    const compatibility = object(ui.compatibility, "ui.compatibility");
    const entrypoints = object(ui.entrypoints, "ui.entrypoints");
    const contributions = ui.contributions ?? [];
    if (!Array.isArray(contributions)) throw new Error("ui.contributions must be an array of tables");
    normalized.ui = {
      schema_version: integer(ui.schema_version, "ui.schema_version", 0),
      compatibility: { protocol: string(compatibility.protocol, "ui.compatibility.protocol"), sdk: string(compatibility.sdk, "ui.compatibility.sdk") },
      entrypoints: {
        shared: relativeEntrypoint(string(entrypoints.shared, "ui.entrypoints.shared"), "ui.entrypoints.shared"),
        terminal: relativeEntrypoint(string(entrypoints.terminal, "ui.entrypoints.terminal"), "ui.entrypoints.terminal"),
        web: relativeEntrypoint(string(entrypoints.web, "ui.entrypoints.web"), "ui.entrypoints.web"),
      },
      requested_capabilities: strings(ui.requested_capabilities, "ui.requested_capabilities"),
      contributions: contributions.map((raw, index) => {
        const contribution = object(raw, `ui.contributions[${index}]`);
        const fallback = string(contribution.fallback_renderer, `ui.contributions[${index}].fallback_renderer`);
        return {
          id: string(contribution.id, `ui.contributions[${index}].id`), point: string(contribution.point, `ui.contributions[${index}].point`),
          renderer: string(contribution.renderer, `ui.contributions[${index}].renderer`), targets: strings(contribution.targets, `ui.contributions[${index}].targets`),
          fallback_renderer: fallback === "" ? null : fallback,
        };
      }),
    };
  }
  return normalized;
}

export function validateUiManifest(source: string): Data {
  const manifest = parseCanonicalManifest(source);
  if (manifest.schema_version !== 1) throw new Error("schema_version must be 1");
  for (const field of ["id", "name", "version", "publisher"] as const) if (manifest[field] === "") throw new Error(`${field} must not be empty`);
  if (manifest.kind !== "ui-component") throw new Error("UI package kind must be ui-component");
  const runtime = manifest.runtime as Data;
  const daemonCapabilities = manifest.capabilities as Data;
  if (Object.values(runtime).some((value) => value !== "")) throw new Error("ui-component packages cannot declare [runtime]");
  if (Object.entries(daemonCapabilities).some(([key, value]) => key === "subprocess" ? value === true : (value as unknown[]).length > 0)) throw new Error("ui-component packages cannot declare daemon [capabilities]");
  const resources = manifest.resources as Data;
  for (const [key, value] of Object.entries(resources)) if ((value as number) <= 0) throw new Error(`resources.${key} must be greater than zero`);
  const ui = object(manifest.ui, "ui");
  if (ui.schema_version !== 1) throw new Error("ui.schema_version must be 1");
  const entrypoints = object(ui.entrypoints, "ui.entrypoints");
  if (Object.values(entrypoints).every((entry) => entry === null)) throw new Error("at least one UI entrypoint is required");
  const compatibility = object(ui.compatibility, "ui.compatibility");
  if (typeof compatibility.protocol !== "string" || !compatibility.protocol.includes("1.0")) throw new Error("ui.compatibility.protocol must include protocol 1.0");
  if (typeof compatibility.sdk !== "string" || !/(?:\^|>=)?1(?:\.0)?/u.test(compatibility.sdk)) throw new Error("ui.compatibility.sdk must include SDK 1.x");
  const requested = ui.requested_capabilities as string[];
  if (new Set(requested).size !== requested.length || requested.some((capability) => !UI_CAPABILITIES.has(capability))) throw new Error("requested_capabilities contains a duplicate or unknown value");
  const contributions = ui.contributions as Data[];
  const ids = new Set<string>();
  for (const contribution of contributions) {
    const id = contribution.id as string;
    if (!/^[A-Za-z0-9][A-Za-z0-9._/-]*[A-Za-z0-9]$|^[A-Za-z0-9]$/u.test(id) || id.includes("..") || ids.has(id)) throw new Error(`invalid or duplicate contribution id ${JSON.stringify(id)}`);
    ids.add(id);
    if (!/^[A-Za-z0-9][A-Za-z0-9._/-]*[A-Za-z0-9]$|^[A-Za-z0-9]$/u.test(contribution.renderer as string)) throw new Error(`invalid renderer id ${JSON.stringify(contribution.renderer)}`);
    if (CORE_POINTS.has(contribution.point as string)) throw new Error(`contribution ${id} targets core-owned point ${contribution.point as string}`);
    if (!PUBLIC_POINTS.has(contribution.point as string)) throw new Error(`contribution ${id} targets unknown public point ${contribution.point as string}`);
    const targets = contribution.targets as string[];
    if (targets.length === 0 || targets.some((target) => !["shared", "terminal", "web"].includes(target))) throw new Error(`contribution ${id} has invalid targets`);
    if (targets.includes("shared") && targets.length > 1) throw new Error(`contribution ${id} cannot combine shared with terminal/web targets`);
    if (targets.length === 1 && targets[0] === "web" && contribution.fallback_renderer === null) throw new Error(`web-only contribution ${id} requires fallback_renderer`);
    for (const target of targets) {
      const supported = target === "shared" ? entrypoints.shared !== null : target === "terminal" ? entrypoints.shared !== null || entrypoints.terminal !== null : entrypoints.shared !== null || entrypoints.web !== null;
      if (!supported) throw new Error(`contribution ${id} targets ${target} without a compatible entrypoint`);
    }
  }
  for (const contribution of contributions) {
    const targets = contribution.targets as string[];
    if (!(targets.length === 1 && targets[0] === "web")) continue;
    const fallback = contribution.fallback_renderer as string;
    const resolved = contributions.some((candidate) =>
      candidate.id !== contribution.id
      && candidate.renderer === fallback
      && candidate.point === contribution.point
      && (candidate.targets as string[]).some((target) => target === "terminal" || target === "shared"));
    if (!resolved) throw new Error(`web-only contribution ${contribution.id as string} fallback_renderer ${fallback} must reference a different same-point terminal/shared contribution`);
  }
  const security = manifest.security as Data;
  if (!/^sha256:[a-fA-F0-9]{64}$/u.test(security.checksum as string)) throw new Error("security.checksum must be sha256:<64 hex characters>");
  return manifest;
}

export function updateSecurityFields(source: string, checksum: string, signature: string): string {
  const lines = source.split(/\r?\n/u);
  const section = lines.findIndex((line) => line.trim() === "[security]");
  if (section < 0) throw new Error("plugin.toml is missing [security]");
  let end = lines.findIndex((line, index) => index > section && /^\s*\[/u.test(line));
  if (end < 0) end = lines.length;
  const set = (key: string, value: string): void => {
    const index = lines.findIndex((line, position) => position > section && position < end && new RegExp(`^\\s*${key}\\s*=`).test(line));
    const next = `${key} = ${JSON.stringify(value)}`;
    if (index >= 0) lines[index] = next;
    else { lines.splice(end, 0, next); end += 1; }
  };
  set("checksum", checksum);
  set("signature", signature);
  return `${lines.join("\n").replace(/\n+$/u, "")}\n`;
}

/** Exact domain-separated digest implemented by crates/sandbox/src/verify.rs. */
export function rustSigningDigest(manifest: Data): Uint8Array {
  const signable = structuredClone(manifest);
  object(signable.security, "security").signature = "";
  const canonical = Buffer.from(JSON.stringify(signable), "utf8");
  const length = Buffer.alloc(8);
  length.writeBigUInt64BE(BigInt(canonical.byteLength));
  return createHash("sha256").update("codypendent-plugin-signature-v1").update(length).update(canonical).digest();
}
