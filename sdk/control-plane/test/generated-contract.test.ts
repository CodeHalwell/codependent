import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const schemaRoot = join(packageRoot, "schema");
const generatedRoot = join(packageRoot, "src", "generated");

/**
 * Schemas the Rust exporter writes that deliberately have no TypeScript target: each one
 * is a single type that already appears inside a catalog schema, so generating it again
 * would emit a duplicate declaration of the same wire type. Listing them here rather than
 * ignoring unmatched files means a NEW schema cannot appear in Rust and silently never
 * reach a client — the drift this whole pipeline exists to prevent.
 */
const intentionallyUngenerated = new Set([
  "audit-record.schema.json",
  "control-plane-error.schema.json",
  "daemon.schema.json",
  "organization.schema.json",
  "protocol-handshake-request.schema.json",
  "protocol-handshake-response.schema.json",
  "protocol-version.schema.json",
  "repository.schema.json",
  "stream-event.schema.json",
  "sync-envelope.schema.json",
  "user.schema.json",
]);

function generatorTargets(): { schema: string; output: string }[] {
  const source = readFileSync(join(packageRoot, "scripts", "generate.mjs"), "utf8");
  const targets: { schema: string; output: string }[] = [];
  const pattern = /schema:\s*"([^"]+)",\s*\n\s*output:\s*"([^"]+)"/g;
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(source)) !== null) {
    targets.push({ schema: match[1]!, output: match[2]! });
  }
  return targets;
}

describe("generated control-plane bindings", () => {
  const targets = generatorTargets();
  const schemas = readdirSync(schemaRoot).filter((name) => name.endsWith(".schema.json"));
  const generated = readdirSync(generatedRoot).filter((name) => name.endsWith(".ts"));

  it("finds every exported Rust schema", () => {
    expect(schemas.length).toBeGreaterThan(0);
    const mapped = new Set(targets.map((target) => target.schema));
    const unaccounted = schemas.filter(
      (name) => !mapped.has(name) && !intentionallyUngenerated.has(name),
    );
    expect(unaccounted).toEqual([]);
  });

  it("never lists a target whose schema no longer exists", () => {
    const present = new Set(schemas);
    expect(targets.filter((target) => !present.has(target.schema))).toEqual([]);
    const generatedNames = new Set(schemas);
    expect([...intentionallyUngenerated].filter((name) => !generatedNames.has(name))).toEqual([]);
  });

  it("emits one module per target plus a barrel, all re-exported", () => {
    const expected = new Set([...targets.map((target) => target.output), "index.ts"]);
    expect(new Set(generated)).toEqual(expected);

    const barrel = readFileSync(join(generatedRoot, "index.ts"), "utf8");
    for (const target of targets) {
      expect(barrel).toContain(`./${target.output.replace(/\.ts$/, ".js")}`);
    }
  });

  it("marks every generated module as machine-written", () => {
    for (const name of generated) {
      if (name === "index.ts") continue;
      const source = readFileSync(join(generatedRoot, name), "utf8");
      expect(source, name).toContain("Do not edit by hand");
    }
  });

  it("declares each wire type exactly once across the generated modules", () => {
    const owners = new Map<string, string>();
    const duplicates: string[] = [];
    for (const name of generated) {
      if (name === "index.ts") continue;
      const source = readFileSync(join(generatedRoot, name), "utf8");
      for (const match of source.matchAll(/^export (?:interface|type) ([A-Za-z_$][\w$]*)/gm)) {
        const typeName = match[1]!;
        if (owners.has(typeName)) {
          duplicates.push(`${typeName} (${owners.get(typeName)} and ${name})`);
        } else {
          owners.set(typeName, name);
        }
      }
    }
    expect(duplicates).toEqual([]);
  });

  it("carries the fail-closed 'unknown' member on every wire enum that has one in Rust", () => {
    // The whole point of the Rust `#[serde(other)] Unknown` variants is that a client can
    // see a tag it does not understand. If the generated union dropped "unknown", a
    // TypeScript consumer would narrow exhaustively over a set that cannot occur.
    const policy = readFileSync(join(generatedRoot, "policy.ts"), "utf8");
    expect(policy).toMatch(/type PublicationClass =[\s\S]*?"unknown"/);
    expect(policy).toMatch(/type DataClassification =[\s\S]*?"unknown"/);

    const rbac = readFileSync(join(generatedRoot, "rbac.ts"), "utf8");
    expect(rbac).toMatch(/type ControlPlaneRole =[\s\S]*?"unknown"/);
    expect(rbac).toMatch(/type RbacAction =[\s\S]*?"unknown"/);

    const sync = readFileSync(join(generatedRoot, "sync.ts"), "utf8");
    expect(sync).toMatch(/type SyncDeltaKind =[\s\S]*?"unknown"/);
  });

  it("keeps the batched sync envelope as the push body", () => {
    // `POST /v1/sync/push` accepts a flat single-delta body today. This is the shape the
    // contract requires, and the assertion that has to keep passing once the route is fixed.
    const envelope = JSON.parse(
      readFileSync(join(schemaRoot, "sync-envelope.schema.json"), "utf8"),
    ) as { required?: string[]; properties?: Record<string, unknown> };
    expect(Object.keys(envelope.properties ?? {}).sort()).toEqual([
      "daemon_id",
      "deltas",
      "organization_id",
      "protocol_version",
      "sent_at",
    ]);
    expect(envelope.required).toContain("protocol_version");
    expect(envelope.required).toContain("deltas");
  });
});
