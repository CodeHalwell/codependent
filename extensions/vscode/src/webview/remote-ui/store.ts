import {
  UI_PROTOCOL_VERSION,
  assertValidDocument,
  projectDocument,
  validatePatchBatch,
  type ContributionPoint,
  type UiCapabilities,
  type UiDocument,
  type UiHostMessage,
  type UiHardLimits,
  type UiNode,
  type UiPatch,
  type UiPatchBatch,
} from "@codypendent/ui";

export interface RemoteUiPlacement {
  point: ContributionPoint;
  /** Broker-attested producer identity; never sourced from document props. */
  extensionId?: string;
  /** Broker-attested opaque producer generation used only for replacement ownership. */
  ownerScope?: string;
  publisher?: string;
  trust?: string;
  slot?: string;
  priority?: number;
}

export interface MountedRemoteUi {
  document: UiDocument;
  projected: UiDocument;
  placement: RemoteUiPlacement;
}

export interface RemoteUiStoreSnapshot {
  mounts: readonly MountedRemoteUi[];
  errors: Readonly<Record<string, string>>;
}

export interface ApplyResult {
  applied: boolean;
  documentId?: string;
  resync?: { documentId: string; knownRevision?: number; reason: string };
}

const EMPTY_SNAPSHOT: RemoteUiStoreSnapshot = { mounts: [], errors: {} };

function canonicalJson(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value) ?? "null";
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  return `{${Object.entries(value as Record<string, unknown>)
    .filter(([, entry]) => entry !== undefined)
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([key, entry]) => `${JSON.stringify(key)}:${canonicalJson(entry)}`)
    .join(",")}}`;
}

function compareMounts(left: MountedRemoteUi, right: MountedRemoteUi): number {
  return (right.placement.priority ?? 0) - (left.placement.priority ?? 0)
    || left.document.documentId.localeCompare(right.document.documentId);
}

function mapNode(node: UiNode, targetId: string, transform: (node: UiNode) => UiNode | undefined): UiNode | undefined {
  if (node.id === targetId) return transform(node);
  if (node.kind === "text") return node;
  let changed = false;
  const children: UiNode[] = [];
  for (const child of node.children) {
    const next = mapNode(child, targetId, transform);
    if (next === undefined) changed = true;
    else {
      changed ||= next !== child;
      children.push(next);
    }
  }
  const fallback = node.fallback === undefined
    ? undefined
    : mapNode(node.fallback, targetId, transform);
  changed ||= fallback !== node.fallback;
  return changed
    ? {
        ...node,
        children,
        ...(fallback === undefined ? { fallback: undefined } : { fallback }),
      }
    : node;
}

function containsNode(node: UiNode, targetId: string): boolean {
  if (node.id === targetId) return true;
  return node.kind === "element"
    && (node.children.some((child) => containsNode(child, targetId))
      || (node.fallback !== undefined && containsNode(node.fallback, targetId)));
}

function requireNode(root: UiNode, targetId: string): UiNode {
  let found: UiNode | undefined;
  mapNode(root, targetId, (node) => {
    found = node;
    return node;
  });
  if (found === undefined) throw new Error(`Unknown node id: ${targetId}`);
  return found;
}

function replaceNode(root: UiNode, targetId: string, transform: (node: UiNode) => UiNode | undefined): UiNode {
  const existing = requireNode(root, targetId);
  if (existing === root) {
    const nextRoot = transform(existing);
    if (nextRoot === undefined) throw new Error("The document root cannot be removed");
    return nextRoot;
  }
  const next = mapNode(root, targetId, transform);
  if (next === undefined) throw new Error("The document root cannot be removed");
  return next;
}

