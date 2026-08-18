/**
 * The orphan guard.
 *
 * A `#[tauri::command]` that is missing from `generate_handler!` is
 * unreachable from the webview. A React component that nothing imports is
 * unreachable in exactly the same way, one layer up — and that is not
 * hypothetical: `ModelPicker`, `ProviderPicker`, `ApiKeys` and `ModePicker`
 * shipped fully built, with registered handlers and genuine `models.toml`
 * reads, and were imported by NOTHING. An audit found it; the type checker
 * could not, because a file that compiles in isolation compiles.
 *
 * So this file makes the defect class impossible to reship. It parses the
 * import graph of `src/` off disk and fails when a component is not wired to
 * the application entry point. It is deliberately blunt: no allowlist, no
 * "except this one". A component that genuinely should not be mounted should
 * not be in `src/components/`.
 *
 * Test-only importers do NOT count. A component reachable solely from its own
 * spec is precisely the defect being guarded against.
 */
import { describe, expect, it } from "vitest";
import fs from "node:fs";
import path from "node:path";

/**
 * The `apps/desktop` root.
 *
 * Resolved by walking up from the working directory rather than from
 * `import.meta.url`: the suite runs in jsdom, where `import.meta.url` is an
 * `http:` URL and `fileURLToPath` throws.
 */
function desktopRoot(): string {
  let dir = process.cwd();
  for (;;) {
    if (fs.existsSync(path.join(dir, "src", "main.tsx"))) {
      return dir;
    }
    const parent = path.dirname(dir);
    if (parent === dir) {
      throw new Error(`no apps/desktop root above ${process.cwd()}`);
    }
    dir = parent;
  }
}

const SRC_DIR = path.join(desktopRoot(), "src");
/** The real Vite entry point — what the shipped bundle actually starts from. */
const ENTRY = path.join(SRC_DIR, "main.tsx");
const COMPONENTS_DIR = path.join(SRC_DIR, "components");

/**
 * Every `from "…"`, side-effect `import "…"`, and dynamic `import("…")`.
 *
 * Type-only imports count: a component whose only tie to the app is a
 * `import type` is still not rendered anywhere, and this test is about
 * reachability, not about whether the bundler keeps the bytes.
 */
const SPECIFIER = /(?:\bfrom|\bimport)\s*\(?\s*["']([^"']+)["']/g;

function walk(dir: string, out: string[] = []): string[] {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      walk(full, out);
    } else if (/\.tsx?$/.test(entry.name) && !/\.d\.ts$/.test(entry.name)) {
      out.push(full);
    }
  }
  return out;
}

/**
 * Resolve a relative specifier to a file on disk.
 *
 * This codebase writes both `"./surfaceChrome.js"` (NodeNext style, resolved
 * to the `.tsx` beside it) and extensionless `"./localConfig"`, so both have
 * to resolve or the graph would show phantom orphans.
 */
function resolveSpecifier(fromFile: string, specifier: string): string | null {
  if (!specifier.startsWith(".")) {
    return null;
  }
  const base = path.resolve(path.dirname(fromFile), specifier);
  const candidates: string[] = [];
  if (/\.jsx?$/.test(base)) {
    candidates.push(base.replace(/\.jsx?$/, ".ts"), base.replace(/\.jsx?$/, ".tsx"));
  }
  candidates.push(
    base,
    `${base}.ts`,
    `${base}.tsx`,
    path.join(base, "index.ts"),
    path.join(base, "index.tsx"),
  );
  for (const candidate of candidates) {
    if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) {
      return candidate;
    }
  }
  return null;
}

interface Graph {
  files: string[];
  /** file → the files it imports. */
  imports: Map<string, Set<string>>;
  /** file → the files that import it, excluding itself. */
  importers: Map<string, string[]>;
  /** Relative specifiers that pointed at nothing. */
  unresolved: Array<{ file: string; specifier: string }>;
}

function buildGraph(): Graph {
  const files = walk(SRC_DIR);
  const imports = new Map<string, Set<string>>();
  const importers = new Map<string, string[]>(files.map((file) => [file, []]));
  const unresolved: Array<{ file: string; specifier: string }> = [];

  for (const file of files) {
    const source = fs.readFileSync(file, "utf8");
    const deps = new Set<string>();
    for (const match of source.matchAll(SPECIFIER)) {
      const specifier = match[1];
      const resolved = resolveSpecifier(file, specifier);
      if (resolved) {
        deps.add(resolved);
      } else if (specifier.startsWith(".")) {
        unresolved.push({ file: path.relative(SRC_DIR, file), specifier });
      }
    }
    imports.set(file, deps);
  }

  for (const [file, deps] of imports) {
    for (const dep of deps) {
      if (dep !== file) {
        importers.get(dep)?.push(file);
      }
    }
  }

  return { files, imports, importers, unresolved };
}

function componentFiles(graph: Graph): string[] {
  return graph.files.filter((file) => file.startsWith(COMPONENTS_DIR + path.sep));
}

function relative(file: string): string {
  return path.relative(SRC_DIR, file);
}

describe("the component import graph", () => {
  const graph = buildGraph();

  it("resolves every relative import in src/, so the graph is not lying", () => {
    // A specifier this test cannot resolve would silently drop an edge, and a
    // dropped edge reads as an orphan. Fail loudly instead of guessing.
    expect(graph.unresolved).toEqual([]);
  });

  it("finds the entry point and at least one component to check", () => {
    expect(fs.existsSync(ENTRY)).toBe(true);
    expect(componentFiles(graph).length).toBeGreaterThan(0);
  });

  it("has an importer outside itself for every file in src/components/", () => {
    const orphans = componentFiles(graph)
      .filter((file) => (graph.importers.get(file) ?? []).length === 0)
      .map(relative);
    expect(
      orphans,
      `these files in src/components/ are imported by nothing in src/. A component ` +
        `nothing imports is dead code the user can never reach — mount it in App.tsx ` +
        `(and add its view to NAV_GROUPS in Navigation.tsx), or delete it.`,
    ).toEqual([]);
  });

  it("reaches every file in src/components/ from src/main.tsx", () => {
    // Strictly stronger than the previous case: a pair of components that
    // import only each other each have an importer, and are still both dead.
    const reached = new Set<string>();
    const stack = [ENTRY];
    while (stack.length > 0) {
      const current = stack.pop() as string;
      if (reached.has(current)) {
        continue;
      }
      reached.add(current);
      for (const dep of graph.imports.get(current) ?? []) {
        stack.push(dep);
      }
    }
    const unreachable = componentFiles(graph)
      .filter((file) => !reached.has(file))
      .map(relative);
    expect(
      unreachable,
      `these files in src/components/ are not reachable from src/main.tsx. They are ` +
        `not in the shipped app at all, whatever imports them.`,
    ).toEqual([]);
  });
});
