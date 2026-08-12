/** Current semantic UI wire protocol. Minor versions are additive. */
export const UI_PROTOCOL_VERSION = { major: 1, minor: 0 } as const;

export interface UiProtocolVersion {
  major: number;
  minor: number;
}

export type UiClientKind = "terminal" | "web" | "vscode" | "desktop" | "test";
export type ColorDepth = "monochrome" | "ansi16" | "ansi256" | "trueColor";
export type MediaCapability = "image" | "audio" | "video";

/** Host services with complete policy and mediation support in public UI v1. */
export const UI_HOST_CAPABILITIES = [
  "artifact-read", "context-read", "run-read", "workflow-read", "command-invoke",
] as const;

export type UiHostCapability = (typeof UI_HOST_CAPABILITIES)[number] | (string & {});

/** Governed public slots. Security-sensitive host-only slots are intentionally absent. */
export const UI_CONTRIBUTION_POINTS = [
  "sidebar", "panel", "status-item", "command", "command-palette",
  "composer-accessory", "message-renderer", "tool-renderer", "artifact-renderer",
  "workflow-inspector", "blackboard-renderer", "document-block", "code-graph-node",
  "settings-section", "setup-step", "form", "wizard", "dashboard-card",
  "trace-span-renderer", "context-menu", "quick-pick", "notification",
] as const;

export type UiContributionPoint = (typeof UI_CONTRIBUTION_POINTS)[number] | (string & {});

/** Cross-process ceilings negotiated with the trusted worker host. */
export interface UiHardLimits {
  maxTreeDepth: number;
  maxNodes: number;
  maxTextBytes: number;
  maxPropertiesPerNode: number;
  maxActionsPerNode: number;
  maxJsonDepth: number;
  maxJsonValues: number;
  maxPatchesPerBatch: number;
  maxPatchBytes: number;
  maxContributions: number;
}

/**
 * Sustained worker-to-host message rate, in messages per second.
 *
 * A worker's self-imposed budget must never exceed the host's, or a legitimate
 * burst is a *kill* (the host drops the worker on `MessageRateExceeded`)
 * instead of a recoverable local error the worker can coalesce around. These
 * two constants mirror `UI_WORKER_MESSAGE_RATE_PER_SECOND` /
 * `UI_WORKER_MESSAGE_BURST` in `crates/protocol/src/remote_ui.rs`, which is
 * where the trusted host reads its own ceiling from.
 */
export const UI_WORKER_MESSAGE_RATE_PER_SECOND = 240;

/**
 * Burst allowance above {@link UI_WORKER_MESSAGE_RATE_PER_SECOND} for the
 * snapshot-then-patch storm a surface emits when it first mounts.
 */
export const UI_WORKER_MESSAGE_BURST = 120;

export const DEFAULT_UI_HARD_LIMITS: UiHardLimits = {
  maxTreeDepth: 64,
  maxNodes: 20_000,
  maxTextBytes: 2 * 1024 * 1024,
  maxPropertiesPerNode: 256,
  maxActionsPerNode: 64,
  maxJsonDepth: 32,
  maxJsonValues: 100_000,
  maxPatchesPerBatch: 2_000,
  maxPatchBytes: 4 * 1024 * 1024,
  maxContributions: 1_000,
};

/** Mirrors `crates/protocol::ClientCapabilities` exactly at the daemon boundary. */
export interface ClientCapabilities {
  rich_text: boolean;
  image_display: boolean;
  audio_capture: boolean;
  editor_mutations: boolean;
  diff_view: boolean;
  mouse: boolean;
  unicode: boolean;
  true_color: boolean;
}

