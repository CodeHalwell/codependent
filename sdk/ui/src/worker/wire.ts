import {
  DEFAULT_UI_HARD_LIMITS,
  type UiCapabilities,
  type UiHardLimits,
  type UiWireMessage,
} from "../protocol.js";
import { validateDocument, validatePatchBatch } from "../validation.js";

export type UiWireDirection = "host-to-worker" | "worker-to-host" | "handshake";

const HOST_KINDS = new Set([
  "capabilities", "capabilitySelection", "event", "projection", "actionResult", "viewport",
  "resync", "hotReload", "dispose", "theme", "host.ping", "host.pong", "host.dispose",
]);
const WORKER_KINDS = new Set([
  "capabilities", "snapshot", "patchBatch", "subscription", "unsubscribe", "action", "cancelAction", "contributions", "dispose", "error",
  "resync", "worker.ready", "worker.ping", "worker.pong", "worker.resync", "worker.reloaded", "worker.disposed",
]);
const PAYLOAD_BY_TYPE: Readonly<Record<string, string | undefined>> = {
  snapshot: "snapshot", patchBatch: "patchBatch", event: "event", action: "action",
  subscription: "subscription", unsubscribe: "unsubscription", projection: "projection", actionResult: "actionResult",
  cancelAction: "cancellation", dispose: "dispose", viewport: "viewport", resync: "resync",
  hotReload: "hotReload", capabilities: "capabilities", capabilitySelection: "selection",
  contributions: "contributions", theme: "theme", error: "error",
};
const TYPED_PAYLOADS = new Set(Object.values(PAYLOAD_BY_TYPE).filter((value): value is string => value !== undefined));

function record(value: unknown, path: string): asserts value is Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value) || Object.getPrototypeOf(value) !== Object.prototype) {
    throw new Error(`${path} must be a plain object`);
  }
}

function text(value: unknown, path: string): asserts value is string {
  if (typeof value !== "string" || value.length === 0 || value.length > 1024 || /[\u0000-\u0008\u000b\u000c\u000e-\u001f]/u.test(value)) {
    throw new Error(`${path} must be a non-empty bounded string without control characters`);
  }
}

function safeInteger(value: unknown, path: string): asserts value is number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) throw new Error(`${path} must be a non-negative safe integer`);
}

function boolean(value: unknown, path: string): asserts value is boolean {
  if (typeof value !== "boolean") throw new Error(`${path} must be a boolean`);
}

function stringArray(value: unknown, path: string): asserts value is string[] {
  if (!Array.isArray(value)) throw new Error(`${path} must be an array`);
  const seen = new Set<string>();
  for (const [index, item] of value.entries()) {
    text(item, `${path}[${index}]`);
    if (seen.has(item)) throw new Error(`${path} contains duplicate ${JSON.stringify(item)}`);
    seen.add(item);
  }
}

function validateViewport(value: unknown, path: string): void {
  record(value, path);
  if (!Number.isSafeInteger(value.width) || (value.width as number) <= 0) throw new Error(`${path}.width must be positive`);
  if (!Number.isSafeInteger(value.height) || (value.height as number) <= 0) throw new Error(`${path}.height must be positive`);
  for (const field of ["pixelWidth", "pixelHeight"] as const) if (value[field] !== undefined && (!Number.isSafeInteger(value[field]) || (value[field] as number) <= 0)) throw new Error(`${path}.${field} must be positive`);
  if (value.density !== undefined && (typeof value.density !== "number" || !Number.isFinite(value.density) || value.density <= 0)) throw new Error(`${path}.density must be finite and positive`);
}

function validateVersion(value: unknown, path: string): void {
  record(value, path);
  safeInteger(value.major, `${path}.major`);
  safeInteger(value.minor, `${path}.minor`);
  if (value.major === 0 || value.major > 65_535 || (value.minor as number) > 65_535) throw new Error(`${path} is outside the supported integer range`);
}

