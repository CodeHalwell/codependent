import {
  UI_EVENT_TYPES,
  UI_PROTOCOL_VERSION,
  type UiCapabilities,
  type UiDocument,
  type UiEvent,
  type UiHostMessage,
  type UiJsonValue,
  type UiPatchBatch,
  type UiRuntimeMessage,
} from "@codypendent/ui";

export interface UiWireSnapshot {
  document: UiDocument;
  reason?: string;
}

export interface UiWireContribution {
  id: string;
  extensionId: string;
  point: string;
  slot: string;
  documentId: string;
  priority?: number;
  when?: string;
  requires?: string[];
  metadata?: Record<string, UiJsonValue>;
}

export interface UiWireTheme {
  id: string;
  name: string;
  revision: number;
  colorScheme?: string;
  highContrast?: boolean;
  reducedMotion?: boolean;
  tokens?: Record<string, UiJsonValue>;
}

export interface UiProjectionSubscription {
  subscriptionId: string;
  kind: string;
  resourceId?: string;
  parameters?: Record<string, UiJsonValue>;
}

export interface UiProjectionUnsubscription {
  subscriptionId: string;
}

export interface UiProjectionUpdate {
  subscriptionId: string;
  revision?: number;
  removed?: boolean;
  value?: UiJsonValue;
}

export interface UiActionInvocation {
  invocationId: string;
  documentId: string;
  revision: number;
  sourceNodeId: string;
  actionId: string;
  payload?: UiJsonValue;
  formData?: Record<string, UiJsonValue>;
  interactionToken?: string;
  interactionEventType?: UiEvent["type"];
}

export interface UiActionResult {
  invocationId: string;
  status: string;
  value?: UiJsonValue;
  error?: UiWireMessage["error"];
}

/** TypeScript mirror of `codypendent_protocol::UiWireMessage`. */
export interface UiWireMessage {
  /** Canonical Rust serialization discriminator. */
  type?: string;
  /** Accepted during the protocol migration; Rust deserializes it as an alias. */
  kind?: string;
  messageId: string;
  snapshot?: UiWireSnapshot;
  patchBatch?: UiPatchBatch;
  event?: UiEvent;
  action?: UiActionInvocation;
  subscription?: UiProjectionSubscription;
  unsubscription?: UiProjectionUnsubscription;
  projection?: UiProjectionUpdate;
  actionResult?: UiActionResult;
  cancellation?: { invocationId: string };
  dispose?: { documentId: string; revision: number };
  viewport?: { width: number; height: number; pixelWidth?: number; pixelHeight?: number; density?: number };
  resync?: { documentId: string; knownRevision?: number };
  hotReload?: { generation: number; changedModules: string[] };
  capabilities?: UiCapabilities;
  selection?: UiJsonValue;
  contributions?: UiWireContribution[];
  theme?: UiWireTheme;
  error?: {
    code: string;
    message: string;
    recoverable?: boolean;
    documentId?: string;
    nodeId?: string;
    recovery?: string;
  };
  extensions?: Record<string, UiJsonValue>;
}

const MAX_WIRE_BYTES = 8 * 1024 * 1024;
const MAX_ID_LENGTH = 256;

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isBoundedJson(value: unknown): value is UiJsonValue {
  const seen = new Set<object>();
  let count = 0;
  const visit = (candidate: unknown, depth: number): boolean => {
    count += 1;
    if (count > 100_000 || depth > 64) return false;
    if (candidate === null || typeof candidate === "string" || typeof candidate === "boolean") return true;
    if (typeof candidate === "number") return Number.isFinite(candidate);
    if (typeof candidate !== "object" || seen.has(candidate)) return false;
    seen.add(candidate);
    if (Array.isArray(candidate)) return candidate.every((entry) => visit(entry, depth + 1));
    const entries = Object.entries(candidate as Record<string, unknown>);
    return Object.getPrototypeOf(candidate) === Object.prototype
      && entries.every(([key, entry]) => key.length <= MAX_ID_LENGTH && visit(entry, depth + 1));
  };
  return visit(value, 0);
}

