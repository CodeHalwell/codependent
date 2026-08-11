import {
  MINIMAL_TERMINAL_CAPABILITIES,
  UI_PROTOCOL_VERSION,
  type UiActionInvocation,
  type UiActionResult,
  type UiCapabilities,
  type UiJsonValue,
  type UiProjectionSubscription,
  type UiProjectionUpdate,
  type UiTheme,
  type UiViewport,
  type UiWireMessage,
} from "../protocol.js";
import type {
  ArtifactProjectionOptions,
  ArtifactView,
  CommandDescriptor,
  ExternalProjection,
  IdeContextView,
  RunView,
  SessionSummary,
  ThemeTokens,
  WorkflowView,
  UiCommandActions,
  UiProjectionStore,
  UiProviderMeta,
} from "../session.js";

export type UiWorkerSend = (message: UiWireMessage) => Promise<void>;

export interface UiActionContext {
  documentId: string;
  revision: number;
  sourceNodeId: string;
  formData?: Readonly<Record<string, UiJsonValue>>;
  interactionToken?: string;
  interactionEventType?: import("../protocol.js").UiEventType;
}

export class UiActionError extends Error {
  constructor(readonly result: UiActionResult) {
    super(result.error?.message ?? `UI action ${result.status}`);
    this.name = "UiActionError";
  }
}

