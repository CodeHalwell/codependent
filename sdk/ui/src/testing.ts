import { createDocument, type CreateDocumentOptions } from "./document.js";
import { UI_PROTOCOL_VERSION, type UiDocument, type UiEvent, type UiJsonValue, type UiNode, type UiPatch, type UiPatchBatch } from "./protocol.js";
import { assertValidDocument } from "./validation.js";

function clone<T>(value: T): T { return structuredClone(value); }

function stable(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(stable);
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(Object.entries(value as Record<string, unknown>).sort(([left], [right]) => left.localeCompare(right)).map(([key, entry]) => [key, stable(entry)]));
  }
  return value;
}

export function stableUiJson(value: unknown): string { return JSON.stringify(stable(value), null, 2); }

export function findNode(root: UiNode, id: string): UiNode | undefined {
  if (root.id === id) return root;
  if (root.kind === "text") return undefined;
  for (const child of root.children) {
    const found = findNode(child, id);
    if (found !== undefined) return found;
  }
  return root.fallback === undefined ? undefined : findNode(root.fallback, id);
}

function updateTree(root: UiNode, nodeId: string, update: (node: UiNode) => UiNode | undefined): UiNode | undefined {
  if (root.id === nodeId) return update(root);
  if (root.kind === "text") return root;
  const children = root.children.map((child) => updateTree(child, nodeId, update)).filter((child): child is UiNode => child !== undefined);
  const fallback = root.fallback === undefined ? undefined : updateTree(root.fallback, nodeId, update);
  const updated = { ...root, children };
  delete updated.fallback;
  if (fallback !== undefined) updated.fallback = fallback;
  return updated;
}

function applyPatch(root: UiNode, patch: UiPatch): UiNode {
  switch (patch.op) {
    case "replaceRoot": return clone(patch.node);
    case "remove": return updateTree(root, patch.nodeId, () => undefined) ?? root;
    case "replace": return updateTree(root, patch.nodeId, () => clone(patch.node)) ?? root;
    case "setText": return updateTree(root, patch.nodeId, (node) => node.kind === "text" ? { ...node, text: patch.text } : node) ?? root;
    case "updateProps": return updateTree(root, patch.nodeId, (node) => {
      if (node.kind === "text") return node;
      const props = { ...node.props, ...patch.set };
      patch.unset?.forEach((key) => delete props[key]);
      return { ...node, props };
    }) ?? root;
    case "insert": return updateTree(root, patch.parentId, (node) => {
      if (node.kind === "text") return node;
      const children = [...node.children];
      children.splice(patch.index, 0, clone(patch.node));
      return { ...node, children };
    }) ?? root;
    case "move": {
      const moving = findNode(root, patch.nodeId);
      if (moving === undefined) return root;
      const removed = updateTree(root, patch.nodeId, () => undefined) ?? root;
      return updateTree(removed, patch.parentId, (node) => {
        if (node.kind === "text") return node;
        const children = [...node.children];
        children.splice(patch.index, 0, moving);
        return { ...node, children };
      }) ?? removed;
    }
  }
}

export function applyPatchBatch(document: UiDocument, batch: UiPatchBatch): UiDocument {
  if (document.documentId !== batch.documentId) throw new Error("Patch document id does not match");
  if (document.revision !== batch.baseRevision) throw new Error(`Stale patch: expected ${document.revision}, received ${batch.baseRevision}`);
  return { ...document, revision: batch.revision, root: batch.patches.reduce(applyPatch, document.root) };
}

export class UiTestRenderer {
  #document: UiDocument;
  #eventSequence = 0;
  readonly events: UiEvent[] = [];

  constructor(root: UiNode, options: CreateDocumentOptions = { documentId: "test-document" }) {
    this.#document = createDocument(root, { idPrefix: "test", ...options });
    assertValidDocument(this.#document);
  }

  get document(): UiDocument { return clone(this.#document); }
  get root(): UiNode { return clone(this.#document.root); }
  find(id: string): UiNode | undefined { const node = findNode(this.#document.root, id); return node === undefined ? undefined : clone(node); }
  toJSON(): string { return stableUiJson(this.#document); }

  update(root: UiNode): UiDocument {
    this.#document = createDocument(root, {
      documentId: this.#document.documentId,
      revision: this.#document.revision + 1,
      idPrefix: "test",
      ...(this.#document.metadata === undefined ? {} : { metadata: this.#document.metadata }),
    });
    assertValidDocument(this.#document);
    return this.document;
  }

  apply(batch: UiPatchBatch): UiDocument {
    this.#document = applyPatchBatch(this.#document, batch);
    assertValidDocument(this.#document);
    return this.document;
  }

  dispatch<T extends UiJsonValue>(targetId: string, type: UiEvent["type"], payload?: T): UiEvent<T> {
    if (findNode(this.#document.root, targetId) === undefined) throw new Error(`Unknown target node: ${targetId}`);
    this.#eventSequence += 1;
    const event: UiEvent<T> = {
      protocolVersion: UI_PROTOCOL_VERSION,
      eventId: `test-event-${this.#eventSequence}`,
      documentId: this.#document.documentId,
      revision: this.#document.revision,
      targetId,
      type,
      ...(payload === undefined ? {} : { payload }),
    };
    this.events.push(event);
    return event;
  }
}

export function renderForTest(root: UiNode, options?: CreateDocumentOptions): UiTestRenderer {
  return new UiTestRenderer(root, options);
}