function boundedString(value: unknown, maximum = MAX_ID_LENGTH): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= maximum;
}

export function wireType(message: Pick<UiWireMessage, "type" | "kind">): string {
  return message.type ?? message.kind ?? "";
}

const MEDIATED_RUNTIME_TYPES = new Set(["subscription", "unsubscribe", "action", "cancelAction"]);
const MEDIATED_HOST_TYPES = new Set(["projection", "actionResult", "subscription", "unsubscribe", "action", "cancelAction"]);

export function isMediatedRuntimeWire(value: unknown): value is UiWireMessage {
  return isUiWireMessage(value) && MEDIATED_RUNTIME_TYPES.has(wireType(value));
}

export function isMediatedHostWire(value: unknown): value is UiWireMessage {
  return isUiWireMessage(value) && MEDIATED_HOST_TYPES.has(wireType(value));
}

function isProtocolVersion(value: unknown): boolean {
  return isObject(value)
    && Number.isSafeInteger(value.major)
    && Number.isSafeInteger(value.minor)
    && value.major === UI_PROTOCOL_VERSION.major;
}

/** Fail-closed validation before untrusted webview input crosses into Node. */
export function isUiEvent(value: unknown): value is UiEvent {
  if (!isObject(value)) return false;
  if (!isProtocolVersion(value.protocolVersion)) return false;
  if (!boundedString(value.eventId) || !boundedString(value.documentId) || !boundedString(value.targetId)) return false;
  if (!Number.isSafeInteger(value.revision) || Number(value.revision) < 0) return false;
  if (value.interactionToken !== undefined && !boundedString(value.interactionToken)) return false;
  const types = new Set<string>(UI_EVENT_TYPES);
  if (typeof value.type !== "string" || !types.has(value.type)) return false;
  try {
    return new TextEncoder().encode(JSON.stringify(value)).byteLength <= MAX_WIRE_BYTES;
  } catch {
    return false;
  }
}

