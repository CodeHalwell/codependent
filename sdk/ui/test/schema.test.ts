import { readFile } from "node:fs/promises";
import { describe, expect, it } from "vitest";
import Ajv2020 from "ajv/dist/2020.js";
import { REMOTE_UI_SCHEMA_URL } from "../src/schema.js";
import { assertUiWireMessage } from "../src/worker/wire.js";
import { validateDocument } from "../src/validation.js";

const documentFixtureUrl = new URL("./fixtures/ui-document.json", import.meta.url);
const interactiveFixtureUrl = new URL("./fixtures/ui-interactive-document.json", import.meta.url);

async function validator() {
  const schema = JSON.parse(await readFile(REMOTE_UI_SCHEMA_URL, "utf8")) as object;
  return new Ajv2020({ allErrors: true, strict: true }).compile(schema);
}

describe("canonical Remote UI JSON Schema", () => {
  it("accepts the cross-language Rust/TypeScript golden document and snapshot", async () => {
    const validate = await validator();
    const document = JSON.parse(await readFile(documentFixtureUrl, "utf8")) as object;
    expect(validate(document), JSON.stringify(validate.errors)).toBe(true);
    const snapshot = { type: "snapshot", messageId: "schema-snapshot", snapshot: { document, reason: "reconnect" }, extensions: { generation: 2 } };
    expect(validate(snapshot), JSON.stringify(validate.errors)).toBe(true);
    expect(() => assertUiWireMessage(snapshot, "worker-to-host")).not.toThrow();
  });

  it("keeps interactive primitive props flat and lossless", async () => {
    const validate = await validator();
    const document = JSON.parse(await readFile(interactiveFixtureUrl, "utf8")) as { root: { children: Array<{ props: Record<string, unknown> }> } };
    expect(validate(document), JSON.stringify(validate.errors)).toBe(true);
    expect(document.root.children[0]?.props).toEqual({ value: "Remote UI", role: "heading" });
    expect(document.root.children[1]?.props).toEqual({ source: "Use **semantic** controls." });
    expect(document.root.children[2]?.props).toMatchObject({ value: "status", changeAction: "query.change" });
    expect(document.root.children[3]?.props.items).toBeInstanceOf(Array);
    expect(document.root.children[4]?.props).toMatchObject({ action: "results.refresh", disabled: true });
    expect(document.root.children[5]?.props).toEqual({ name: "localQuery", value: "", eventHandlers: ["change", "focus"] });
  });

  it("accepts patches, mediation, contributions, themes, and control messages", async () => {
    const validate = await validator();
    const fixtures = [
      {
        type: "patchBatch", messageId: "patch-1", extensions: { traceId: "trace-1" },
        patchBatch: {
          protocolVersion: { major: 1, minor: 0 }, documentId: "document", baseRevision: 2, revision: 3,
          patches: [{ op: "futurePatch", nodeId: "root", payload: { enabled: true } }],
          atomic: true,
          fallback: { plainText: "Updated content unavailable", replacement: { kind: "text", id: "fallback", text: "Updated" } },
        },
      },
      { type: "subscription", messageId: "sub-1", subscription: { subscriptionId: "subscription-1", kind: "artifact", resourceId: "artifact-1", parameters: { view: "summary" } } },
      { type: "unsubscribe", messageId: "unsub-1", unsubscription: { subscriptionId: "subscription-1" } },
      { type: "projection", messageId: "projection-1", projection: { subscriptionId: "subscription-1", revision: 4, value: { id: "artifact-1" } } },
      { type: "action", messageId: "action-1", action: { invocationId: "invocation-1", documentId: "document", revision: 3, sourceNodeId: "root", actionId: "refresh", payload: null } },
      { type: "actionResult", messageId: "result-1", actionResult: { invocationId: "invocation-1", status: "succeeded", value: { refreshed: true } } },
      {
        type: "contributions", messageId: "contributions-1", extensions: { contributionOwner: "acme" }, contributions: [{
          id: "acme.panel", extensionId: "acme", point: "panel", slot: "sidebar.secondary", documentId: "document",
          priority: 10, requires: ["artifact-read"], metadata: { mediaType: "application/acme+json" },
        }],
      },
      { type: "theme", messageId: "theme-1", theme: { id: "dark", name: "Dark", revision: 3, colorScheme: "dark", highContrast: false, reducedMotion: false, tokens: { accent: "#44aaff" } } },
      { type: "worker.ready", messageId: "ready-1", extensions: { control: { protocolVersion: { major: 1, minor: 0 } } } },
      { type: "contributions", messageId: "contributions-empty", extensions: { contributionOwner: "acme" }, contributions: [] },
    ];
    for (const fixture of fixtures) expect(validate(fixture), `${JSON.stringify(fixture)}\n${JSON.stringify(validate.errors)}`).toBe(true);
    expect(() => assertUiWireMessage(
      { type: "unsubscribe", messageId: "unsub-runtime", unsubscription: { subscriptionId: "subscription-1" } },
      "worker-to-host",
    )).not.toThrow();
  });

  it("rejects missing, mismatched, and multiple typed payloads", async () => {
    const validate = await validator();
    const invalid = [
      { type: "snapshot", messageId: "empty" },
      { type: "snapshot", messageId: "wrong", event: {} },
      { type: "projection", messageId: "smuggled", projection: { subscriptionId: "s" }, cancellation: { invocationId: "i" } },
      { type: "projection", messageId: "removed-value", projection: { subscriptionId: "s", removed: true, value: { stale: true } } },
      { type: "actionResult", messageId: "failed-without-error", actionResult: { invocationId: "i", status: "failed" } },
      { type: "capabilities", messageId: "bad-density", capabilities: { ...structuredClone({
        client: "test", protocolVersions: [{ major: 1, minor: 0 }], daemon: { rich_text: false, image_display: false, audio_capture: false, editor_mutations: false, diff_view: false, mouse: false, unicode: false, true_color: false },
        primitives: [], media: [], colorDepth: "monochrome", keyboard: true, screenReader: false, reducedMotion: true, clipboard: false, viewport: { width: 80, height: 24, density: -1 },
      }) } },
    ];
    for (const fixture of invalid) {
      expect(validate(fixture)).toBe(false);
      expect(() => assertUiWireMessage(fixture, "handshake")).toThrow();
    }
    expect(() => assertUiWireMessage({
      type: "contributions", messageId: "owner-mismatch", extensions: { contributionOwner: "acme" },
      contributions: [{ id: "x", extensionId: "other", point: "panel", slot: "panel", documentId: "d" }],
    }, "worker-to-host")).toThrow("contributionOwner");
  });

  it("rejects secret entry props and non-atomic patch batches", async () => {
    const validate = await validator();
    const document = {
      protocolVersion: { major: 1, minor: 0 }, documentId: "secret", revision: 0,
      root: { kind: "element", id: "credential", type: "TextInput", props: { name: "credential", inputType: "password" }, children: [] },
    };
    expect(validate(document)).toBe(false);
    expect(() => assertUiWireMessage({ type: "snapshot", messageId: "secret", snapshot: { document } }, "worker-to-host")).toThrow("Secret entry is host-owned");

    const patch = {
      type: "patchBatch", messageId: "not-atomic",
      patchBatch: { protocolVersion: { major: 1, minor: 0 }, documentId: "document", baseRevision: 0, revision: 1, patches: [{ op: "setText", nodeId: "text", text: "changed" }], atomic: false },
    };
    expect(validate(patch)).toBe(false);
    expect(() => assertUiWireMessage(patch, "worker-to-host")).toThrow("must be atomic");
  });

  it("enforces negotiated property, action, and JSON value limits", () => {
    const base = {
      protocolVersion: { major: 1, minor: 0 }, documentId: "bounded", revision: 0,
      root: { kind: "element" as const, id: "root", type: "Button", props: { label: "Run", action: "run", eventHandlers: ["focus"] }, children: [] },
    };
    expect(validateDocument(base, { maxPropertiesPerNode: 1 }).valid).toBe(false);
    expect(validateDocument(base, { maxActionsPerNode: 1 }).valid).toBe(false);
    expect(validateDocument(base, { maxJsonValues: 2 }).valid).toBe(false);
    expect(validateDocument(base, { maxPropertiesPerNode: 8, maxActionsPerNode: 8, maxJsonValues: 32 }).valid).toBe(true);
  });
});