/** Negotiated presentation features used by the semantic UI host. */
export interface UiCapabilities {
  client: UiClientKind;
  protocolVersions: UiProtocolVersion[];
  daemon: ClientCapabilities;
  primitives: readonly string[] | "*";
  media: readonly MediaCapability[];
  colorDepth: ColorDepth;
  keyboard: boolean;
  screenReader: boolean;
  reducedMotion: boolean;
  clipboard: boolean;
  terminalGraphics?: readonly ("kitty" | "iterm2" | "sixel")[];
  viewport: UiViewport;
  capabilities?: readonly UiHostCapability[];
  contributionPoints?: readonly UiContributionPoint[];
  limits?: UiHardLimits;
}

export interface UiCapabilitySelection {
  protocolVersion: UiProtocolVersion;
  primitives: readonly string[];
  capabilities: readonly UiHostCapability[];
  contributionPoints: readonly UiContributionPoint[];
  imageProtocols: readonly string[];
  colorDepth: number;
  unicode: boolean;
  mouse: boolean;
  screenReader: boolean;
  viewport?: UiViewport;
  limits: UiHardLimits;
}

export interface UiViewport {
  width: number;
  height: number;
  pixelWidth?: number;
  pixelHeight?: number;
  density?: number;
}

/** Single-source built-in primitive catalogue used by types, hosts, and tooling. */
export const UI_PRIMITIVES = [
  "Box", "Stack", "Row", "Grid", "Split", "Spacer", "ScrollArea", "VirtualList",
  "Text", "Markdown", "Code", "Diff", "Image", "Audio", "JsonTree", "LogViewer",
  "List", "Table", "Tree", "KeyValue", "Timeline", "Graph", "Chart", "Sparkline",
  "Badge", "Progress", "Spinner", "Alert", "Toast", "EmptyState", "ErrorBoundary",
  "Tabs", "Breadcrumb", "Menu", "CommandList", "Pagination", "Link", "Details",
  "TextInput", "TextArea", "Select", "MultiSelect", "Checkbox", "Radio", "Form",
  "Button", "ActionMenu", "Toolbar", "ContextMenu",
  "ToolCard", "ArtifactCard", "ApprovalCard", "AgentCard", "WorkflowNode",
  "PatchCard", "TestReport", "PermissionDiff", "TraceView", "CostView",
  "TerminalOnly", "WebOnly",
] as const;

export type UiPrimitive = (typeof UI_PRIMITIVES)[number];

export type UiJsonPrimitive = string | number | boolean | null;
export type UiJsonValue = UiJsonPrimitive | UiJsonValue[] | { readonly [key: string]: UiJsonValue };
export type UiProps = Readonly<Record<string, UiJsonValue | undefined>>;

export interface UiRequirement {
  feature:
    | "richText" | "imageDisplay" | "audioCapture" | "editorMutations" | "diffView"
    | "mouse" | "unicode" | "trueColor" | "keyboard" | "screenReader" | "clipboard"
    | "terminal" | "web";
  optional?: boolean;
}

export interface UiTextNode {
  kind: "text";
  id?: string;
  text: string;
}

export interface UiElementNode<P extends UiProps = UiProps> {
  kind: "element";
  id?: string;
  /** Built-ins use `UiPrimitive`; plugins use a namespaced value such as `acme.TraceFlamegraph`. */
  type: UiPrimitive | (string & {});
  props: P;
  children: UiNode[];
  fallback?: UiNode;
  requires?: UiRequirement[];
}

/** Reserved flat prop emitted by the React worker for worker-local handlers. */
export const UI_EVENT_HANDLERS_PROP = "eventHandlers" as const;

export type UiNode = UiTextNode | UiElementNode;

export interface UiDocumentMetadata {
  title?: string;
  source?: string;
  contributionId?: string;
  traceId?: string;
  hotReloadGeneration?: number;
  labels?: Readonly<Record<string, string>>;
}

export interface UiFallback {
  plainText?: string;
  replacement?: UiNode;
  behavior?: string;
}

export interface UiCompatibility {
  minimumProtocol?: UiProtocolVersion;
  requiredPrimitives?: readonly string[];
  requiredCapabilities?: readonly UiHostCapability[];
  fallback?: UiFallback;
}