class ProjectionCell<T> implements ExternalProjection<T> {
  #listeners = new Map<() => void, number>();
  #subscriptionCount = 0;
  #value: T;
  constructor(
    initial: T,
    private readonly onFirstListener: () => void = () => undefined,
    private readonly onLastListener: () => void = () => undefined,
  ) { this.#value = initial; }
  getSnapshot = (): T => this.#value;
  subscribe = (listener: () => void): (() => void) => {
    const first = this.#subscriptionCount === 0;
    this.#listeners.set(listener, (this.#listeners.get(listener) ?? 0) + 1);
    this.#subscriptionCount += 1;
    if (first) this.onFirstListener();
    let active = true;
    return () => {
      if (!active) return;
      active = false;
      const count = this.#listeners.get(listener) ?? 0;
      if (count <= 1) this.#listeners.delete(listener);
      else this.#listeners.set(listener, count - 1);
      this.#subscriptionCount -= 1;
      if (this.#subscriptionCount === 0) this.onLastListener();
    };
  };
  update(value: T): void {
    if (Object.is(this.#value, value)) return;
    this.#value = value;
    this.#listeners.forEach((_count, listener) => listener());
  }
}

interface RegisteredProjection<T> {
  cell: ProjectionCell<T>;
  key: string;
  kind: string;
  resourceId: string;
  parameters: Readonly<Record<string, UiJsonValue>>;
  subscriptionId?: string;
  graceTimer?: ReturnType<typeof setTimeout>;
  decode(value: UiJsonValue | undefined, removed: boolean): T;
  lastRevision: number | undefined;
}

interface PendingAction {
  resolve(value: UiJsonValue): void;
  reject(cause: unknown): void;
  timeout: ReturnType<typeof setTimeout>;
  removeAbort?: () => void;
}

function clone<T>(value: T): T { return structuredClone(value); }

function object(value: UiJsonValue | undefined): Readonly<Record<string, UiJsonValue>> | undefined {
  return value !== null && value !== undefined && !Array.isArray(value) && typeof value === "object" ? value : undefined;
}

function stringField(value: Readonly<Record<string, UiJsonValue>>, field: string): string | undefined {
  return typeof value[field] === "string" ? value[field] : undefined;
}

const DEFAULT_THEME: ThemeTokens = { id: "host", mode: "monochrome", colors: {}, spacing: {} };
const UNSUBSCRIBE_GRACE_MS = 50;

/**
 * Least-authority implementation behind UiProvider. It exposes immutable JSON
 * projections and command intents only; no process, filesystem, network, secret,
 * database, or daemon client object crosses this boundary.
 */
export class MediatedUiBridge implements UiProjectionStore, UiCommandActions {
  readonly meta: UiProviderMeta;
  #sequence = 0;
  #projections = new Map<string, RegisteredProjection<unknown>>();
  #keys = new Map<string, RegisteredProjection<unknown>>();
  #pending = new Map<string, PendingAction>();
  #context: UiActionContext | undefined;
  #theme = new ProjectionCell<ThemeTokens>(DEFAULT_THEME);
  #viewport: ProjectionCell<UiViewport>;
  #capabilities: ProjectionCell<UiCapabilities>;

  constructor(
    private readonly send: UiWorkerSend,
    options: {
      clientId?: string;
      sessionId?: string;
      pluginId?: string;
      hotReloadGeneration?: number;
      capabilities?: UiCapabilities;
      actionTimeoutMs?: number;
      onError?: (cause: unknown) => void;
    } = {},
  ) {
    const capabilities = clone(options.capabilities ?? MINIMAL_TERMINAL_CAPABILITIES);
    this.#viewport = new ProjectionCell(capabilities.viewport);
    this.#capabilities = new ProjectionCell(capabilities);
    this.actionTimeoutMs = options.actionTimeoutMs ?? 120_000;
    this.onError = options.onError ?? (() => undefined);
    this.meta = {
      clientId: options.clientId ?? "remote-ui-worker",
      hotReloadGeneration: options.hotReloadGeneration ?? 0,
      ...(options.sessionId === undefined ? {} : { sessionId: options.sessionId }),
      ...(options.pluginId === undefined ? {} : { pluginId: options.pluginId }),
    };
  }

  private readonly actionTimeoutMs: number;
  private readonly onError: (cause: unknown) => void;

  session(id: string): ExternalProjection<SessionSummary | undefined> {
    return this.#projection("session", id, undefined, (value, removed) => {
      if (removed) return undefined;
      const entry = object(value);
      const state = entry === undefined ? undefined : stringField(entry, "state");
      if (entry === undefined || state === undefined) return undefined;
      const title = stringField(entry, "title");
      const activeRunId = stringField(entry, "activeRunId");
      const updatedAt = stringField(entry, "updatedAt");
      return {
        id: stringField(entry, "id") ?? id,
        state,
        ...(title === undefined ? {} : { title }),
        ...(activeRunId === undefined ? {} : { activeRunId }),
        ...(updatedAt === undefined ? {} : { updatedAt }),
      };
    });
  }

  run(id: string): ExternalProjection<RunView | undefined> {
    return this.#projection("run", id, undefined, (value, removed) => {
      if (removed) return undefined;
      const entry = object(value);
      const sessionId = entry === undefined ? undefined : stringField(entry, "sessionId");
      const state = entry === undefined ? undefined : stringField(entry, "state");
      if (entry === undefined || sessionId === undefined || state === undefined) return undefined;
      const progress = typeof entry.progress === "number" ? entry.progress : undefined;
      const cost = typeof entry.cost === "number" ? entry.cost : undefined;
      const agentMode = stringField(entry, "agentMode");
      const startedAt = stringField(entry, "startedAt");
      const completedAt = stringField(entry, "completedAt");
      return {
        id: stringField(entry, "id") ?? id, sessionId, state,
        ...(agentMode === undefined ? {} : { agentMode }),
        ...(progress === undefined ? {} : { progress }),
        ...(cost === undefined ? {} : { cost }),
        ...(startedAt === undefined ? {} : { startedAt }),
        ...(completedAt === undefined ? {} : { completedAt }),
        ...(entry.data === undefined ? {} : { data: clone(entry.data) }),
      };
    });
  }