export function isUiWireMessage(value: unknown): value is UiWireMessage {
  if (!isObject(value) || !boundedString(value.type ?? value.kind) || !boundedString(value.messageId)) return false;
  if (value.type !== undefined && value.kind !== undefined && value.type !== value.kind) return false;
  try {
    if (new TextEncoder().encode(JSON.stringify(value)).byteLength > MAX_WIRE_BYTES) return false;
  } catch {
    return false;
  }
  if (value.snapshot !== undefined && (!isObject(value.snapshot) || !isObject(value.snapshot.document))) return false;
  if (value.patchBatch !== undefined && !isObject(value.patchBatch)) return false;
  if (value.event !== undefined && !isUiEvent(value.event)) return false;
  if (value.action !== undefined && (!isObject(value.action)
    || !boundedString(value.action.invocationId)
    || !boundedString(value.action.documentId)
    || !boundedString(value.action.sourceNodeId)
    || !boundedString(value.action.actionId)
    || !Number.isSafeInteger(value.action.revision)
    || Number(value.action.revision) < 0)) return false;
  if (isObject(value.action)
    && ((value.action.payload !== undefined && !isBoundedJson(value.action.payload))
      || (value.action.formData !== undefined && !isBoundedJson(value.action.formData))
      || (value.action.interactionToken !== undefined && !boundedString(value.action.interactionToken))
      || (value.action.interactionEventType !== undefined && !new Set<string>(UI_EVENT_TYPES).has(String(value.action.interactionEventType))))) return false;
  if (value.subscription !== undefined && (!isObject(value.subscription)
    || !boundedString(value.subscription.subscriptionId)
    || !boundedString(value.subscription.kind)
    || (value.subscription.resourceId !== undefined && !boundedString(value.subscription.resourceId)))) return false;
  if (isObject(value.subscription) && value.subscription.parameters !== undefined && !isBoundedJson(value.subscription.parameters)) return false;
  if (value.unsubscription !== undefined && (!isObject(value.unsubscription) || !boundedString(value.unsubscription.subscriptionId))) return false;
  if (value.projection !== undefined && (!isObject(value.projection)
    || !boundedString(value.projection.subscriptionId)
    || (value.projection.revision !== undefined && (!Number.isSafeInteger(value.projection.revision) || Number(value.projection.revision) < 0))
    || (value.projection.removed === true && value.projection.value !== undefined && value.projection.value !== null))) return false;
  if (isObject(value.projection) && value.projection.value !== undefined && !isBoundedJson(value.projection.value)) return false;
  if (value.actionResult !== undefined && (!isObject(value.actionResult)
    || !boundedString(value.actionResult.invocationId)
    || !boundedString(value.actionResult.status))) return false;
  if (isObject(value.actionResult)
    && ((value.actionResult.status === "succeeded" && value.actionResult.error !== undefined)
      || (value.actionResult.status === "failed" && !isObject(value.actionResult.error)))) return false;
  if (isObject(value.actionResult)
    && ((value.actionResult.value !== undefined && !isBoundedJson(value.actionResult.value))
      || (value.actionResult.error !== undefined && (!isObject(value.actionResult.error) || !boundedString(value.actionResult.error.code) || typeof value.actionResult.error.message !== "string")))) return false;
  if (value.cancellation !== undefined && (!isObject(value.cancellation) || !boundedString(value.cancellation.invocationId))) return false;
  if (value.dispose !== undefined && (!isObject(value.dispose) || !boundedString(value.dispose.documentId) || !Number.isSafeInteger(value.dispose.revision) || Number(value.dispose.revision) < 0)) return false;
  if (value.viewport !== undefined && (!isObject(value.viewport)
    || Number(value.viewport.width) <= 0
    || Number(value.viewport.height) <= 0
    || (value.viewport.density !== undefined && (!Number.isFinite(value.viewport.density) || Number(value.viewport.density) <= 0)))) return false;
  if (value.resync !== undefined && (!isObject(value.resync) || !boundedString(value.resync.documentId) || (value.resync.knownRevision !== undefined && (!Number.isSafeInteger(value.resync.knownRevision) || Number(value.resync.knownRevision) < 0)))) return false;
  if (value.hotReload !== undefined && (!isObject(value.hotReload)
    || !Number.isSafeInteger(value.hotReload.generation)
    || !Array.isArray(value.hotReload.changedModules)
    || value.hotReload.changedModules.length > 1_000
    || !value.hotReload.changedModules.every((module) => typeof module === "string" && module.length <= 1_024))) return false;
  if (value.contributions !== undefined) {
    if (!Array.isArray(value.contributions) || value.contributions.length > 1_000) return false;
    if (!value.contributions.every((item) => isObject(item)
      && boundedString(item.id)
      && boundedString(item.extensionId)
      && boundedString(item.documentId)
      && boundedString(item.point)
      && boundedString(item.slot))) return false;
  }
  if (value.theme !== undefined && (!isObject(value.theme) || !boundedString(value.theme.id))) return false;
  if (value.error !== undefined && (!isObject(value.error) || !boundedString(value.error.code) || typeof value.error.message !== "string")) return false;
  if (value.capabilities !== undefined && !isUiRuntimeMessage({ type: "capabilities", capabilities: value.capabilities })) return false;
  if (value.selection !== undefined && !isBoundedJson(value.selection)) return false;
  if (value.extensions !== undefined && !isBoundedJson(value.extensions)) return false;
  const kind = String(value.type ?? value.kind);
  const requiredPayload: Readonly<Record<string, string>> = {
    snapshot: "snapshot", patchBatch: "patchBatch", event: "event", action: "action",
    subscription: "subscription", unsubscribe: "unsubscription", projection: "projection", actionResult: "actionResult",
    cancelAction: "cancellation", dispose: "dispose", viewport: "viewport", resync: "resync",
    hotReload: "hotReload", capabilities: "capabilities", capabilitySelection: "selection",
    contributions: "contributions", theme: "theme", error: "error",
  };
  const field = requiredPayload[kind];
  if (field === undefined) return isObject(value.extensions) || value.error !== undefined;
  const payloadCount = [
    "snapshot", "patchBatch", "event", "action", "subscription", "unsubscription", "projection",
    "actionResult", "cancellation", "dispose", "viewport", "resync", "hotReload",
    "capabilities", "selection", "theme", "error",
  ].filter((candidate) => value[candidate] !== undefined).length
    + (Array.isArray(value.contributions) ? 1 : 0);
  if (payloadCount !== 1) return false;
  if (field === "contributions") {
    if (!Array.isArray(value.contributions)) return false;
    if (!isObject(value.extensions) || !boundedString(value.extensions.contributionOwner)) return false;
    const owner = value.extensions.contributionOwner;
    return value.contributions.every((registration) => isObject(registration)
      && registration.extensionId === owner);
  }
  return value[field] !== undefined;
}