const LIMIT_FIELDS: ReadonlyArray<keyof UiHardLimits> = [
  "maxTreeDepth", "maxNodes", "maxTextBytes", "maxPropertiesPerNode", "maxActionsPerNode",
  "maxJsonDepth", "maxJsonValues", "maxPatchesPerBatch", "maxPatchBytes", "maxContributions",
];
function validateLimits(value: unknown, path: string): asserts value is UiHardLimits {
  record(value, path);
  for (const field of LIMIT_FIELDS) if (!Number.isSafeInteger(value[field]) || (value[field] as number) <= 0) throw new Error(`${path}.${field} must be a positive integer`);
}

function boundedJson(value: unknown, limits: UiHardLimits, path = "$", depth = 0, counter = { values: 0 }): void {
  counter.values += 1;
  if (counter.values > limits.maxJsonValues) throw new Error(`${path} exceeds maxJsonValues`);
  if (depth > limits.maxJsonDepth) throw new Error(`${path} exceeds maxJsonDepth`);
  if (value === null || typeof value === "boolean" || typeof value === "string") return;
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw new Error(`${path} contains a non-finite number`);
    return;
  }
  if (Array.isArray(value)) {
    for (const [index, child] of value.entries()) boundedJson(child, limits, `${path}[${index}]`, depth + 1, counter);
    return;
  }
  record(value, path);
  for (const [key, child] of Object.entries(value)) {
    text(key, `${path} key`);
    boundedJson(child, limits, `${path}.${key}`, depth + 1, counter);
  }
}

function validateCapabilities(value: unknown): asserts value is UiCapabilities {
  record(value, "capabilities");
  text(value.client, "capabilities.client");
  if (!Array.isArray(value.protocolVersions) || value.protocolVersions.length === 0) throw new Error("capabilities.protocolVersions must not be empty");
  for (const [index, version] of value.protocolVersions.entries()) {
    validateVersion(version, `capabilities.protocolVersions[${index}]`);
  }
  record(value.daemon, "capabilities.daemon");
  for (const field of ["rich_text", "image_display", "audio_capture", "editor_mutations", "diff_view", "mouse", "unicode", "true_color"] as const) boolean(value.daemon[field], `capabilities.daemon.${field}`);
  if (value.primitives !== "*") stringArray(value.primitives, "capabilities.primitives");
  stringArray(value.media, "capabilities.media");
  for (const media of value.media) if (!["image", "audio", "video"].includes(media)) throw new Error(`unsupported media capability ${JSON.stringify(media)}`);
  for (const field of ["keyboard", "screenReader", "reducedMotion", "clipboard"] as const) boolean(value[field], `capabilities.${field}`);
  if (value.terminalGraphics !== undefined) stringArray(value.terminalGraphics, "capabilities.terminalGraphics");
  if (value.capabilities !== undefined) stringArray(value.capabilities, "capabilities.capabilities");
  if (value.contributionPoints !== undefined) stringArray(value.contributionPoints, "capabilities.contributionPoints");
  validateViewport(value.viewport, "capabilities.viewport");
  if (value.limits !== undefined) validateLimits(value.limits, "capabilities.limits");
  if (value.colorDepth !== "monochrome" && value.colorDepth !== "ansi16" && value.colorDepth !== "ansi256" && value.colorDepth !== "trueColor") {
    throw new Error("capabilities.colorDepth is unsupported");
  }
}

function validateSelection(value: unknown): void {
  record(value, "selection");
  validateVersion(value.protocolVersion, "selection.protocolVersion");
  for (const field of ["primitives", "capabilities", "contributionPoints", "imageProtocols"] as const) stringArray(value[field], `selection.${field}`);
  if (![1, 4, 8, 24].includes(value.colorDepth as number)) throw new Error("selection.colorDepth must be 1, 4, 8, or 24");
  for (const field of ["unicode", "mouse", "screenReader"] as const) boolean(value[field], `selection.${field}`);
  if (value.viewport !== undefined) validateViewport(value.viewport, "selection.viewport");
  validateLimits(value.limits, "selection.limits");
}