  context(sessionId: string): ExternalProjection<IdeContextView | undefined> {
    return this.#projection("context", sessionId, undefined, (value, removed) => {
      if (removed) return undefined;
      const entry = object(value);
      if (entry === undefined) return undefined;
      const diagnosticsRevision = entry.diagnosticsRevision;
      if (!Number.isSafeInteger(diagnosticsRevision)) return undefined;
      const activeFile = stringField(entry, "activeFile");
      const openFiles = Array.isArray(entry.openFiles)
        ? entry.openFiles.filter((item): item is string => typeof item === "string")
        : [];
      const dirtyBuffers = Array.isArray(entry.dirtyBuffers) ? clone(entry.dirtyBuffers) : [];
      return {
        ...(activeFile === undefined ? {} : { activeFile }),
        ...(entry.selection === undefined ? {} : { selection: clone(entry.selection) }),
        openFiles,
        dirtyBuffers,
        diagnosticsRevision: diagnosticsRevision as number,
      };
    });
  }

  workflow(id: string): ExternalProjection<WorkflowView | undefined> {
    return this.#projection("workflow", id, undefined, (value, removed) => {
      if (removed) return undefined;
      const entry = object(value);
      const workflowRunId = entry === undefined ? undefined : stringField(entry, "workflowRunId");
      const phase = entry === undefined ? undefined : stringField(entry, "phase");
      if (entry === undefined || workflowRunId === undefined || phase === undefined || !Array.isArray(entry.nodes)) return undefined;
      const nodes = entry.nodes.flatMap((value) => {
        const node = object(value);
        if (node === undefined) return [];
        const nodeWorkflowRunId = stringField(node, "workflowRunId");
        const nodeId = stringField(node, "nodeId");
        const state = stringField(node, "state");
        if (nodeWorkflowRunId === undefined || nodeId === undefined || state === undefined || !Number.isSafeInteger(node.attempt)) return [];
        const error = stringField(node, "error");
        return [{
          workflowRunId: nodeWorkflowRunId,
          nodeId,
          state,
          attempt: node.attempt as number,
          ...(node.cost === undefined ? {} : { cost: clone(node.cost) }),
          ...(error === undefined ? {} : { error }),
          warnings: Array.isArray(node.warnings) ? node.warnings.filter((item): item is string => typeof item === "string") : [],
        }];
      });
      return { workflowRunId, phase, nodes };
    });
  }

  artifact<T extends UiJsonValue = UiJsonValue>(id: string, options: ArtifactProjectionOptions = {}): ExternalProjection<ArtifactView<T> | undefined> {
    return this.#projection("artifact", id, undefined, (value, removed) => {
      if (removed) return undefined;
      const entry = object(value);
      const mediaType = entry === undefined ? undefined : stringField(entry, "mediaType");
      const revision = entry?.revision;
      if (entry === undefined || mediaType === undefined || !Number.isSafeInteger(revision) || entry.value === undefined) return undefined;
      const schema = stringField(entry, "schema");
      const title = stringField(entry, "title");
      return {
        id: stringField(entry, "id") ?? id, mediaType, revision: revision as number, value: clone(entry.value) as T,
        ...(schema === undefined ? {} : { schema }),
        ...(title === undefined ? {} : { title }),
      };
    }, Object.fromEntries(Object.entries(options).filter((entry): entry is [string, UiJsonValue] => entry[1] !== undefined)));
  }

  command<TInput extends UiJsonValue = UiJsonValue, TOutput extends UiJsonValue = UiJsonValue>(id: string): ExternalProjection<CommandDescriptor<TInput, TOutput> | undefined> {
    return this.#projection("command", id, undefined, (value, removed) => {
      if (removed) return undefined;
      const entry = object(value);
      const title = entry === undefined ? undefined : stringField(entry, "title");
      if (entry === undefined || title === undefined || typeof entry.enabled !== "boolean") return undefined;
      const disabledReason = stringField(entry, "disabledReason");
      return {
        id: stringField(entry, "id") ?? id,
        title,
        enabled: entry.enabled,
        ...(disabledReason === undefined ? {} : { disabledReason }),
        execute: (input: TInput) => this.invoke<TInput, TOutput>(id, input),
      };
    });
  }

  theme(): ExternalProjection<ThemeTokens> { return this.#theme; }
  viewport(): ExternalProjection<UiViewport> { return this.#viewport; }
  capabilities(): ExternalProjection<UiCapabilities> { return this.#capabilities; }

  #projection<T>(
    kind: string,
    resourceId: string,
    initial: T,
    decode: RegisteredProjection<T>["decode"],
    parameters: Readonly<Record<string, UiJsonValue>> = {},
  ): ExternalProjection<T> {
    const parameterKey = JSON.stringify(Object.entries(parameters).sort(([left], [right]) => left.localeCompare(right)));
    const key = `${kind}\u0000${resourceId}\u0000${parameterKey}`;
    const known = this.#keys.get(key);
    if (known !== undefined) return (known as RegisteredProjection<T>).cell;
    let registration: RegisteredProjection<T>;
    const cell = new ProjectionCell(
      initial,
      () => this.#activateProjection(registration),
      () => this.#scheduleProjectionDeactivation(registration),
    );
    registration = { cell, key, kind, resourceId, parameters, decode, lastRevision: undefined };
    this.#keys.set(key, registration as RegisteredProjection<unknown>);
    return registration.cell;
  }

  #activateProjection(registration: RegisteredProjection<unknown>): void {
    if (registration.graceTimer !== undefined) {
      clearTimeout(registration.graceTimer);
      delete registration.graceTimer;
    }
    this.#keys.set(registration.key, registration);
    if (registration.subscriptionId !== undefined) return;
    this.#sequence += 1;
    const subscriptionId = `subscription-${this.#sequence}`;
    registration.subscriptionId = subscriptionId;
    registration.lastRevision = undefined;
    this.#projections.set(subscriptionId, registration);
    const subscription: UiProjectionSubscription = {
      subscriptionId,
      kind: registration.kind,
      resourceId: registration.resourceId,
      ...(Object.keys(registration.parameters).length === 0 ? {} : { parameters: registration.parameters }),
    };
    void this.send({ type: "subscription", messageId: `subscription-message-${this.#sequence}`, subscription }).catch(this.onError);
  }

  #scheduleProjectionDeactivation(registration: RegisteredProjection<unknown>): void {
    if (registration.graceTimer !== undefined) return;
    registration.graceTimer = setTimeout(() => {
      delete registration.graceTimer;
      this.#deactivateProjection(registration);
    }, UNSUBSCRIBE_GRACE_MS);
  }

  #deactivateProjection(registration: RegisteredProjection<unknown>): void {
    if (registration.graceTimer !== undefined) {
      clearTimeout(registration.graceTimer);
      delete registration.graceTimer;
    }
    const subscriptionId = registration.subscriptionId;
    if (subscriptionId === undefined) {
      if (this.#keys.get(registration.key) === registration) this.#keys.delete(registration.key);
      return;
    }
    delete registration.subscriptionId;
    registration.lastRevision = undefined;
    this.#projections.delete(subscriptionId);
    if (this.#keys.get(registration.key) === registration) this.#keys.delete(registration.key);
    this.#sequence += 1;
    void this.send({
      type: "unsubscribe",
      messageId: `unsubscribe-message-${this.#sequence}`,
      unsubscription: { subscriptionId },
    }).catch(this.onError);
  }

  applyProjection(update: UiProjectionUpdate): boolean {
    const registration = this.#projections.get(update.subscriptionId);
    if (registration === undefined) return false;
    if (registration.lastRevision !== undefined && (update.revision === undefined || update.revision <= registration.lastRevision)) return false;
    if (update.revision !== undefined) registration.lastRevision = update.revision;
    registration.cell.update(registration.decode(update.value, update.removed ?? false));
    return true;
  }

  updateViewport(viewport: UiViewport): void { this.#viewport.update(clone(viewport)); }
  updateCapabilities(capabilities: UiCapabilities): void {
    this.#capabilities.update(clone(capabilities));
    this.#viewport.update(clone(capabilities.viewport));
  }
  updateTheme(theme: UiTheme): void {
    const tokens = theme.tokens ?? {};
    const colors = Object.fromEntries(Object.entries(tokens).filter((item): item is [string, string] => typeof item[1] === "string"));
    const spacing = Object.fromEntries(Object.entries(tokens).filter((item): item is [string, number] => typeof item[1] === "number"));
    const mode: ThemeTokens["mode"] = theme.highContrast === true ? "highContrast"
      : theme.colorScheme === "dark" ? "dark"
        : theme.colorScheme === "light" ? "light" : "monochrome";
    this.#theme.update({ id: theme.id, mode, colors, spacing });
  }

  withActionContext<T>(context: UiActionContext, run: () => T): T {
    const previous = this.#context;
    this.#context = context;
    try { return run(); } finally { this.#context = previous; }
  }

  async invoke<TInput extends UiJsonValue = UiJsonValue, TOutput extends UiJsonValue = UiJsonValue>(
    commandId: string,
    input: TInput,
    options: { signal?: AbortSignal } = {},
  ): Promise<TOutput> {
    if (options.signal?.aborted === true) throw options.signal.reason ?? new DOMException("Action aborted", "AbortError");
    const context = this.#context;
    if (context === undefined) throw new Error("Cannot invoke a command without an active document/action context");
    this.#sequence += 1;
    const invocationId = crypto.randomUUID();
    const action: UiActionInvocation<TInput> = {
      invocationId,
      documentId: context.documentId,
      revision: context.revision,
      sourceNodeId: context.sourceNodeId,
      actionId: commandId,
      payload: input,
      ...(context.formData === undefined ? {} : { formData: context.formData }),
      ...(context.interactionToken === undefined ? {} : { interactionToken: context.interactionToken }),
      ...(context.interactionEventType === undefined ? {} : { interactionEventType: context.interactionEventType }),
    };
    const result = new Promise<TOutput>((resolve, reject) => {
      const timeout = setTimeout(() => {
        void this.cancel(invocationId, new Error(`UI action ${invocationId} timed out after ${this.actionTimeoutMs}ms`)).catch(reject);
      }, this.actionTimeoutMs);
      const pending: PendingAction = { resolve: (value) => resolve(value as TOutput), reject, timeout };
      if (options.signal !== undefined) {
        const abort = (): void => { void this.cancel(invocationId, options.signal?.reason ?? new DOMException("Action aborted", "AbortError")); };
        options.signal.addEventListener("abort", abort, { once: true });
        pending.removeAbort = () => options.signal?.removeEventListener("abort", abort);
      }
      this.#pending.set(invocationId, pending);
    });
    try {
      await this.send({ type: "action", messageId: `action-message-${this.#sequence}`, action });
    } catch (cause) {
      this.#settle(invocationId, undefined, cause);
    }
    return result;
  }

  async cancel(invocationId: string, cause: unknown = new DOMException("Action cancelled", "AbortError")): Promise<void> {
    const pending = this.#pending.get(invocationId);
    if (pending === undefined) throw new Error(`Cannot cancel unknown UI action ${invocationId}`);
    this.#settle(invocationId, undefined, cause);
    this.#sequence += 1;
    await this.send({
      type: "cancelAction",
      messageId: `cancel-action-message-${this.#sequence}`,
      cancellation: { invocationId },
    });
  }

  applyActionResult(result: UiActionResult): boolean {
    const pending = this.#pending.get(result.invocationId);
    if (pending === undefined) return false;
    if (result.status === "succeeded") this.#settle(result.invocationId, result.value ?? null);
    else this.#settle(result.invocationId, undefined, new UiActionError(result));
    return true;
  }

  #settle(invocationId: string, value?: UiJsonValue, cause?: unknown): void {
    const pending = this.#pending.get(invocationId);
    if (pending === undefined) return;
    this.#pending.delete(invocationId);
    clearTimeout(pending.timeout);
    pending.removeAbort?.();
    if (cause === undefined) pending.resolve(value ?? null); else pending.reject(cause);
  }

  dispose(cause: unknown = new Error("UI worker disposed")): void {
    for (const invocationId of [...this.#pending.keys()]) this.#settle(invocationId, undefined, cause);
    for (const registration of new Set(this.#keys.values())) this.#deactivateProjection(registration);
    this.#keys.clear();
  }
}

export function defaultWorkerCapabilities(overrides: Partial<UiCapabilities> = {}): UiCapabilities {
  return {
    ...MINIMAL_TERMINAL_CAPABILITIES,
    client: "test",
    protocolVersions: [UI_PROTOCOL_VERSION],
    primitives: "*",
    ...overrides,
    daemon: { ...MINIMAL_TERMINAL_CAPABILITIES.daemon, ...overrides.daemon },
    viewport: { ...MINIMAL_TERMINAL_CAPABILITIES.viewport, ...overrides.viewport },
  };
}