function applyPatch(root: UiNode, patch: UiPatch): UiNode {
  switch (patch.op) {
    case "replaceRoot":
      return patch.node;
    case "insert":
      return replaceNode(root, patch.parentId, (parent) => {
        if (parent.kind !== "element") throw new Error(`Parent ${patch.parentId} is not an element`);
        if (patch.index < 0 || patch.index > parent.children.length) {
          throw new Error(`Insert index ${patch.index} is outside parent ${patch.parentId}`);
        }
        const children = [...parent.children];
        children.splice(patch.index, 0, patch.node);
        return { ...parent, children };
      });
    case "remove":
      return replaceNode(root, patch.nodeId, () => undefined);
    case "replace":
      return replaceNode(root, patch.nodeId, () => patch.node);
    case "updateProps":
      return replaceNode(root, patch.nodeId, (node) => {
        if (node.kind !== "element") throw new Error(`Node ${patch.nodeId} has no props`);
        const props: Record<string, typeof node.props[string]> = { ...node.props, ...patch.set };
        for (const key of patch.unset ?? []) delete props[key];
        return { ...node, props };
      });
    case "setText":
      return replaceNode(root, patch.nodeId, (node) => {
        if (node.kind !== "text") throw new Error(`Node ${patch.nodeId} is not text`);
        return { ...node, text: patch.text };
      });
    case "move": {
      const moved = requireNode(root, patch.nodeId);
      if (moved === root) throw new Error("The document root cannot be moved");
      if (containsNode(moved, patch.parentId)) throw new Error("A node cannot move into itself or its descendants");
      const without = replaceNode(root, patch.nodeId, () => undefined);
      return applyPatch(without, { op: "insert", parentId: patch.parentId, index: patch.index, node: moved });
    }
  }
}

export function applyPatchBatch(document: UiDocument, batch: UiPatchBatch, limits?: UiHardLimits): UiDocument {
  if (batch.documentId !== document.documentId) throw new Error("Patch document id does not match mounted document");
  if (batch.protocolVersion.major !== UI_PROTOCOL_VERSION.major) throw new Error("Unsupported Remote UI protocol major version");
  const validation = validatePatchBatch(batch, document.revision, {
    ...(limits === undefined ? {} : {
      maxDepth: limits.maxTreeDepth,
      maxNodes: limits.maxNodes,
      maxTextBytes: limits.maxTextBytes,
      maxPatchCount: limits.maxPatchesPerBatch,
      maxDocumentBytes: limits.maxPatchBytes * 2,
    }),
  });
  if (!validation.valid) throw new Error(validation.issues.map((issue) => issue.message).join("; "));
  let root = document.root;
  for (const patch of batch.patches) root = applyPatch(root, patch);
  const next = { ...document, protocolVersion: batch.protocolVersion, revision: batch.revision, root };
  assertValidDocument(next);
  return next;
}

/**
 * External-store boundary used by React and tests. Each host message is applied
 * atomically; a failed patch leaves the previous revision visible and asks the
 * producer for an authoritative snapshot.
 */
export class RemoteUiStore {
  readonly #mounts = new Map<string, MountedRemoteUi>();
  readonly #errors = new Map<string, string>();
  readonly #placements = new Map<string, RemoteUiPlacement>();
  readonly #listeners = new Set<() => void>();
  #capabilities: UiCapabilities;
  #snapshot: RemoteUiStoreSnapshot = EMPTY_SNAPSHOT;

  constructor(capabilities: UiCapabilities) {
    this.#capabilities = capabilities;
  }

