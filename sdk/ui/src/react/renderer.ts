import ReactReconciler from "react-reconciler";
import { DefaultEventPriority } from "react-reconciler/constants.js";
import type { ReactNode } from "react";
import { createDocument, diffDocuments } from "../document.js";
import { UI_EVENT_HANDLERS_PROP, UI_PROTOCOL_VERSION, type UiDocument, type UiEvent, type UiHostMessage, type UiJsonValue, type UiNode, type UiProps, type UiRequirement } from "../protocol.js";
import { assertValidDocument, type UiLimits } from "../validation.js";

type EventHandler = (event: UiEvent) => void;
type HostChild = HostInstance | HostText;

interface HostText { kind: "text"; id: string; text: string; hidden: boolean; }
interface HostInstance {
  kind: "element";
  id: string;
  type: string;
  props: UiProps;
  fallback?: UiNode;
  requires?: UiRequirement[];
  children: HostChild[];
  handlers: Partial<Record<UiEvent["type"], EventHandler>>;
  hidden: boolean;
}

interface HostContainer {
  documentId: string;
  idPrefix: string;
  revision: number;
  nextId: number;
  children: HostChild[];
  previous?: UiDocument;
  current?: UiDocument;
  handlers: Map<string, Partial<Record<UiEvent["type"], EventHandler>>>;
  listeners: Set<(message: UiHostMessage) => void>;
  limits?: Partial<UiLimits>;
  onError: (error: unknown) => void;
}

const handlerNames: Readonly<Record<string, UiEvent["type"]>> = {
  onPress: "press",
  onAction: "action", onChange: "change", onSubmit: "submit", onSelect: "select",
  onNavigate: "navigate", onFocus: "focus", onBlur: "blur", onCustom: "custom",
};
const EXCLUDED_PROPS = new Set(["children", "id", "fallback", "requires", UI_EVENT_HANDLERS_PROP]);

function isJson(value: unknown, seen = new Set<unknown>()): value is UiJsonValue {
  if (value === null || typeof value === "string" || typeof value === "boolean") return true;
  if (typeof value === "number") return Number.isFinite(value);
  if (typeof value !== "object" || seen.has(value)) return false;
  seen.add(value);
  if (Array.isArray(value)) return value.every((entry) => isJson(entry, seen));
  if (Object.getPrototypeOf(value) !== Object.prototype) return false;
  return Object.values(value as Record<string, unknown>).every((entry) => entry === undefined || isJson(entry, seen));
}

function parseProps(input: Readonly<Record<string, unknown>>): Pick<HostInstance, "props" | "handlers" | "fallback" | "requires"> {
  const props: Record<string, UiJsonValue | undefined> = {};
  const handlers: Partial<Record<UiEvent["type"], EventHandler>> = {};
  for (const [key, value] of Object.entries(input)) {
    const eventType = handlerNames[key];
    if (eventType !== undefined && typeof value === "function") {
      handlers[eventType] = value as EventHandler;
    } else if (!EXCLUDED_PROPS.has(key) && (value === undefined || isJson(value))) {
      props[key] = value;
    }
  }
  const declaredHandlers = Object.keys(handlers).sort() as UiEvent["type"][];
  if (declaredHandlers.length > 0) {
    const bindings = props.eventBindings;
    if (Array.isArray(bindings)) {
      const boundEvents = new Set(bindings.flatMap((binding) => binding !== null && typeof binding === "object" && !Array.isArray(binding) && typeof binding.event === "string" ? [binding.event] : []));
      const ambiguous = declaredHandlers.find((event) => boundEvents.has(event));
      if (ambiguous !== undefined) throw new Error(`worker-local handler ${JSON.stringify(ambiguous)} conflicts with props.eventBindings`);
    }
    const commandProperties: ReadonlyArray<readonly [string, readonly UiEvent["type"][]]> = [
      ["action", ["action", "press"]], ["changeAction", ["change"]], ["submitAction", ["submit"]],
      ["selectAction", ["select"]], ["navigateAction", ["navigate"]], ["validateAction", ["custom"]],
      ["dismissAction", ["action", "press"]], ["resetAction", ["action", "press"]],
    ];
    for (const [property, events] of commandProperties) {
      if (typeof props[property] === "string" && declaredHandlers.some((event) => events.includes(event))) {
        throw new Error(`worker-local handler conflicts with host-mediated ${property}`);
      }
    }
    props[UI_EVENT_HANDLERS_PROP] = declaredHandlers;
  }
  const fallback = isUiNode(input.fallback) ? input.fallback : undefined;
  const requirements = Array.isArray(input.requires) ? input.requires : input.requires === undefined ? undefined : [input.requires];
  const requires = requirements?.filter(isRequirement);
  return { props, handlers, ...(fallback === undefined ? {} : { fallback }), ...(requires === undefined ? {} : { requires }) };
}