/** Full, authoritative semantic tree. */
export interface UiDocument {
  protocolVersion: UiProtocolVersion;
  documentId: string;
  revision: number;
  root: UiNode;
  capabilities?: UiCapabilities;
  metadata?: UiDocumentMetadata;
  compatibility?: UiCompatibility;
}

export type UiPatch = (
  | { op: "replaceRoot"; node: UiNode }
  | { op: "insert"; parentId: string; index: number; node: UiNode }
  | { op: "remove"; nodeId: string }
  | { op: "replace"; nodeId: string; node: UiNode }
  | { op: "updateProps"; nodeId: string; set: UiProps; unset?: string[] }
  | { op: "setText"; nodeId: string; text: string }
  | { op: "move"; nodeId: string; parentId: string; index: number }
) & { payload?: UiJsonValue };

/** Atomic incremental update. Hosts reject it unless `baseRevision` matches. */
export interface UiPatchBatch {
  protocolVersion: UiProtocolVersion;
  documentId: string;
  baseRevision: number;
  revision: number;
  patches: UiPatch[];
  issuedAt?: string;
  atomic: true;
  fallback?: UiFallback;
}

export const UI_EVENT_TYPES = [
  "action", "press", "change", "submit", "focus", "blur", "select",
  "expand", "collapse", "navigate", "scroll", "key", "custom",
] as const;

export type UiEventType = (typeof UI_EVENT_TYPES)[number];

export interface UiEventModifiers {
  alt?: boolean;
  control?: boolean;
  meta?: boolean;
  shift?: boolean;
}

/** Revision-bound user input. A host must discard actions from a stale tree. */
export interface UiEvent<T extends UiJsonValue = UiJsonValue> {
  protocolVersion: UiProtocolVersion;
  eventId: string;
  documentId: string;
  revision: number;
  targetId: string;
  type: UiEventType;
  payload?: T;
  modifiers?: UiEventModifiers;
  timestamp?: string;
  /** Opaque one-shot authority added only by the host on the owner-bound copy. */
  interactionToken?: string;
}

/** Revision-bound, host-authorized command intent produced by a component. */
export interface UiActionInvocation<T extends UiJsonValue = UiJsonValue> {
  invocationId: string;
  documentId: string;
  revision: number;
  sourceNodeId: string;
  actionId: string;
  payload?: T;
  formData?: Readonly<Record<string, UiJsonValue>>;
  interactionToken?: string;
  interactionEventType?: UiEventType;
}

/** A request for a least-authority, host-mediated state projection. */
export interface UiProjectionSubscription {
  subscriptionId: string;
  kind: string;
  resourceId?: string;
  parameters?: Readonly<Record<string, UiJsonValue>>;
}

export interface UiProjectionUnsubscription {
  subscriptionId: string;
}

/** Latest-wins state for one projection subscription. */
export interface UiProjectionUpdate<T extends UiJsonValue = UiJsonValue> {
  subscriptionId: string;
  revision?: number;
  removed?: boolean;
  value?: T;
}

export interface UiActionCancellation {
  invocationId: string;
}

export interface UiRemoteError {
  code: string;
  message: string;
  recoverable?: boolean;
  documentId?: string;
  nodeId?: string;
  patchIndex?: number;
  recovery?: string;
  details?: UiJsonValue;
  fallback?: UiFallback;
}

export interface UiActionResult<T extends UiJsonValue = UiJsonValue> {
  invocationId: string;
  status: "succeeded" | "failed" | "cancelled" | (string & {});
  value?: T;
  error?: UiRemoteError;
}

export interface UiTheme {
  id: string;
  name: string;
  revision: number;
  colorScheme?: string;
  highContrast?: boolean;
  reducedMotion?: boolean;
  tokens?: Readonly<Record<string, UiJsonValue>>;
}