  getSnapshot = (): RemoteUiStoreSnapshot => this.#snapshot;

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  setCapabilities(capabilities: UiCapabilities): void {
    this.#capabilities = capabilities;
    for (const [documentId, mount] of this.#mounts) {
      this.#mounts.set(documentId, {
        ...mount,
        projected: projectDocument(mount.document, capabilities),
      });
    }
    this.#publish();
  }

  apply(message: UiHostMessage, placement?: RemoteUiPlacement): ApplyResult {
    try {
      switch (message.type) {
        case "snapshot": {
          const document = message.document;
          if (document.protocolVersion.major !== UI_PROTOCOL_VERSION.major) {
            throw new Error(`Unsupported Remote UI protocol ${document.protocolVersion.major}.${document.protocolVersion.minor}`);
          }
          assertValidDocument(document);
          const current = this.#mounts.get(document.documentId);
          if (current !== undefined && document.revision < current.document.revision) {
            throw new Error(`Snapshot revision ${document.revision} is older than ${current.document.revision}`);
          }
          if (current !== undefined
            && document.revision === current.document.revision
            && canonicalJson(document) !== canonicalJson(current.document)) {
            throw new Error(`Snapshot revision ${document.revision} conflicts with the mounted document`);
          }
          const attestedPlacement = placement ?? current?.placement ?? this.#placements.get(document.documentId);
          if (attestedPlacement === undefined || typeof attestedPlacement.extensionId !== "string" || attestedPlacement.extensionId.length === 0) {
            throw new Error("Snapshot has no broker-attested extension placement");
          }
          if (this.#capabilities.contributionPoints?.includes(attestedPlacement.point) !== true) {
            throw new Error(`Contribution point ${attestedPlacement.point} is not mounted by this client`);
          }
          this.#mounts.set(document.documentId, {
            document,
            projected: projectDocument(document, this.#capabilities),
            placement: attestedPlacement,
          });
          if (placement !== undefined) this.#placements.set(document.documentId, placement);
          this.#errors.delete(document.documentId);
          this.#publish();
          return { applied: true, documentId: document.documentId };
        }
        case "patch": {
          const documentId = message.batch.documentId;
          const current = this.#mounts.get(documentId);
          if (current === undefined) {
            return this.#reject(documentId, undefined, "Patch arrived before a snapshot");
          }
          const document = applyPatchBatch(current.document, message.batch, this.#capabilities.limits);
          this.#mounts.set(documentId, {
            document,
            projected: projectDocument(document, this.#capabilities),
            placement: placement ?? current.placement,
          });
          if (placement !== undefined) this.#placements.set(documentId, placement);
          this.#errors.delete(documentId);
          this.#publish();
          return { applied: true, documentId };
        }
        case "dispose": {
          const current = this.#mounts.get(message.documentId);
          if (current !== undefined && message.revision !== current.document.revision) {
            return this.#reject(message.documentId, current.document.revision, "Dispose revision did not match the mounted document");
          }
          const deleted = this.#mounts.delete(message.documentId);
          this.#placements.delete(message.documentId);
          this.#errors.delete(message.documentId);
          if (deleted) this.#publish();
          return { applied: deleted, documentId: message.documentId };
        }
        case "error": {
          const documentId = message.documentId ?? "host";
          this.#errors.set(documentId, `${message.code}: ${message.message}`);
          this.#publish();
          return { applied: true, documentId: message.documentId };
        }
        case "action":
        case "subscription":
        case "unsubscribe":
        case "cancelAction":
          // Mediated producer requests are forwarded by the extension bridge;
          // they never mutate the renderer's document store.
          return { applied: false };
      }
    } catch (error) {
      const rawDocumentId = message.type === "snapshot"
        ? message.document?.documentId
        : message.type === "patch"
          ? message.batch?.documentId
          : "documentId" in message
            ? message.documentId
            : undefined;
      const documentId = typeof rawDocumentId === "string" && rawDocumentId.length > 0 ? rawDocumentId : "unknown-document";
      return this.#reject(documentId, this.#mounts.get(documentId)?.document.revision, error instanceof Error ? error.message : String(error));
    }
  }

  clear(): void {
    if (this.#mounts.size === 0 && this.#errors.size === 0 && this.#placements.size === 0) return;
    this.#mounts.clear();
    this.#errors.clear();
    this.#placements.clear();
    this.#publish();
  }

  serialize(): { mounts: { document: UiDocument; placement: RemoteUiPlacement }[] } {
    return {
      mounts: [...this.#mounts.values()].map(({ document, placement }) => ({ document, placement })),
    };
  }

  setPlacement(documentId: string, placement: RemoteUiPlacement): void {
    if (this.#capabilities.contributionPoints?.includes(placement.point) !== true) return;
    this.#placements.set(documentId, placement);
    const current = this.#mounts.get(documentId);
    if (current === undefined) return;
    this.#mounts.set(documentId, { ...current, placement });
    this.#publish();
  }

  /** Atomically replace exactly one broker-attested extension's placements. */
  replaceContributions(
    owner: string,
    registrations: readonly { documentId: string; placement: RemoteUiPlacement }[],
  ): boolean {
    if (owner.length === 0) return false;
    const next = new Map<string, RemoteUiPlacement>();
    for (const registration of registrations) {
      if (registration.documentId.length === 0
        || (registration.placement.ownerScope ?? registration.placement.extensionId) !== owner
        || this.#capabilities.contributionPoints?.includes(registration.placement.point) !== true
        || next.has(registration.documentId)) return false;
      const current = this.#placements.get(registration.documentId)
        ?? this.#mounts.get(registration.documentId)?.placement;
      if (current !== undefined
        && (current.ownerScope ?? current.extensionId) !== owner) return false;
      next.set(registration.documentId, registration.placement);
    }
    for (const [documentId, placement] of [...this.#placements]) {
      if ((placement.ownerScope ?? placement.extensionId) === owner && !next.has(documentId)) this.#placements.delete(documentId);
    }
    for (const [documentId, mount] of [...this.#mounts]) {
      if ((mount.placement.ownerScope ?? mount.placement.extensionId) === owner && !next.has(documentId)) {
        this.#mounts.delete(documentId);
        this.#errors.delete(documentId);
      }
    }
    for (const [documentId, placement] of next) {
      this.#placements.set(documentId, placement);
      const current = this.#mounts.get(documentId);
      if (current !== undefined) this.#mounts.set(documentId, { ...current, placement });
    }
    this.#publish();
    return true;
  }

  restore(value: unknown): void {
    if (typeof value !== "object" || value === null || !("mounts" in value) || !Array.isArray(value.mounts)) return;
    for (const candidate of value.mounts) {
      if (typeof candidate !== "object" || candidate === null || !("document" in candidate)) continue;
      const rawPlacement = "placement" in candidate && typeof candidate.placement === "object" && candidate.placement !== null
        ? candidate.placement as Partial<RemoteUiPlacement>
        : undefined;
      const placement = rawPlacement !== undefined && typeof rawPlacement.point === "string"
        ? {
            point: rawPlacement.point,
            ...(typeof rawPlacement.extensionId === "string" ? { extensionId: rawPlacement.extensionId } : {}),
            ...(typeof rawPlacement.ownerScope === "string" ? { ownerScope: rawPlacement.ownerScope } : {}),
            ...(typeof rawPlacement.publisher === "string" ? { publisher: rawPlacement.publisher } : {}),
            ...(typeof rawPlacement.trust === "string" ? { trust: rawPlacement.trust } : {}),
            ...(typeof rawPlacement.slot === "string" ? { slot: rawPlacement.slot } : {}),
            ...(typeof rawPlacement.priority === "number" && Number.isFinite(rawPlacement.priority) ? { priority: rawPlacement.priority } : {}),
          } as RemoteUiPlacement
        : undefined;
      this.apply({ type: "snapshot", document: candidate.document as UiDocument }, placement);
    }
  }

  #reject(documentId: string, knownRevision: number | undefined, reason: string): ApplyResult {
    this.#errors.set(documentId, reason);
    this.#publish();
    return { applied: false, documentId, resync: { documentId, knownRevision, reason } };
  }

  #publish(): void {
    this.#snapshot = {
      mounts: [...this.#mounts.values()].sort(compareMounts),
      errors: Object.fromEntries(this.#errors),
    };
    this.#listeners.forEach((listener) => listener());
  }
}