export function isUiRuntimeMessage(value: unknown): value is UiRuntimeMessage {
  if (!isObject(value) || typeof value.type !== "string") return false;
  try {
    if (new TextEncoder().encode(JSON.stringify(value)).byteLength > MAX_WIRE_BYTES) return false;
  } catch {
    return false;
  }
  switch (value.type) {
    case "event":
      return isUiEvent(value.event);
    case "projection":
      return isUiWireMessage({ type: "projection", messageId: "validation", projection: value.projection });
    case "actionResult":
      return isUiWireMessage({ type: "actionResult", messageId: "validation", actionResult: value.result });
    case "capabilities":
      if (!isObject(value.capabilities)
        || !Array.isArray(value.capabilities.protocolVersions)
        || !value.capabilities.protocolVersions.every(isProtocolVersion)
        || !(value.capabilities.primitives === "*" || (Array.isArray(value.capabilities.primitives) && value.capabilities.primitives.length <= 512 && value.capabilities.primitives.every((primitive) => boundedString(primitive))))
        || !Array.isArray(value.capabilities.media)
        || !isObject(value.capabilities.daemon)
        || !isObject(value.capabilities.viewport)) return false;
      return Number(value.capabilities.viewport.width) > 0 && Number(value.capabilities.viewport.height) > 0;
    case "viewport":
      return isObject(value.viewport) && Number(value.viewport.width) > 0 && Number(value.viewport.height) > 0;
    case "resync":
      return boundedString(value.documentId) && (value.knownRevision === undefined || Number.isSafeInteger(value.knownRevision));
    case "hotReload":
      return Number.isSafeInteger(value.generation) && Array.isArray(value.changedModules)
        && value.changedModules.every((module) => typeof module === "string" && module.length <= 1024);
    default:
      return false;
  }
}

export function runtimeToWire(message: UiRuntimeMessage): UiWireMessage {
  const messageId = message.type === "event" ? message.event.eventId : crypto.randomUUID();
  switch (message.type) {
    case "event":
      return { type: "event", messageId, event: message.event };
    case "projection":
      return { type: "projection", messageId, projection: message.projection };
    case "actionResult":
      return { type: "actionResult", messageId, actionResult: message.result };
    case "capabilities":
      return { type: "capabilities", messageId, capabilities: message.capabilities };
    case "viewport":
      return { type: "viewport", messageId, viewport: message.viewport };
    case "resync":
      return {
        type: "resync",
        messageId,
        resync: { documentId: message.documentId, ...(message.knownRevision === undefined ? {} : { knownRevision: message.knownRevision }) },
      };
    case "hotReload":
      return {
        type: "hotReload",
        messageId,
        hotReload: { generation: message.generation, changedModules: message.changedModules },
      };
  }
}

