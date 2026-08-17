import { readFile, rename, rm, mkdir, stat, writeFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { compile } from "json-schema-to-typescript";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const schemaRoot = join(packageRoot, "schema");
const canonicalOutputRoot = join(packageRoot, "src", "generated");

// Order is the ownership order used by `selectExports`: the first module to produce a
// type name owns its export. Domain catalogs come first so each type is exported from the
// module it belongs to; `page-catalog` is last because it only re-wraps them in `Page<T>`.
const targets = [
  {
    schema: "control-plane-id-catalog.schema.json",
    output: "ids.ts",
    exports: ["ControlPlaneIdCatalog"],
    aliasesFrom: "ControlPlaneIdCatalog",
  },
  {
    schema: "version-catalog.schema.json",
    output: "version.ts",
    exports: ["VersionCatalog"],
  },
  {
    schema: "policy-catalog.schema.json",
    output: "policy.ts",
    exports: ["PolicyCatalog"],
  },
  {
    schema: "organization-catalog.schema.json",
    output: "organization.ts",
    exports: ["OrganizationCatalog"],
  },
  {
    schema: "workspace-catalog.schema.json",
    output: "workspace.ts",
    exports: ["WorkspaceCatalog"],
  },
  {
    schema: "repository-catalog.schema.json",
    output: "repository.ts",
    exports: ["RepositoryCatalog"],
  },
  {
    schema: "user-catalog.schema.json",
    output: "user.ts",
    exports: ["UserCatalog"],
  },
  {
    schema: "auth-catalog.schema.json",
    output: "auth.ts",
    exports: ["AuthCatalog"],
  },
  {
    schema: "daemon-catalog.schema.json",
    output: "daemon.ts",
    exports: ["DaemonCatalog"],
  },
  {
    schema: "workload-catalog.schema.json",
    output: "workload.ts",
    exports: ["WorkloadCatalog"],
  },
  {
    schema: "rbac-catalog.schema.json",
    output: "rbac.ts",
    exports: ["RbacCatalog"],
  },
  {
    schema: "sync-catalog.schema.json",
    output: "sync.ts",
    exports: ["SyncCatalog"],
  },
  {
    schema: "audit-catalog.schema.json",
    output: "audit.ts",
    exports: ["AuditCatalog"],
  },
  {
    schema: "object-storage-catalog.schema.json",
    output: "storage.ts",
    exports: ["ObjectStorageCatalog"],
  },
  {
    schema: "stream-catalog.schema.json",
    output: "stream.ts",
    exports: ["StreamCatalog"],
  },
  {
    schema: "runner-catalog.schema.json",
    output: "runner.ts",
    exports: ["RunnerCatalog"],
  },
  {
    schema: "page-catalog.schema.json",
    output: "page.ts",
    exports: ["PageCatalog"],
  },
];

const jsonValueType =
  "type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };";

function outputDirectory() {
  const arguments_ = process.argv.slice(2);
  if (arguments_.length !== 2 || arguments_[0] !== "--output-dir") {
    throw new Error("usage: node scripts/generate.mjs --output-dir <directory>");
  }
  return isAbsolute(arguments_[1]) ? resolve(arguments_[1]) : resolve(packageRoot, arguments_[1]);
}

// Unlike `sdk/protocol`, the export list per target is NOT hand-maintained: every type the
// compiler produces is part of the wire contract and must reach a client. What the catalogs
// DO share is types (`PublicationClass` appears in six of them), so the same name would be
// exported from several modules and `export type *` in the barrel would collide. Ownership
// is therefore resolved automatically and deterministically: targets are processed in a
// fixed order and the first module to produce a name owns the export; later modules keep the
// declaration (they reference it) but drop the `export` keyword. Adding a type to a catalog
// in Rust needs no edit here.
function selectExports(source, requiredExports, claimed, filename) {
  const found = new Set();
  const rendered = source.replace(/^export (interface|type) ([A-Za-z_$][\w$]*)/gm, (match, kind, name) => {
    found.add(name);
    const owner = claimed.get(name);
    if (owner === undefined) {
      claimed.set(name, filename);
      return match;
    }
    return owner === filename ? match : `${kind} ${name}`;
  });
  const missing = requiredExports.filter((name) => !found.has(name));
  if (missing.length > 0) {
    throw new Error(`${filename} did not generate expected exports: ${missing.join(", ")}`);
  }
  return rendered;
}

const annotationKeys = new Set(["$comment", "default", "deprecated", "description", "examples", "readOnly", "title", "writeOnly"]);
const schemaMapKeys = new Set(["$defs", "definitions", "dependentSchemas", "patternProperties", "properties"]);
const schemaArrayKeys = new Set(["allOf", "anyOf", "oneOf"]);
const schemaValueKeys = new Set([
  "additionalItems",
  "additionalProperties",
  "contains",
  "else",
  "if",
  "items",
  "not",
  "propertyNames",
  "then",
  "unevaluatedItems",
  "unevaluatedProperties",
]);

function normalizeUnconstrainedSchemas(value) {
  if (value === true) return { tsType: "JsonValue" };
  if (value === false) return value;
  if (value === null || typeof value !== "object" || Array.isArray(value)) return value;
  const entries = Object.entries(value);
  if (entries.every(([key]) => annotationKeys.has(key))) return { tsType: "JsonValue" };
  return Object.fromEntries(
    entries.map(([key, child]) => {
      if (schemaMapKeys.has(key) && child !== null && typeof child === "object" && !Array.isArray(child)) {
        return [
          key,
          Object.fromEntries(Object.entries(child).map(([name, schema]) => [name, normalizeUnconstrainedSchemas(schema)])),
        ];
      }
      if (schemaArrayKeys.has(key) && Array.isArray(child)) {
        return [key, child.map(normalizeUnconstrainedSchemas)];
      }
      if (schemaValueKeys.has(key)) {
        if (key === "additionalProperties" && child === true && value.properties) {
          return [key, child];
        }
        return [
          key,
          Array.isArray(child) ? child.map(normalizeUnconstrainedSchemas) : normalizeUnconstrainedSchemas(child),
        ];
      }
      return [key, child];
    }),
  );
}

function typeName(property) {
  if (!/^[a-z][a-z0-9_]*$/.test(property)) throw new Error(`cannot derive a type name from ${property}`);
  return property
    .split("_")
    .map((part) => `${part[0].toUpperCase()}${part.slice(1)}`)
    .join("");
}

async function exists(path) {
  try {
    await stat(path);
    return true;
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
}

async function render(outputRoot) {
  await mkdir(outputRoot, { recursive: true });

  const claimed = new Map();
  for (const target of targets) {
    const schema = normalizeUnconstrainedSchemas(JSON.parse(await readFile(join(schemaRoot, target.schema), "utf8")));
    const source = await compile(schema, schema.title, {
      additionalProperties: false,
      bannerComment:
        "/**\n * Generated from the authoritative Rust protocol schema.\n * Do not edit by hand; run `npm run generate`.\n */",
      cwd: schemaRoot,
      declareExternallyReferenced: true,
      enableConstEnums: false,
      format: true,
      strictIndexSignatures: true,
      style: {
        bracketSpacing: true,
        printWidth: 120,
        semi: true,
        singleQuote: false,
        tabWidth: 2,
        trailingComma: "all",
        useTabs: false,
      },
      unknownAny: true,
      unreachableDefinitions: false,
    });
    let rendered = selectExports(source, target.exports, claimed, target.output);
    if (target.aliasesFrom) {
      // Identifier newtypes that schemars inlined rather than naming: recover a name for
      // each from the catalog's own property, so `UserId` exists as a type even when the
      // schema carried only `{"type":"string","format":"uuid"}`.
      const properties = Object.keys(schema.properties ?? {})
        .filter((property) => !claimed.has(typeName(property)))
        .sort();
      const aliases = properties
        .map(
          (property) =>
            `export type ${typeName(property)} = ${target.aliasesFrom}[${JSON.stringify(property)}];`,
        )
        .join("\n");
      for (const property of properties) claimed.set(typeName(property), target.output);
      rendered += `\n${aliases}\n\nexport ${jsonValueType}\n`;
    } else if (/\bJsonValue\b/.test(rendered)) {
      // Only when the module actually references it: this package typechecks with
      // `noUnusedLocals`, and a non-exported `JsonValue` nobody uses is an error.
      rendered += `\n${jsonValueType}\n`;
    }
    await writeFile(join(outputRoot, target.output), rendered.replaceAll("\r\n", "\n"));
  }

  const index = targets
    .map(({ output }) => `export type * from ${JSON.stringify(`./${output.replace(/\.ts$/, ".js")}`)};`)
    .join("\n");
  await writeFile(join(outputRoot, "index.ts"), `${index}\n`);
}

async function generate(outputRoot) {
  const outputExists = await exists(outputRoot);
  if (outputExists && outputRoot !== canonicalOutputRoot) {
    throw new Error(`refusing to replace an existing non-canonical output directory: ${outputRoot}`);
  }
  const parent = dirname(outputRoot);
  await mkdir(parent, { recursive: true });
  const staging = join(parent, `.${basename(outputRoot)}.tmp-${randomUUID()}`);
  const backup = join(parent, `.${basename(outputRoot)}.backup-${randomUUID()}`);
  let preserveBackup = false;
  try {
    await render(staging);
    if (outputExists) await rename(outputRoot, backup);
    try {
      await rename(staging, outputRoot);
    } catch (installError) {
      if (outputExists) {
        try {
          await rename(backup, outputRoot);
        } catch (rollbackError) {
          preserveBackup = true;
          throw new AggregateError(
            [installError, rollbackError],
            `failed to install generated output and restore the previous output; backup preserved at ${backup}`,
          );
        }
      }
      throw installError;
    }
    if (outputExists) await rm(backup, { recursive: true });
  } finally {
    await rm(staging, { recursive: true, force: true });
    if (!preserveBackup) await rm(backup, { recursive: true, force: true });
  }
}

await generate(outputDirectory());
