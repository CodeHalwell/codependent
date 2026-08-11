import { describe, expect, it } from "vitest";
import {
  Button,
  HotReloadStateStore,
  Image,
  MINIMAL_TERMINAL_CAPABILITIES,
  Stack,
  TextInput,
  UI_PROTOCOL_VERSION,
  auditAccessibility,
  createDocument,
  negotiateCapabilities,
  toAccessibleText,
  validatePatchBatch,
  validateDocument,
} from "../src/index.js";

describe("platform helpers", () => {
  it("audits inaccessible controls and produces a text projection", () => {
    const root = Stack({
      id: "root",
      accessibleLabel: "Evidence",
      children: [Button({ id: "unlabelled", action: "run" }), Image({ id: "image", src: "artifact://image", alt: "Architecture diagram" })],
    });
    expect(auditAccessibility(root).some((issue) => issue.code === "missingLabel")).toBe(true);
    expect(toAccessibleText(root)).toContain("Architecture diagram");
  });

  it("preserves JSON state through hot reload with functional updates", () => {
    const store = new HotReloadStateStore();
    store.set<number>("count", (current) => current + 1, 0);
    expect(store.get("count", 0)).toBe(1);
    const snapshot = store.snapshot();
    const replacement = new HotReloadStateStore();
    if (snapshot.type === "state") replacement.apply(snapshot);
    expect(replacement.get("count", 0)).toBe(1);
  });

  it("rejects stale patch batches", () => {
    const document = createDocument(Stack({ id: "root" }), { documentId: "stale", revision: 2 });
    const result = validatePatchBatch({
      protocolVersion: UI_PROTOCOL_VERSION,
      documentId: document.documentId,
      baseRevision: 1,
      revision: 2,
      patches: [],
      atomic: true,
    }, document.revision);
    expect(result).toMatchObject({ valid: false, issues: expect.arrayContaining([expect.objectContaining({ code: "staleRevision" })]) });
  });

  it("rejects empty and skipped-revision patch batches", () => {
    const result = validatePatchBatch({
      protocolVersion: UI_PROTOCOL_VERSION,
      documentId: "skipped",
      baseRevision: 4,
      revision: 6,
      patches: [],
      atomic: true,
    });
    expect(result).toMatchObject({
      valid: false,
      issues: expect.arrayContaining([
        expect.objectContaining({ path: "revision", code: "schema" }),
        expect.objectContaining({ path: "patches", code: "schema" }),
      ]),
    });
  });

  it("rejects secret inputs and deeply nested props without overflowing", () => {
    const input = TextInput({ id: "credential", name: "credential" });
    const secret = createDocument({ ...input, props: { ...input.props, inputType: "password", value: "do-not-send" } }, { documentId: "secret" });
    expect(validateDocument(secret)).toMatchObject({
      valid: false,
      issues: expect.arrayContaining([expect.objectContaining({ code: "unsafeValue", path: "root.props.inputType" })]),
    });

    let deeplyNested: unknown = "leaf";
    for (let depth = 0; depth < 10_000; depth += 1) deeplyNested = { child: deeplyNested };
    const root = Stack({ id: "root" });
    const deepDocument = createDocument({ ...root, props: { ...root.props, data: deeplyNested as never } }, { documentId: "deep" });
    expect(() => validateDocument(deepDocument)).not.toThrow();
    expect(validateDocument(deepDocument).valid).toBe(false);
  });

  it("requires explicit atomic patch batches", () => {
    const result = validatePatchBatch({
      protocolVersion: UI_PROTOCOL_VERSION,
      documentId: "not-atomic",
      baseRevision: 0,
      revision: 1,
      patches: [{ op: "setText", nodeId: "message", text: "updated" }],
      atomic: false as never,
    });
    expect(result.issues).toEqual(expect.arrayContaining([expect.objectContaining({ path: "atomic", code: "schema" })]));
  });

  it("negotiates additive minor versions down to the highest shared contract", () => {
    const local = {
      ...MINIMAL_TERMINAL_CAPABILITIES,
      protocolVersions: [{ major: 1, minor: 2 }],
    };
    const remote = {
      ...MINIMAL_TERMINAL_CAPABILITIES,
      protocolVersions: [{ major: 1, minor: 1 }],
    };
    expect(negotiateCapabilities(local, remote).protocolVersions).toEqual([
      { major: 1, minor: 1 },
    ]);
  });

  it("negotiates color depth and assistive capabilities by intersection", () => {
    const local = { ...MINIMAL_TERMINAL_CAPABILITIES, colorDepth: "trueColor" as const, screenReader: true };
    const remote = { ...MINIMAL_TERMINAL_CAPABILITIES, colorDepth: "ansi256" as const, screenReader: false };
    const selected = negotiateCapabilities(local, remote);
    expect(selected.colorDepth).toBe("ansi256");
    expect(selected.screenReader).toBe(false);
  });

  it("fails negotiation when no protocol major is shared", () => {
    const incompatible = { ...MINIMAL_TERMINAL_CAPABILITIES, protocolVersions: [{ major: 2, minor: 0 }] };
    expect(() => negotiateCapabilities(MINIMAL_TERMINAL_CAPABILITIES, incompatible)).toThrow("No mutually supported");
  });
});