function isRequirement(value: unknown): value is UiRequirement {
  return value !== null && typeof value === "object" && typeof (value as { feature?: unknown }).feature === "string";
}

function isUiNode(value: unknown): value is UiNode {
  if (value === null || typeof value !== "object") return false;
  const candidate = value as { kind?: unknown };
  return candidate.kind === "text" || candidate.kind === "element";
}

function allocateId(container: HostContainer, props: Readonly<Record<string, unknown>>): string {
  if (typeof props.id === "string" && props.id.length > 0) return props.id;
  container.nextId += 1;
  return `${container.idPrefix}-${container.nextId}`;
}

function append(parent: { children: HostChild[] }, child: HostChild): void {
  const current = parent.children.indexOf(child);
  if (current >= 0) parent.children.splice(current, 1);
  parent.children.push(child);
}

function insert(parent: { children: HostChild[] }, child: HostChild, before: HostChild): void {
  const current = parent.children.indexOf(child);
  if (current >= 0) parent.children.splice(current, 1);
  const index = parent.children.indexOf(before);
  parent.children.splice(index < 0 ? parent.children.length : index, 0, child);
}

function remove(parent: { children: HostChild[] }, child: HostChild): void {
  const index = parent.children.indexOf(child);
  if (index >= 0) parent.children.splice(index, 1);
}

function toUiNode(child: HostChild): UiNode {
  if (child.kind === "text") return { kind: "text", id: child.id, text: child.hidden ? "" : child.text };
  return {
    kind: "element",
    id: child.id,
    type: child.type,
    props: { ...child.props, ...(child.hidden ? { hidden: true } : {}) },
    children: child.children.map(toUiNode),
    ...(child.fallback === undefined ? {} : { fallback: child.fallback }),
    ...(child.requires === undefined ? {} : { requires: child.requires }),
  };
}

function commit(container: HostContainer): void {
  const root: UiNode = container.children.length === 1
    ? toUiNode(container.children[0] as HostChild)
    : { kind: "element", id: `${container.idPrefix}-root`, type: "Stack", props: {}, children: container.children.map(toUiNode) };
  const revision = container.previous === undefined ? 0 : container.revision + 1;
  const document = createDocument(root, { documentId: container.documentId, revision, idPrefix: container.idPrefix });
  assertValidDocument(document, container.limits);
  container.handlers.clear();
  const indexHandlers = (child: HostChild): void => {
    if (child.kind === "element") {
      if (Object.keys(child.handlers).length > 0) container.handlers.set(child.id, child.handlers);
      child.children.forEach(indexHandlers);
    }
  };
  container.children.forEach(indexHandlers);
  const message: UiHostMessage = container.previous === undefined
    ? { type: "snapshot", document }
    : { type: "patch", batch: diffDocuments(container.previous, document) };
  // React may commit handler/context changes that do not alter the serialized
  // semantic tree. Those stay local to the worker: emitting an empty batch
  // would advance a revision with no mutation and is rejected by trusted hosts.
  if (message.type === "patch" && message.batch.patches.length === 0) return;
  container.revision = revision;
  container.current = document;
  container.previous = document;
  container.listeners.forEach((listener) => listener(message));
}

let currentPriority = DefaultEventPriority;
const HOST_CONTEXT = {} as const;