export interface UiContributionRegistration {
  id: string;
  extensionId: string;
  point: UiContributionPoint;
  slot: string;
  documentId: string;
  priority?: number;
  when?: string;
  requires?: readonly UiHostCapability[];
  metadata?: Readonly<Record<string, UiJsonValue>>;
}

export interface UiDispose { documentId: string; revision: number; }
export interface UiResyncRequest { documentId: string; knownRevision?: number; }
export interface UiHotReload { generation: number; changedModules: string[]; }

/** Canonical daemon/worker envelope. `type` and its typed payload must agree. */
export interface UiWireExtensions { extensions?: Readonly<Record<string, UiJsonValue>>; }
export type UiWireMessage = UiWireExtensions & (
  | { type: "snapshot"; messageId: string; snapshot: { document: UiDocument; reason?: string } }
  | { type: "patchBatch"; messageId: string; patchBatch: UiPatchBatch }
  | { type: "event"; messageId: string; event: UiEvent }
  | { type: "action"; messageId: string; action: UiActionInvocation }
  | { type: "subscription"; messageId: string; subscription: UiProjectionSubscription }
  | { type: "unsubscribe"; messageId: string; unsubscription: UiProjectionUnsubscription }
  | { type: "projection"; messageId: string; projection: UiProjectionUpdate }
  | { type: "actionResult"; messageId: string; actionResult: UiActionResult }
  | { type: "cancelAction"; messageId: string; cancellation: UiActionCancellation }
  | { type: "dispose"; messageId: string; dispose: UiDispose }
  | { type: "viewport"; messageId: string; viewport: UiViewport }
  | { type: "resync"; messageId: string; resync: UiResyncRequest }
  | { type: "hotReload"; messageId: string; hotReload: UiHotReload }
  | { type: "capabilities"; messageId: string; capabilities: UiCapabilities }
  | { type: "capabilitySelection"; messageId: string; selection: UiCapabilitySelection }
  | { type: "contributions"; messageId: string; contributions: readonly UiContributionRegistration[]; extensions: Readonly<{ contributionOwner: string } & Record<string, UiJsonValue>> }
  | { type: "theme"; messageId: string; theme: UiTheme }
  | { type: "error"; messageId: string; error: UiRemoteError }
  | { type: `host.${string}` | `worker.${string}`; messageId: string; extensions: Readonly<Record<string, UiJsonValue>> }
);

export type UiHostMessage =
  | { type: "snapshot"; document: UiDocument }
  | { type: "patch"; batch: UiPatchBatch }
  | { type: "dispose"; documentId: string; revision: number }
  | { type: "action"; action: UiActionInvocation }
  | { type: "subscription"; subscription: UiProjectionSubscription }
  | { type: "unsubscribe"; unsubscription: UiProjectionUnsubscription }
  | { type: "cancelAction"; cancellation: UiActionCancellation }
  | { type: "error"; documentId?: string; code: string; message: string };

export type UiRuntimeMessage =
  | { type: "event"; event: UiEvent }
  | { type: "projection"; projection: UiProjectionUpdate }
  | { type: "actionResult"; result: UiActionResult }
  | { type: "capabilities"; capabilities: UiCapabilities }
  | { type: "viewport"; viewport: UiViewport }
  | { type: "resync"; documentId: string; knownRevision?: number }
  | { type: "hotReload"; generation: number; changedModules: string[] };

export const MINIMAL_TERMINAL_CAPABILITIES: UiCapabilities = {
  client: "terminal",
  protocolVersions: [UI_PROTOCOL_VERSION],
  daemon: {
    rich_text: false,
    image_display: false,
    audio_capture: false,
    editor_mutations: false,
    diff_view: false,
    mouse: false,
    unicode: false,
    true_color: false,
  },
  primitives: ["Box", "Stack", "Row", "Text", "List", "Badge", "Alert", "Link", "Button"],
  media: [],
  colorDepth: "monochrome",
  keyboard: true,
  screenReader: false,
  reducedMotion: true,
  clipboard: false,
  viewport: { width: 80, height: 24 },
};