/** Runtime validation for values crossing the process boundary; static types are never trusted here. */
export function assertUiWireMessage(
  value: unknown,
  direction: UiWireDirection,
  limits: UiHardLimits = DEFAULT_UI_HARD_LIMITS,
): asserts value is UiWireMessage {
  record(value, "message");
  text(value.type, "message.type");
  text(value.messageId, "message.messageId");
  const type = value.type;
  const allowed = direction === "host-to-worker" ? HOST_KINDS : direction === "worker-to-host" ? WORKER_KINDS : new Set([...HOST_KINDS, ...WORKER_KINDS]);
  if (!allowed.has(type)) throw new Error(`${direction} message type ${JSON.stringify(type)} is not allowed`);
  const expected = PAYLOAD_BY_TYPE[type];
  const payloads = [...TYPED_PAYLOADS].filter((key) => value[key] !== undefined);
  if (expected !== undefined && (value[expected] === undefined || payloads.length !== 1)) {
    throw new Error(`message ${type} must carry exactly its ${expected} payload`);
  }
  if (expected === undefined && (payloads.length !== 0 || value.extensions === undefined)) {
    throw new Error(`control message ${type} must carry only extensions`);
  }
  const allowedKeys = new Set(["type", "messageId", "extensions", ...(expected === undefined ? [] : [expected])]);
  for (const key of Object.keys(value)) if (!allowedKeys.has(key)) throw new Error(`message ${type} contains unexpected top-level field ${key}`);
  boundedJson(value, limits);
  if (type === "capabilities") validateCapabilities(value.capabilities);
  if (type === "capabilitySelection") validateSelection(value.selection);
  if (type === "snapshot") {
    record(value.snapshot, "snapshot");
    record(value.snapshot.document, "snapshot.document");
    validateVersion(value.snapshot.document.protocolVersion, "snapshot.document.protocolVersion");
    text(value.snapshot.document.documentId, "snapshot.document.documentId");
    const result = validateDocument(value.snapshot.document as never, {
      maxDepth: limits.maxTreeDepth,
      maxNodes: limits.maxNodes,
      maxTextBytes: limits.maxTextBytes,
      maxPropertiesPerNode: limits.maxPropertiesPerNode,
      maxActionsPerNode: limits.maxActionsPerNode,
      maxJsonDepth: limits.maxJsonDepth,
      maxJsonValues: limits.maxJsonValues,
      maxPatchCount: limits.maxPatchesPerBatch,
      maxDocumentBytes: limits.maxPatchBytes * 2,
    });
    if (!result.valid) throw new Error(result.issues.map((issue) => `${issue.path}: ${issue.message}`).join("; "));
  }
  if (type === "patchBatch") {
    record(value.patchBatch, "patchBatch");
    validateVersion(value.patchBatch.protocolVersion, "patchBatch.protocolVersion");
    text(value.patchBatch.documentId, "patchBatch.documentId");
    const result = validatePatchBatch(value.patchBatch as never, undefined, { maxPatchCount: limits.maxPatchesPerBatch });
    if (!result.valid) throw new Error(result.issues.map((issue) => `${issue.path}: ${issue.message}`).join("; "));
  }
  if (type === "event") {
    record(value.event, "event"); validateVersion(value.event.protocolVersion, "event.protocolVersion");
    for (const field of ["eventId", "documentId", "targetId", "type"] as const) text(value.event[field], `event.${field}`);
    safeInteger(value.event.revision, "event.revision");
  }
  if (type === "projection") {
    record(value.projection, "projection"); text(value.projection.subscriptionId, "projection.subscriptionId");
    if (value.projection.revision !== undefined) safeInteger(value.projection.revision, "projection.revision");
    if (value.projection.removed !== undefined) boolean(value.projection.removed, "projection.removed");
    if (value.projection.removed === true && value.projection.value !== undefined && value.projection.value !== null) throw new Error("a removed projection cannot carry a value");
  }
  if (type === "actionResult") {
    record(value.actionResult, "actionResult"); text(value.actionResult.invocationId, "actionResult.invocationId"); text(value.actionResult.status, "actionResult.status");
    if (value.actionResult.status === "succeeded" && value.actionResult.error !== undefined) throw new Error("a succeeded action cannot carry an error");
    if (value.actionResult.status === "failed" && value.actionResult.error === undefined) throw new Error("a failed action requires a structured error");
    if (value.actionResult.error !== undefined) { record(value.actionResult.error, "actionResult.error"); text(value.actionResult.error.code, "actionResult.error.code"); text(value.actionResult.error.message, "actionResult.error.message"); }
  }
  if (type === "subscription") { record(value.subscription, "subscription"); text(value.subscription.subscriptionId, "subscription.subscriptionId"); text(value.subscription.kind, "subscription.kind"); if (value.subscription.resourceId !== undefined) text(value.subscription.resourceId, "subscription.resourceId"); }
  if (type === "unsubscribe") { record(value.unsubscription, "unsubscription"); text(value.unsubscription.subscriptionId, "unsubscribe.subscriptionId"); }
  if (type === "action") {
    record(value.action, "action");
    for (const field of ["invocationId", "documentId", "sourceNodeId", "actionId"] as const) text(value.action[field], `action.${field}`);
    safeInteger(value.action.revision, "action.revision");
  }
  if (type === "cancelAction") { record(value.cancellation, "cancellation"); text(value.cancellation.invocationId, "cancellation.invocationId"); }
  if (type === "viewport") validateViewport(value.viewport, "viewport");
  if (type === "dispose") { record(value.dispose, "dispose"); text(value.dispose.documentId, "dispose.documentId"); safeInteger(value.dispose.revision, "dispose.revision"); }
  if (type === "resync") { record(value.resync, "resync"); text(value.resync.documentId, "resync.documentId"); if (value.resync.knownRevision !== undefined) safeInteger(value.resync.knownRevision, "resync.knownRevision"); }
  if (type === "hotReload") { record(value.hotReload, "hotReload"); safeInteger(value.hotReload.generation, "hotReload.generation"); stringArray(value.hotReload.changedModules, "hotReload.changedModules"); if (value.hotReload.changedModules.length === 0) throw new Error("hotReload.changedModules must not be empty"); }
  if (type === "contributions") {
    if (!Array.isArray(value.contributions) || value.contributions.length > limits.maxContributions) throw new Error("contributions must be a bounded array");
    record(value.extensions, "extensions");
    const owner = value.extensions.contributionOwner;
    text(owner, "extensions.contributionOwner");
    const ids = new Set<string>();
    for (const [index, contribution] of value.contributions.entries()) {
      record(contribution, `contributions[${index}]`);
      for (const field of ["id", "extensionId", "point", "slot", "documentId"] as const) text(contribution[field], `contributions[${index}].${field}`);
      if (contribution.extensionId !== owner) throw new Error(`contributions[${index}].extensionId must equal contributionOwner`);
      const id = contribution.id;
      text(id, `contributions[${index}].id`);
      if (ids.has(id)) throw new Error(`duplicate contribution id ${JSON.stringify(id)}`);
      ids.add(id);
      if (contribution.priority !== undefined && !Number.isSafeInteger(contribution.priority)) throw new Error(`contributions[${index}].priority must be an integer`);
      if (contribution.requires !== undefined) stringArray(contribution.requires, `contributions[${index}].requires`);
    }
  }
  if (type === "theme") { record(value.theme, "theme"); text(value.theme.id, "theme.id"); text(value.theme.name, "theme.name"); safeInteger(value.theme.revision, "theme.revision"); }
}

export function protocolMatchesSelection(message: UiWireMessage, major: number, minor: number): boolean {
  const protocol = message.type === "snapshot" ? message.snapshot.document.protocolVersion
    : message.type === "patchBatch" ? message.patchBatch.protocolVersion
      : message.type === "event" ? message.event.protocolVersion
        : undefined;
  return protocol === undefined || (protocol.major === major && protocol.minor <= minor);
}