const hostConfig = {
  rendererVersion: "0.1.0",
  rendererPackageName: "@codypendent/ui",
  isPrimaryRenderer: false,
  warnsIfNotActing: false,
  supportsMutation: true,
  supportsPersistence: false,
  supportsHydration: false,
  supportsResources: false,
  supportsSingletons: false,
  supportsTestSelectors: false,
  getRootHostContext: () => HOST_CONTEXT,
  getChildHostContext: () => HOST_CONTEXT,
  getPublicInstance: (instance: HostChild) => instance,
  prepareForCommit: () => null,
  resetAfterCommit: (container: HostContainer) => commit(container),
  preparePortalMount: () => undefined,
  createInstance: (type: string, props: Readonly<Record<string, unknown>>, container: HostContainer): HostInstance => ({
    kind: "element", id: allocateId(container, props), type, ...parseProps(props), children: [], hidden: false,
  }),
  createTextInstance: (text: string, container: HostContainer): HostText => ({ kind: "text", id: allocateId(container, {}), text, hidden: false }),
  appendInitialChild: append,
  finalizeInitialChildren: () => false,
  shouldSetTextContent: () => false,
  scheduleTimeout: setTimeout,
  cancelTimeout: clearTimeout,
  noTimeout: -1,
  supportsMicrotasks: true,
  scheduleMicrotask: queueMicrotask,
  setCurrentUpdatePriority: (priority: number) => { currentPriority = priority; },
  getCurrentUpdatePriority: () => currentPriority,
  resolveUpdatePriority: () => currentPriority || DefaultEventPriority,
  shouldAttemptEagerTransition: () => false,
  detachDeletedInstance: () => undefined,
  maySuspendCommit: () => false,
  preloadInstance: () => true,
  startSuspendingCommit: () => undefined,
  suspendInstance: () => undefined,
  waitForCommitToBeReady: () => null,
  NotPendingTransition: null,
  HostTransitionContext: null,
  resetFormInstance: () => undefined,
  appendChild: append,
  appendChildToContainer: append,
  insertBefore: insert,
  insertInContainerBefore: insert,
  removeChild: remove,
  removeChildFromContainer: remove,
  clearContainer: (container: HostContainer) => { container.children = []; },
  commitUpdate: (instance: HostInstance, type: string, _oldProps: Readonly<Record<string, unknown>>, newProps: Readonly<Record<string, unknown>>) => {
    const parsed = parseProps(newProps);
    instance.type = type;
    instance.props = parsed.props;
    instance.handlers = parsed.handlers;
    delete instance.fallback;
    delete instance.requires;
    if (parsed.fallback !== undefined) instance.fallback = parsed.fallback;
    if (parsed.requires !== undefined) instance.requires = parsed.requires;
  },
  commitTextUpdate: (instance: HostText, _oldText: string, newText: string) => { instance.text = newText; },
  commitMount: () => undefined,
  resetTextContent: (instance: HostInstance) => { instance.children = []; },
  hideInstance: (instance: HostInstance) => { instance.hidden = true; },
  unhideInstance: (instance: HostInstance) => { instance.hidden = false; },
  hideTextInstance: (instance: HostText) => { instance.hidden = true; },
  unhideTextInstance: (instance: HostText) => { instance.hidden = false; },
};

interface OpaqueRoot {}
interface RendererApi {
  createContainer(...args: unknown[]): OpaqueRoot;
  updateContainer(element: ReactNode, root: OpaqueRoot, parent: null, callback?: (() => void) | null): unknown;
  updateContainerSync(element: ReactNode, root: OpaqueRoot, parent: null, callback?: (() => void) | null): unknown;
  flushSyncWork(): void;
  batchedUpdates<T, R>(callback: (value: T) => R, value: T): R;
}

const reconciler = (ReactReconciler as unknown as (config: object) => RendererApi)(hostConfig);

export interface ReactUiRootOptions {
  documentId: string;
  idPrefix?: string;
  limits?: Partial<UiLimits>;
  strictMode?: boolean;
  onMessage?: (message: UiHostMessage) => void;
  onError?: (error: unknown) => void;
}

export interface ReactUiRoot {
  render(children: ReactNode): void;
  unmount(): void;
  getDocument(): UiDocument | undefined;
  dispatch(event: UiEvent): boolean;
  subscribe(listener: (message: UiHostMessage) => void): () => void;
}

/** A pinned React 19 adapter; the rest of `@codypendent/ui` has no React dependency. */
export function createReactUiRoot(options: ReactUiRootOptions): ReactUiRoot {
  const container: HostContainer = {
    documentId: options.documentId,
    idPrefix: options.idPrefix ?? "react-ui",
    revision: 0,
    nextId: 0,
    children: [],
    handlers: new Map(),
    listeners: new Set(options.onMessage === undefined ? [] : [options.onMessage]),
    ...(options.limits === undefined ? {} : { limits: options.limits }),
    onError: options.onError ?? (() => undefined),
  };
  const reportError = (error: unknown): void => container.onError(error);
  const root = reconciler.createContainer(
    container, 1, null, options.strictMode ?? false, null, container.idPrefix,
    reportError, reportError, reportError, null,
  );
  return {
    render(children) {
      reconciler.updateContainerSync(children, root, null);
      reconciler.flushSyncWork();
    },
    unmount() {
      reconciler.updateContainerSync(null, root, null);
      reconciler.flushSyncWork();
      container.listeners.forEach((listener) => listener({ type: "dispose", documentId: container.documentId, revision: container.revision }));
    },
    getDocument: () => container.current === undefined ? undefined : structuredClone(container.current),
    dispatch(event) {
      if (event.protocolVersion.major !== UI_PROTOCOL_VERSION.major || event.documentId !== container.documentId || event.revision !== container.revision) return false;
      const handler = container.handlers.get(event.targetId)?.[event.type];
      if (handler === undefined) return false;
      reconciler.batchedUpdates(handler, event);
      return true;
    },
    subscribe(listener) {
      container.listeners.add(listener);
      return () => container.listeners.delete(listener);
    },
  };
}