export interface WireHostProjection {
  messages: UiHostMessage[];
  placements: Map<string, { point: string; extensionId?: string; ownerScope?: string; publisher?: string; trust?: string; slot?: string; priority?: number }>;
  contributionReplacement?: {
    owner: string;
    registrations: { documentId: string; point: string; extensionId: string; ownerScope?: string; publisher?: string; trust?: string; slot?: string; priority?: number }[];
  };
  theme?: UiWireTheme;
  mediated: UiWireMessage[];
}

/** Translate the dedicated daemon envelope into SDK-level webview messages. */
export function wireToHost(message: UiWireMessage): WireHostProjection {
  const messages: UiHostMessage[] = [];
  if (message.snapshot !== undefined) messages.push({ type: "snapshot", document: message.snapshot.document });
  if (message.patchBatch !== undefined) messages.push({ type: "patch", batch: message.patchBatch });
  if (message.error !== undefined) {
    messages.push({
      type: "error",
      ...(message.error.documentId === undefined ? {} : { documentId: message.error.documentId }),
      code: message.error.code,
      message: message.error.message,
    });
  }
  if (wireType(message) === "dispose" && message.dispose !== undefined) {
    messages.push({ type: "dispose", documentId: message.dispose.documentId, revision: message.dispose.revision });
  }
  const placements = new Map<string, { point: string; extensionId?: string; ownerScope?: string; publisher?: string; trust?: string; slot?: string; priority?: number }>();
  for (const contribution of message.contributions ?? []) {
    if (typeof contribution.documentId !== "string" || typeof contribution.point !== "string") continue;
    placements.set(contribution.documentId, {
      point: contribution.point,
      ...(typeof contribution.metadata?.hostExtensionId === "string"
        ? { extensionId: contribution.metadata.hostExtensionId }
        : typeof contribution.extensionId !== "string" || contribution.extensionId.length === 0 ? {} : { extensionId: contribution.extensionId }),
      ...(typeof contribution.extensionId !== "string" || contribution.extensionId.length === 0 ? {} : { ownerScope: contribution.extensionId }),
      ...(typeof contribution.metadata?.hostPublisher !== "string" ? {} : { publisher: contribution.metadata.hostPublisher }),
      ...(typeof contribution.metadata?.hostTrust !== "string" ? {} : { trust: contribution.metadata.hostTrust }),
      ...(typeof contribution.slot !== "string" || contribution.slot.length === 0 ? {} : { slot: contribution.slot }),
      ...(typeof contribution.priority !== "number" || !Number.isFinite(contribution.priority) ? {} : { priority: contribution.priority }),
    });
  }
  const mediated = isMediatedHostWire(message)
    ? [message]
    : [];
  const replacementOwner = wireType(message) === "contributions"
    && typeof message.extensions?.contributionOwner === "string"
    ? message.extensions.contributionOwner
    : undefined;
  const contributionReplacement = replacementOwner === undefined
    ? undefined
    : {
        owner: replacementOwner,
        registrations: [...placements].flatMap(([documentId, placement]) => (placement.ownerScope ?? placement.extensionId) !== replacementOwner
          ? []
          : [{
              documentId,
              point: placement.point,
              extensionId: placement.extensionId ?? replacementOwner,
              ownerScope: replacementOwner,
              ...(placement.publisher === undefined ? {} : { publisher: placement.publisher }),
              ...(placement.trust === undefined ? {} : { trust: placement.trust }),
              ...(placement.slot === undefined ? {} : { slot: placement.slot }),
              ...(placement.priority === undefined ? {} : { priority: placement.priority }),
            }]),
      };
  return {
    messages,
    placements,
    mediated,
    ...(contributionReplacement === undefined ? {} : { contributionReplacement }),
    ...(message.theme === undefined ? {} : { theme: message.theme }),
  };
}
