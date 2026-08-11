import { UI_PROTOCOL_VERSION, type UiDocument, type UiDocumentMetadata, type UiNode, type UiPatch, type UiPatchBatch, type UiProps } from "./protocol.js";

export interface CreateDocumentOptions {
  documentId: string;
  revision?: number;
  idPrefix?: string;
  metadata?: UiDocumentMetadata;
}

function materializeNode(node: UiNode, path: readonly number[], prefix: string): UiNode {
  const id = node.id ?? `${prefix}-${path.join("-") || "root"}`;
  if (node.kind === "text") return { ...node, id };
  return {
    ...node,
    id,
    children: node.children.map((child, index) => materializeNode(child, [...path, index], prefix)),
    ...(node.fallback === undefined ? {} : { fallback: materializeNode(node.fallback, [...path, -1], prefix) }),
  };
}

/** Assigns stable path IDs to anonymous nodes, making the result wire-ready. */
export function createDocument(root: UiNode, options: CreateDocumentOptions): UiDocument {
  return {
    protocolVersion: UI_PROTOCOL_VERSION,
    documentId: options.documentId,
    revision: options.revision ?? 0,
    root: materializeNode(root, [], options.idPrefix ?? "ui"),
    ...(options.metadata === undefined ? {} : { metadata: options.metadata }),
  };
}

function equalJson(left: unknown, right: unknown): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

function changedProps(before: UiProps, after: UiProps): { set: UiProps; unset: string[] } {
  const set: Record<string, UiProps[string]> = {};
  const unset: string[] = [];
  for (const [key, value] of Object.entries(after)) {
    if (!equalJson(before[key], value)) set[key] = value;
  }
  for (const key of Object.keys(before)) {
    if (!(key in after)) unset.push(key);
  }
  return { set, unset };
}

function diffNode(before: UiNode, after: UiNode, patches: UiPatch[]): void {
  if (before.id !== after.id || before.kind !== after.kind) {
    if (before.id === undefined) patches.push({ op: "replaceRoot", node: after });
    else patches.push({ op: "replace", nodeId: before.id, node: after });
    return;
  }
  if (before.kind === "text" && after.kind === "text") {
    if (before.text !== after.text && before.id !== undefined) patches.push({ op: "setText", nodeId: before.id, text: after.text });
    return;
  }
  if (before.kind !== "element" || after.kind !== "element") return;
  if (before.type !== after.type || !equalJson(before.requires, after.requires) || !equalJson(before.fallback, after.fallback)) {
    if (before.id !== undefined) patches.push({ op: "replace", nodeId: before.id, node: after });
    return;
  }
  const props = changedProps(before.props, after.props);
  if ((Object.keys(props.set).length > 0 || props.unset.length > 0) && before.id !== undefined) {
    patches.push({ op: "updateProps", nodeId: before.id, set: props.set, ...(props.unset.length === 0 ? {} : { unset: props.unset }) });
  }

  // Reconcile keyed children. A move is cheaper than a replace when stable IDs survive reordering.
  const oldById = new Map(before.children.map((child, index) => [child.id, { child, index }]));
  const newIds = new Set(after.children.map((child) => child.id));
  for (const child of before.children) {
    if (child.id !== undefined && !newIds.has(child.id)) patches.push({ op: "remove", nodeId: child.id });
  }
  after.children.forEach((child, index) => {
    const previous = oldById.get(child.id);
    if (previous === undefined) {
      if (before.id !== undefined) patches.push({ op: "insert", parentId: before.id, index, node: child });
      return;
    }
    if (previous.index !== index && child.id !== undefined && before.id !== undefined) {
      patches.push({ op: "move", nodeId: child.id, parentId: before.id, index });
    }
    diffNode(previous.child, child, patches);
  });
}

export function diffDocuments(before: UiDocument, after: UiDocument): UiPatchBatch {
  if (before.documentId !== after.documentId) throw new Error("Cannot diff documents with different document IDs");
  const patches: UiPatch[] = [];
  diffNode(before.root, after.root, patches);
  return {
    protocolVersion: UI_PROTOCOL_VERSION,
    documentId: after.documentId,
    baseRevision: before.revision,
    revision: after.revision,
    patches,
    atomic: true,
  };
}
