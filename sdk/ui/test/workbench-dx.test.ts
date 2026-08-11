import { describe, expect, it } from "vitest";
import { TransactionalHotReload } from "../src/hot-reload.js";
import { DEFAULT_UI_HARD_LIMITS, UI_PROTOCOL_VERSION } from "../src/protocol.js";
import { diagnoseDocument } from "../src/tooling/diagnostics.js";
import { UI_CONFORMANCE_STORY } from "../src/tooling/stories.js";
import { colorDepth, createWorkbenchCapabilities, DEFAULT_WORKBENCH_OPTIONS, formatNodeTree, loadWorkbenchFixture, viewport, workbenchTheme } from "../src/tooling/workbench.js";
import { DEFAULT_UI_LIMITS } from "../src/validation.js";

describe("component workbench DX", () => {
  it("keeps author validation limits aligned with the negotiated wire ceiling", () => {
    expect(DEFAULT_UI_LIMITS).toMatchObject({
      maxDepth: DEFAULT_UI_HARD_LIMITS.maxTreeDepth,
      maxNodes: DEFAULT_UI_HARD_LIMITS.maxNodes,
      maxTextBytes: DEFAULT_UI_HARD_LIMITS.maxTextBytes,
      maxPropsBytes: DEFAULT_UI_HARD_LIMITS.maxPatchBytes,
      maxPropertiesPerNode: DEFAULT_UI_HARD_LIMITS.maxPropertiesPerNode,
      maxActionsPerNode: DEFAULT_UI_HARD_LIMITS.maxActionsPerNode,
      maxJsonDepth: DEFAULT_UI_HARD_LIMITS.maxJsonDepth,
      maxJsonValues: DEFAULT_UI_HARD_LIMITS.maxJsonValues,
      maxPatchCount: DEFAULT_UI_HARD_LIMITS.maxPatchesPerBatch,
    });
  });

  it("reports ignored dimensions, tokens, missing fallbacks, and accessibility automatically", () => {
    const document = {
      protocolVersion: UI_PROTOCOL_VERSION,
      documentId: "diagnostic-story",
      revision: 1,
      root: {
        kind: "element" as const, id: "web", type: "WebOnly", props: { width: "10fr", gap: "xxl", foreground: "theme.unknown" }, children: [
          { kind: "element" as const, id: "button", type: "Button", props: { action: "run" }, children: [] },
        ],
      },
    };
    const codes = diagnoseDocument(document, "vscode").map((diagnostic) => diagnostic.code);
    expect(codes).toEqual(expect.arrayContaining([
      "missingLabel", "ignored-dimension", "ignored-spacing", "unknown-theme-token", "missing-terminal-fallback",
    ]));
  });

  it("uses the shared semantic story as an accessibility-clean golden", () => {
    if (UI_CONFORMANCE_STORY.document === undefined) throw new Error("shared conformance story must include a document");
    const diagnostics = diagnoseDocument(UI_CONFORMANCE_STORY.document, "vscode");
    expect(diagnostics.filter((diagnostic) => diagnostic.severity === "error")).toEqual([]);
    expect(formatNodeTree(UI_CONFORMANCE_STORY.document.root).join("\n")).toMatchInlineSnapshot(`
      "Stack#root props={\"gap\":\"md\",\"accessibleLabel\":\"Remote UI surface states\"}
        Text#heading props={\"value\":\"Surface states\",\"role\":\"heading\",\"weight\":\"bold\",\"accessibleLabel\":\"Surface states\"}
        Spinner#loading props={\"label\":\"Loading…\",\"accessibleLabel\":\"Loading\"}
        EmptyState#empty props={\"title\":\"No Results\",\"message\":\"Change the filters to see results.\",\"accessibleLabel\":\"No results\"}
        Alert#error props={\"tone\":\"critical\",\"title\":\"Could Not Load\",\"message\":\"Retry the request or disable this surface.\",\"accessibleLabel\":\"Could not load\"}
        Text#long props={\"value\":\"A-very-long-unbroken-extension-value-that-must-wrap-without-overflow-or-hiding-recovery-controls\",\"accessibleLabel\":\"Long content\"}
        Button#retry props={\"action\":\"conformance.retry\",\"label\":\"Retry Surface\",\"accessibleLabel\":\"Retry surface\"}"
    `);
  });

  it("preflights replacement generations transactionally and retains last-valid state", async () => {
    const reload = new TransactionalHotReload({ value: "v1", states: { count: 2 } }, 1);
    const failed = await reload.reload(async ({ states }) => {
      expect(states).toEqual({ count: 2 });
      throw new Error("candidate failed accessibility audit");
    });
    expect(failed).toMatchObject({ committed: false, generation: 2, value: "v1" });
    expect(reload.active).toBe("v1");
    expect(reload.generation).toBe(1);
    const committed = await reload.reload(async ({ generation, states }) => ({ value: `v${generation}`, states: { ...states, count: 3 } }));
    expect(committed).toMatchObject({ committed: true, generation: 2, value: "v2" });
    expect(reload.states).toEqual({ count: 3 });
  });

  it("builds honest target-specific capabilities and rejects malformed options", () => {
    const options = {
      ...DEFAULT_WORKBENCH_OPTIONS,
      target: "vscode" as const,
      point: "artifact-renderer",
      viewport: viewport("120x40"),
      colorDepth: colorDepth("ansi256"),
      theme: workbenchTheme("highContrast"),
    };
    expect(createWorkbenchCapabilities(options)).toMatchObject({
      client: "vscode",
      contributionPoints: ["artifact-renderer"],
      viewport: { width: 120, height: 40 },
      colorDepth: "ansi256",
      screenReader: true,
    });
    expect(() => viewport("tiny")).toThrow("WIDTHxHEIGHT");
    expect(() => colorDepth("millions")).toThrow("color-depth");
    expect(() => workbenchTheme("neon")).toThrow("theme");
  });

  it("loads inert projection, action, event, and state fixtures", async () => {
    const root = await mkdtemp(join(tmpdir(), "codypendent-workbench-fixture-"));
    const path = join(root, "story.json");
    await writeFile(path, JSON.stringify({
      id: "fixture",
      target: "vscode",
      point: "panel",
      projections: { "session:s1": { revision: 1, value: { id: "s1", state: "active" } } },
      actions: { retry: { status: "succeeded", value: { ok: true } } },
      events: [{ documentId: "main", targetId: "retry", type: "press" }],
      hotReloadState: { expanded: true },
    }));
    await expect(loadWorkbenchFixture(path)).resolves.toMatchObject({
      id: "fixture",
      projections: { "session:s1": { revision: 1 } },
      actions: { retry: { status: "succeeded" } },
      events: [{ documentId: "main", targetId: "retry", type: "press" }],
      hotReloadState: { expanded: true },
    });
  });
});
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
