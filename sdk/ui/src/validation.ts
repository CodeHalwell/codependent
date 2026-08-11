import {
  DEFAULT_UI_HARD_LIMITS,
  UI_EVENT_HANDLERS_PROP,
  UI_EVENT_TYPES,
  type UiDocument,
  type UiJsonValue,
  type UiNode,
  type UiPatchBatch,
} from "./protocol.js";

export interface UiLimits {
  maxDepth: number;
  maxNodes: number;
  maxChildrenPerNode: number;
  maxTextBytes: number;
  maxPropsBytes: number;
  maxPropertiesPerNode: number;
  maxActionsPerNode: number;
  maxJsonDepth: number;
  maxJsonValues: number;
  maxPatchCount: number;
  maxDocumentBytes: number;
  maxIdLength: number;
}

export const DEFAULT_UI_LIMITS: UiLimits = {
  // Derive the overlapping limits from the negotiated wire contract. Local
  // tests, workbench inspection, workers, and production hosts must never
  // disagree about whether the same semantic document is admissible.
  maxDepth: DEFAULT_UI_HARD_LIMITS.maxTreeDepth,
  maxNodes: DEFAULT_UI_HARD_LIMITS.maxNodes,
  maxChildrenPerNode: 2_000,
  maxTextBytes: DEFAULT_UI_HARD_LIMITS.maxTextBytes,
  maxPropsBytes: DEFAULT_UI_HARD_LIMITS.maxPatchBytes,
  maxPropertiesPerNode: DEFAULT_UI_HARD_LIMITS.maxPropertiesPerNode,
  maxActionsPerNode: DEFAULT_UI_HARD_LIMITS.maxActionsPerNode,
  maxJsonDepth: DEFAULT_UI_HARD_LIMITS.maxJsonDepth,
  maxJsonValues: DEFAULT_UI_HARD_LIMITS.maxJsonValues,
  maxPatchCount: DEFAULT_UI_HARD_LIMITS.maxPatchesPerBatch,
  maxDocumentBytes: DEFAULT_UI_HARD_LIMITS.maxPatchBytes * 2,
  maxIdLength: 256,
};

export interface ValidationIssue {
  path: string;
  code: "limit" | "schema" | "duplicateId" | "unsafeValue" | "staleRevision";
  message: string;
}

export interface ValidationResult {
  valid: boolean;
  issues: ValidationIssue[];
}

