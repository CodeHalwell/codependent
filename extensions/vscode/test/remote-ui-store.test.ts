import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it, vi } from "vitest";
import type { UiDocument, UiPatchBatch } from "@codypendent/ui";

import { createWebviewCapabilities } from "../src/webview/remote-ui/capabilities.js";
import { applyPatchBatch, RemoteUiStore } from "../src/webview/remote-ui/store.js";

const PLACEMENT = { point: "panel", extensionId: "test.plugin" } as const;

function golden(): UiDocument {
  return JSON.parse(readFileSync(resolve(process.cwd(), "../../sdk/ui/test/fixtures/ui-document.json"), "utf8")) as UiDocument;
}

function batch(document: UiDocument, patches: UiPatchBatch["patches"], revision = document.revision + 1): UiPatchBatch {
  return {
    protocolVersion: document.protocolVersion,
    documentId: document.documentId,
    baseRevision: document.revision,
    revision,
    patches,
    atomic: true,
  };
}

describe("RemoteUiStore", () => {
  it("loads the cross-language golden document and projects it for VS Code", () => {
    const store = new RemoteUiStore(createWebviewCapabilities());
    const document = golden();
    const listener = vi.fn();
    store.subscribe(listener);

    expect(store.apply({ type: "snapshot", document }, { point: "panel", extensionId: "test.plugin", slot: "primary", priority: 7 })).toMatchObject({ applied: true });
    const mount = store.getSnapshot().mounts[0];
    expect(mount.document).toEqual(document);
    expect(mount.projected.capabilities?.client).toBe("vscode");
    expect(mount.placement).toEqual({ point: "panel", extensionId: "test.plugin", slot: "primary", priority: 7 });
    expect(listener).toHaveBeenCalledOnce();
  });

  it("applies every incremental operation atomically", () => {
    let document = golden();
    document = applyPatchBatch(document, batch(document, [
      { op: "insert", parentId: "root", index: 1, node: { kind: "text", id: "status", text: "running" } },
      { op: "setText", nodeId: "heading", text: "Build complete" },
      { op: "updateProps", nodeId: "open", set: { label: "Inspect", tone: "positive" }, unset: ["payload"] },
    ]));
    expect(document.revision).toBe(8);
    expect(document.root.kind).toBe("element");
    if (document.root.kind !== "element") throw new Error("expected element root");
    expect(document.root.children.map((node) => node.id)).toEqual(["heading", "status", "open"]);

    document = applyPatchBatch(document, batch(document, [
      { op: "move", nodeId: "open", parentId: "root", index: 0 },
      { op: "replace", nodeId: "status", node: { kind: "element", id: "status-card", type: "Badge", props: { label: "Passed" }, children: [] } },
      { op: "remove", nodeId: "heading" },
    ]));
    if (document.root.kind !== "element") throw new Error("expected element root");
    expect(document.root.children.map((node) => node.id)).toEqual(["open", "status-card"]);

    document = applyPatchBatch(document, batch(document, [
      { op: "replaceRoot", node: { kind: "element", id: "new-root", type: "Text", props: { value: "replaced" }, children: [] } },
    ]));
    expect(document.root.id).toBe("new-root");
  });

  it("keeps the last good revision and requests resync after a failed batch", () => {
    const store = new RemoteUiStore(createWebviewCapabilities());
    const document = golden();
    store.apply({ type: "snapshot", document }, PLACEMENT);

    const result = store.apply({
      type: "patch",
      batch: batch(document, [
        { op: "setText", nodeId: "heading", text: "this would have applied" },
        { op: "remove", nodeId: "missing" },
      ]),
    });

    expect(result.applied).toBe(false);
    expect(result.resync).toMatchObject({ documentId: document.documentId, knownRevision: 7 });
    expect(store.getSnapshot().mounts[0].document).toEqual(document);
    expect(store.getSnapshot().errors[document.documentId]).toContain("Unknown node id");
  });

  it("rejects stale patches and stale snapshots", () => {
    const store = new RemoteUiStore(createWebviewCapabilities());
    const document = golden();
    store.apply({ type: "snapshot", document }, PLACEMENT);
    const stale = { ...document, revision: 6 };
    expect(store.apply({ type: "snapshot", document: stale }, PLACEMENT).applied).toBe(false);
    expect(store.apply({ type: "patch", batch: { ...batch(document, []), baseRevision: 5 } }).applied).toBe(false);
    expect(store.getSnapshot().mounts[0].document.revision).toBe(7);
  });

  it("rejects a different authoritative tree at the same revision", () => {
    const store = new RemoteUiStore(createWebviewCapabilities());
    const document = golden();
    expect(store.apply({ type: "snapshot", document }, PLACEMENT).applied).toBe(true);
    const conflicting = {
      ...document,
      root: { kind: "element", id: "replacement", type: "Button", props: { action: "privileged.command" }, children: [] } as UiDocument["root"],
    };
    const result = store.apply({ type: "snapshot", document: conflicting }, PLACEMENT);
    expect(result.applied).toBe(false);
    expect(result.resync).toMatchObject({ documentId: document.documentId, knownRevision: document.revision });
    expect(store.getSnapshot().mounts[0]?.document).toEqual(document);
  });

  it("rejects producer-owned secret entry before it reaches the DOM", () => {
    const store = new RemoteUiStore(createWebviewCapabilities());
    const document = golden();
    const secret = {
      ...document,
      documentId: "secret-document",
      root: {
        kind: "element",
        id: "credential",
        type: "TextInput",
        props: { name: "credential", inputType: "password", value: "" },
        children: [],
      } as UiDocument["root"],
    };
    const result = store.apply({ type: "snapshot", document: secret }, PLACEMENT);
    expect(result.applied).toBe(false);
    expect(store.getSnapshot().mounts).toHaveLength(0);
    expect(store.getSnapshot().errors["secret-document"]).toContain("Secret entry is host-owned");
  });

  it("sorts contribution mounts by priority and survives persistence recovery", () => {
    const store = new RemoteUiStore(createWebviewCapabilities());
    const first = golden();
    const second = { ...golden(), documentId: "second-document" };
    store.apply({ type: "snapshot", document: first }, { point: "panel", extensionId: "test.plugin", priority: 1 });
    store.apply({ type: "snapshot", document: second }, { point: "panel", extensionId: "test.plugin", priority: 10 });
    expect(store.getSnapshot().mounts.map((mount) => mount.document.documentId)).toEqual(["second-document", "golden-ui-document"]);
    store.setPlacement(first.documentId, { point: "panel", extensionId: "test.plugin", slot: "top", priority: 20 });
    expect(store.getSnapshot().mounts[0].placement).toEqual({ point: "panel", extensionId: "test.plugin", slot: "top", priority: 20 });

    const restored = new RemoteUiStore(createWebviewCapabilities());
    restored.restore(store.serialize());
    expect(restored.getSnapshot().mounts.map((mount) => mount.document.documentId)).toEqual(["golden-ui-document", "second-document"]);
  });

  it("keeps orphan snapshots hidden until a broker-attested placement exists", () => {
    const store = new RemoteUiStore(createWebviewCapabilities());
    const document = golden();
    expect(store.apply({ type: "snapshot", document }).applied).toBe(false);
    expect(store.getSnapshot().mounts).toHaveLength(0);
    expect(store.getSnapshot().errors[document.documentId]).toContain("broker-attested");

    expect(store.replaceContributions("test.plugin", [
      { documentId: document.documentId, placement: PLACEMENT },
    ])).toBe(true);
    expect(store.apply({ type: "snapshot", document }).applied).toBe(true);
    expect(store.getSnapshot().mounts).toHaveLength(1);
  });

  it("atomically replaces one owner without removing another owner's mounts", () => {
    const store = new RemoteUiStore(createWebviewCapabilities());
    const first = golden();
    const second = { ...golden(), documentId: "second-document" };
    const foreign = { ...golden(), documentId: "foreign-document" };
    const firstPlacement = { point: "panel", extensionId: "owner.a" } as const;
    const secondPlacement = { point: "panel", extensionId: "owner.a" } as const;
    const foreignPlacement = { point: "panel", extensionId: "owner.b" } as const;
    expect(store.replaceContributions("owner.a", [{ documentId: first.documentId, placement: firstPlacement }])).toBe(true);
    expect(store.replaceContributions("owner.b", [{ documentId: foreign.documentId, placement: foreignPlacement }])).toBe(true);
    store.apply({ type: "snapshot", document: first });
    store.apply({ type: "snapshot", document: foreign });

    expect(store.replaceContributions("owner.a", [{ documentId: second.documentId, placement: secondPlacement }])).toBe(true);
    store.apply({ type: "snapshot", document: second });
    expect(store.getSnapshot().mounts.map((mount) => mount.document.documentId).sort()).toEqual(["foreign-document", "second-document"]);

    expect(store.replaceContributions("owner.a", [{ documentId: foreign.documentId, placement: firstPlacement }])).toBe(false);
    expect(store.replaceContributions("owner.a", [])).toBe(true);
    expect(store.getSnapshot().mounts.map((mount) => mount.document.documentId)).toEqual(["foreign-document"]);
    expect(store.apply({ type: "snapshot", document: second }).applied).toBe(false);
    expect(store.getSnapshot().mounts.map((mount) => mount.document.documentId)).toEqual(["foreign-document"]);
  });

  it("does not confuse opaque producer scopes that share visible extension chrome", () => {
    const store = new RemoteUiStore(createWebviewCapabilities());
    const document = golden();
    const first = { point: "panel", extensionId: "acme.plugin", ownerScope: "ui-producer:first" } as const;
    const second = { point: "panel", extensionId: "acme.plugin", ownerScope: "ui-producer:second" } as const;
    expect(store.replaceContributions("ui-producer:first", [
      { documentId: document.documentId, placement: first },
    ])).toBe(true);
    expect(store.replaceContributions("ui-producer:second", [
      { documentId: document.documentId, placement: second },
    ])).toBe(false);
  });
});