function byteLength(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function isJsonValue(
  value: unknown,
  seen: Set<unknown>,
  counter: { values: number },
  maxDepth = DEFAULT_UI_LIMITS.maxJsonDepth,
  maxValues = DEFAULT_UI_LIMITS.maxJsonValues,
): value is UiJsonValue | undefined {
  const pending: Array<readonly [unknown, number]> = [[value, 0]];
  while (pending.length > 0) {
    const [current, depth] = pending.pop()!;
    counter.values += 1;
    if (counter.values > maxValues) return false;
    if (current === undefined || current === null || typeof current === "string" || typeof current === "boolean") continue;
    if (typeof current === "number") {
      if (!Number.isFinite(current)) return false;
      continue;
    }
    if (typeof current !== "object" || seen.has(current) || depth > maxDepth) return false;
    seen.add(current);
    if (!Array.isArray(current) && Object.getPrototypeOf(current) !== Object.prototype) return false;
    for (const entry of Array.isArray(current) ? current : Object.values(current as Record<string, unknown>)) {
      pending.push([entry, depth + 1]);
    }
  }
  return true;
}

function validateNodes(
  root: UiNode,
  state: { count: number; jsonValues: number; ids: Set<string>; issues: ValidationIssue[] },
  limits: UiLimits,
): void {
  const pending: Array<readonly [UiNode, string, number]> = [[root, "root", 0]];
  const seenNodes = new Set<UiNode>();
  while (pending.length > 0 && state.count <= limits.maxNodes) {
    const [node, path, depth] = pending.pop()!;
    state.count += 1;
    if (seenNodes.has(node)) {
      state.issues.push({ path, code: "unsafeValue", message: "Tree nodes must be acyclic and uniquely owned" });
      continue;
    }
    seenNodes.add(node);
    if (depth > limits.maxDepth) {
      state.issues.push({ path, code: "limit", message: `Tree depth exceeds ${limits.maxDepth}` });
      continue;
    }
    if (state.count > limits.maxNodes) break;
    if (node.id === undefined || node.id.length === 0) state.issues.push({ path, code: "schema", message: "Every wire node must have a non-empty id" });
    else if (node.id.length > limits.maxIdLength) state.issues.push({ path, code: "limit", message: `Node id exceeds ${limits.maxIdLength} characters` });
    else if (state.ids.has(node.id)) state.issues.push({ path, code: "duplicateId", message: `Duplicate node id: ${node.id}` });
    else state.ids.add(node.id);
    if (node.kind === "text") {
      if (byteLength(node.text) > limits.maxTextBytes) state.issues.push({ path, code: "limit", message: "Text node exceeds byte limit" });
      continue;
    }
    if (node.type.length === 0) state.issues.push({ path, code: "schema", message: "Element type is empty" });
    if (node.children.length > limits.maxChildrenPerNode) state.issues.push({ path, code: "limit", message: "Element has too many children" });
    const properties = Object.keys(node.props);
    if (properties.length > limits.maxPropertiesPerNode) state.issues.push({ path: `${path}.props`, code: "limit", message: `Props exceed ${limits.maxPropertiesPerNode} properties` });
    const jsonCounter = { values: state.jsonValues };
    const propsAreJson = isJsonValue(node.props, new Set(), jsonCounter, limits.maxJsonDepth, limits.maxJsonValues);
    if (!propsAreJson) state.issues.push({ path: `${path}.props`, code: "unsafeValue", message: "Props must be finite, acyclic, bounded JSON data" });
    else if (byteLength(JSON.stringify(node.props)) > limits.maxPropsBytes) state.issues.push({ path: `${path}.props`, code: "limit", message: "Props exceed byte limit" });
    state.jsonValues = jsonCounter.values;
    const inputType = typeof node.props.inputType === "string" ? node.props.inputType : typeof node.props.type === "string" ? node.props.type : undefined;
    if (inputType !== undefined && /^(password|secret|token|api[-_ ]?key|credential|private[-_ ]?key|passphrase|pin)$/iu.test(inputType.trim())) {
      state.issues.push({ path: `${path}.props.inputType`, code: "unsafeValue", message: "Secret entry is host-owned; remote UI may only receive an opaque handle or decision" });
    }
    for (const key of ["secret", "secretName", "secretValue", "password", "credential", "sensitive"] as const) {
      const value = node.props[key];
      if (value !== undefined && value !== null && value !== false) state.issues.push({ path: `${path}.props.${key}`, code: "unsafeValue", message: "Secret metadata is forbidden in remote UI documents" });
    }
  const eventHandlers = node.props[UI_EVENT_HANDLERS_PROP];
  const eventBindings = Array.isArray(node.props.eventBindings) ? node.props.eventBindings : [];
  const flatActions = ["action", "changeAction", "submitAction", "selectAction", "navigateAction", "validateAction", "dismissAction", "resetAction"]
    .filter((property) => typeof node.props[property] === "string").length;
  const actionCount = eventBindings.length + flatActions + (Array.isArray(eventHandlers) ? eventHandlers.length : 0);
  if (actionCount > limits.maxActionsPerNode) {
    state.issues.push({ path: `${path}.props`, code: "limit", message: `Node declares more than ${limits.maxActionsPerNode} actions` });
  }
  const canonicalEvents = new Set<string>(UI_EVENT_TYPES);
  if (eventHandlers !== undefined && (!Array.isArray(eventHandlers)
    || eventHandlers.length === 0
    || eventHandlers.length > limits.maxActionsPerNode
    || eventHandlers.some((event) => typeof event !== "string" || !canonicalEvents.has(event))
    || new Set(eventHandlers).size !== eventHandlers.length)) {
    state.issues.push({ path: `${path}.props.${UI_EVENT_HANDLERS_PROP}`, code: "schema", message: `eventHandlers must be a non-empty unique array of at most ${limits.maxActionsPerNode} canonical event names` });
  } else if (Array.isArray(eventHandlers) && Array.isArray(node.props.eventBindings)) {
    const boundEvents = new Set(node.props.eventBindings.flatMap((binding) => binding !== null && typeof binding === "object" && !Array.isArray(binding) && typeof binding.event === "string" ? [binding.event] : []));
    const ambiguous = eventHandlers.find((event) => typeof event === "string" && boundEvents.has(event));
    if (ambiguous !== undefined) state.issues.push({ path: `${path}.props`, code: "schema", message: `event ${JSON.stringify(ambiguous)} cannot be both worker-local and command-bound` });
  }
  if (Array.isArray(eventHandlers)) {
    const mediatedProperties: ReadonlyArray<readonly [string, readonly string[]]> = [
      ["action", ["action", "press"]], ["changeAction", ["change"]], ["submitAction", ["submit"]],
      ["selectAction", ["select"]], ["navigateAction", ["navigate"]], ["validateAction", ["custom"]],
      ["dismissAction", ["action", "press"]], ["resetAction", ["action", "press"]],
    ];
    for (const [property, events] of mediatedProperties) {
      if (typeof node.props[property] === "string" && eventHandlers.some((event) => typeof event === "string" && events.includes(event))) {
        state.issues.push({ path: `${path}.props`, code: "schema", message: `worker-local event conflicts with host-mediated ${property}` });
      }
    }
  }
    for (let index = node.children.length - 1; index >= 0; index -= 1) pending.push([node.children[index]!, `${path}.children[${index}]`, depth + 1]);
    if (node.fallback !== undefined) pending.push([node.fallback, `${path}.fallback`, depth + 1]);
  }
  if (state.count > limits.maxNodes) state.issues.push({ path: "root", code: "limit", message: `Tree contains more than ${limits.maxNodes} nodes` });
}

export function validateDocument(document: UiDocument, overrides: Partial<UiLimits> = {}): ValidationResult {
  const limits = { ...DEFAULT_UI_LIMITS, ...overrides };
  const issues: ValidationIssue[] = [];
  if (document.revision < 0 || !Number.isSafeInteger(document.revision)) issues.push({ path: "revision", code: "schema", message: "Revision must be a non-negative safe integer" });
  validateNodes(document.root, { count: 0, jsonValues: 0, ids: new Set(), issues }, limits);
  if (!issues.some((issue) => issue.code === "unsafeValue" || (issue.code === "limit" && issue.message.includes("depth")))
    && byteLength(JSON.stringify(document)) > limits.maxDocumentBytes) issues.push({ path: "$", code: "limit", message: "Document exceeds byte limit" });
  return { valid: issues.length === 0, issues };
}

export function validatePatchBatch(batch: UiPatchBatch, knownRevision?: number, overrides: Partial<UiLimits> = {}): ValidationResult {
  const limits = { ...DEFAULT_UI_LIMITS, ...overrides };
  const issues: ValidationIssue[] = [];
  if (batch.revision !== batch.baseRevision + 1) issues.push({ path: "revision", code: "schema", message: "Revision must advance baseRevision by exactly one" });
  if (batch.atomic !== true) issues.push({ path: "atomic", code: "schema", message: "Patch batches must be atomic" });
  if (knownRevision !== undefined && batch.baseRevision !== knownRevision) issues.push({ path: "baseRevision", code: "staleRevision", message: `Expected base revision ${knownRevision}, received ${batch.baseRevision}` });
  if (batch.patches.length > limits.maxPatchCount) issues.push({ path: "patches", code: "limit", message: "Patch batch exceeds operation limit" });
  if (batch.patches.length === 0) issues.push({ path: "patches", code: "schema", message: "A revision-advancing patch batch must contain at least one operation" });
  return { valid: issues.length === 0, issues };
}

export function assertValidDocument(document: UiDocument, limits?: Partial<UiLimits>): void {
  const result = validateDocument(document, limits);
  if (!result.valid) throw new Error(result.issues.map((issue) => `${issue.path}: ${issue.message}`).join("\n"));
}
