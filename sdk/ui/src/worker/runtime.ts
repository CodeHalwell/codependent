import {
  DEFAULT_UI_HARD_LIMITS,
  type UiCapabilities,
  type UiCapabilitySelection,
  type UiContributionPoint,
  type UiContributionRegistration,
  type UiDocument,
  type UiEvent,
  type UiHardLimits,
  type UiNode,
  type UiWireMessage,
} from "../protocol.js";
import { createDocument, diffDocuments } from "../document.js";
import type { HotReloadStateStore } from "../hot-reload.js";
import { MediatedUiBridge, type UiActionContext, type UiWorkerSend } from "./bridge.js";
import { assertUiWireMessage, protocolMatchesSelection } from "./wire.js";

export interface UiWorkerTransport {
  readonly incoming: AsyncIterable<unknown>;
  send(message: UiWireMessage): Promise<void>;
  close?(): Promise<void>;
}

export interface UiSurfaceController {
  readonly documentId: string;
  getDocument(): UiDocument | undefined;
  dispatch(event: UiEvent): boolean;
  render(): void;
  dispose(): void;
}

export interface UiSurfaceContext {
  bridge: MediatedUiBridge;
  selection: UiCapabilitySelection;
  send: UiWorkerSend;
  fail(cause: unknown): void;
}

export interface UiSurfaceFactory {
  readonly documentId: string;
  mount(context: UiSurfaceContext): UiSurfaceController;
}

export interface PureUiSurfaceOptions {
  documentId: string;
  idPrefix?: string;
  render(bridge: MediatedUiBridge): UiNode;
  /** Handle a declared `localEvents` event. Returning true rerenders the surface. */
  onEvent?(event: UiEvent, bridge: MediatedUiBridge): boolean;
  onError?: (cause: unknown) => void;
}

function remoteError(cause: unknown, code = "ui.worker.failure"): UiWireMessage {
  return {
    type: "error",
    messageId: `worker-error-${Date.now()}`,
    error: {
      code,
      message: cause instanceof Error ? cause.message : String(cause),
      recoverable: false,
    },
  };
}

export function createPureUiSurface(options: PureUiSurfaceOptions): UiSurfaceFactory {
  return {
    documentId: options.documentId,
    mount(context) {
      let sequence = 0;
      let document: UiDocument | undefined;
      const render = (): void => {
        try {
          const next = createDocument(options.render(context.bridge), {
            documentId: options.documentId,
            revision: document === undefined ? 0 : document.revision + 1,
            idPrefix: options.idPrefix ?? "pure-ui",
          });
          sequence += 1;
          if (document === undefined) {
            document = next;
            void context.send({ type: "snapshot", messageId: `${options.documentId}-${sequence}`, snapshot: { document: next } }).catch(context.fail);
          } else {
            const batch = diffDocuments(document, next);
            if (batch.patches.length > 0) {
              document = next;
              void context.send({ type: "patchBatch", messageId: `${options.documentId}-${sequence}`, patchBatch: batch }).catch(context.fail);
            }
          }
        } catch (cause) { options.onError?.(cause); context.fail(cause); }
      };
      render();
      return {
        documentId: options.documentId,
        getDocument: () => document === undefined ? undefined : structuredClone(document),
        dispatch: (event) => {
          if (document === undefined || event.documentId !== document.documentId || event.revision !== document.revision) return false;
          const handled = options.onEvent?.(event, context.bridge) === true;
          if (handled) render();
          return handled;
        },
        render,
        dispose: () => { document = undefined; },
      };
    },
  };
}

export interface UiWorkerRuntimeOptions {
  capabilityOffer: UiCapabilities;
  surfaces: readonly UiSurfaceFactory[];
  /** Manifest-declared registrations. `renderer` is carried as inert metadata; `documentId` must name a surface. */
  contributions?: readonly UiWorkerContribution[];
  pluginId?: string;
  sessionId?: string;
  handshakeTimeoutMs?: number;
  maximumMessages?: number;
  messagesPerSecond?: number;
  messageBurst?: number;
  actionTimeoutMs?: number;
  /** Opt-in JSON state transferred by the development workbench. */
  hotReloadState?: HotReloadStateStore;
  onHotReload?(generation: number, changedModules: readonly string[]): void | Promise<void>;
  onDiagnostic?(event: { direction: "in" | "out"; message: UiWireMessage }): void;
  onError?(cause: unknown): void;
}

export interface UiWorkerContribution {
  id: string;
  point: UiContributionPoint;
  renderer: string;
  documentId: string;
  /** Host-owned slot id; defaults to the canonical contribution point. */
  slot?: string;
  priority?: number;
  when?: string;
  requires?: readonly import("../protocol.js").UiHostCapability[];
  metadata?: Readonly<Record<string, import("../protocol.js").UiJsonValue>>;
}

/** Complete worker lifecycle and protocol state machine. */
export class UiWorkerRuntime {
  #state: "created" | "handshake" | "ready" | "disposing" | "disposed" = "created";
  #sequence = 0;
  #sent = 0;
  #recent: number[] = [];
  #selection?: UiCapabilitySelection;
  #bridge?: MediatedUiBridge;
  #surfaces = new Map<string, UiSurfaceController>();
  #fatal?: unknown;

  constructor(private readonly transport: UiWorkerTransport, private readonly options: UiWorkerRuntimeOptions) {
    if (new Set(options.surfaces.map((surface) => surface.documentId)).size !== options.surfaces.length) {
      throw new Error("UI worker surface document ids must be unique");
    }
    const contributions = options.contributions ?? [];
    if (contributions.length > 0 && (options.pluginId === undefined || options.pluginId.length === 0)) {
      throw new Error("UI worker contributions require a non-empty pluginId owner");
    }
    const surfaceIds = new Set(options.surfaces.map((surface) => surface.documentId));
    const contributionIds = new Set<string>();
    for (const contribution of contributions) {
      if (contributionIds.has(contribution.id)) throw new Error(`duplicate UI worker contribution id ${JSON.stringify(contribution.id)}`);
      contributionIds.add(contribution.id);
      if (!surfaceIds.has(contribution.documentId)) throw new Error(`UI contribution ${JSON.stringify(contribution.id)} maps to unknown surface ${JSON.stringify(contribution.documentId)}`);
      for (const [field, value] of [["id", contribution.id], ["point", contribution.point], ["renderer", contribution.renderer], ["documentId", contribution.documentId]] as const) {
        if (value.length === 0) throw new Error(`UI contribution ${field} must not be empty`);
      }
      if (contribution.priority !== undefined && !Number.isSafeInteger(contribution.priority)) throw new Error(`UI contribution ${JSON.stringify(contribution.id)} priority must be an integer`);
    }
  }

  get state(): string { return this.#state; }
  get selection(): UiCapabilitySelection | undefined { return this.#selection; }
  get bridge(): MediatedUiBridge | undefined { return this.#bridge; }

  async run(): Promise<void> {
    if (this.#state !== "created") throw new Error("UiWorkerRuntime.run may only be called once");
    this.#state = "handshake";
    const iterator = this.transport.incoming[Symbol.asyncIterator]();
    try {
      const hostOffer = await this.#nextWithTimeout(iterator, this.options.handshakeTimeoutMs ?? 10_000);
      assertUiWireMessage(hostOffer, "handshake");
      if (hostOffer.type !== "capabilities") throw new Error(`expected capabilities, received ${hostOffer.type}`);
      await this.#send({ type: "capabilities", messageId: this.#messageId("capabilities"), capabilities: this.options.capabilityOffer }, true);

      const selected = await this.#nextWithTimeout(iterator, this.options.handshakeTimeoutMs ?? 10_000);
      assertUiWireMessage(selected, "handshake");
      if (selected.type !== "capabilitySelection") throw new Error(`expected capabilitySelection, received ${selected.type}`);
      this.#validateSelection(selected.selection, hostOffer.capabilities);
      this.#selection = selected.selection;
      let activeContext: UiActionContext | undefined;
      const bridge = new MediatedUiBridge((message) => this.#send(message), {
        ...(this.options.pluginId === undefined ? {} : { pluginId: this.options.pluginId }),
        ...(this.options.sessionId === undefined ? {} : { sessionId: this.options.sessionId }),
        capabilities: hostOffer.capabilities,
        ...(this.options.actionTimeoutMs === undefined ? {} : { actionTimeoutMs: this.options.actionTimeoutMs }),
        onError: (cause) => this.#fail(cause),
      });
      this.#bridge = bridge;
      this.#state = "ready";
      await this.#sendControl("worker.ready", { protocolVersion: { major: selected.selection.protocolVersion.major, minor: selected.selection.protocolVersion.minor } }, true);
      for (const factory of this.options.surfaces) {
        const controller = factory.mount({ bridge, selection: selected.selection, send: (message) => this.#send(message), fail: (cause) => this.#fail(cause) });
        this.#surfaces.set(factory.documentId, controller);
      }
      if (this.#fatal !== undefined) throw this.#fatal;
      await this.#sendContributions();

      while (this.#state === "ready") {
        const next = await iterator.next();
        if (next.done) throw new Error("UI host transport ended without host.dispose");
        assertUiWireMessage(next.value, "host-to-worker", selected.selection.limits);
        const message = next.value;
        this.options.onDiagnostic?.({ direction: "in", message });
        if (!protocolMatchesSelection(message, selected.selection.protocolVersion.major, selected.selection.protocolVersion.minor)) {
          throw new Error(`message ${message.type} does not use the negotiated protocol version`);
        }
        if (message.type === "event") {
          const surface = this.#surfaces.get(message.event.documentId);
          if (surface !== undefined) {
            activeContext = {
              documentId: message.event.documentId,
              revision: message.event.revision,
              sourceNodeId: message.event.targetId,
              ...(message.event.interactionToken === undefined ? {} : { interactionToken: message.event.interactionToken }),
              interactionEventType: message.event.type,
            };
            try { bridge.withActionContext(activeContext, () => surface.dispatch(message.event)); } finally { activeContext = undefined; }
          }
        } else if (message.type === "projection") {
          bridge.applyProjection(message.projection);
        } else if (message.type === "actionResult") {
          bridge.applyActionResult(message.actionResult);
        } else if (message.type === "viewport") {
          bridge.updateViewport(message.viewport);
        } else if (message.type === "capabilities") {
          bridge.updateCapabilities(message.capabilities);
        } else if (message.type === "theme") {
          bridge.updateTheme(message.theme);
        } else if (message.type === "resync") {
          const document = this.#surfaces.get(message.resync.documentId)?.getDocument();
          if (document !== undefined) await this.#send({ type: "snapshot", messageId: this.#messageId("resync-snapshot"), snapshot: { document, reason: "host-request" } });
        } else if (message.type === "hotReload") {
          await this.options.onHotReload?.(message.hotReload.generation, message.hotReload.changedModules);
          await this.#sendControl("worker.reloaded", {
            generation: message.hotReload.generation,
            ...(this.options.hotReloadState === undefined
              ? {}
              : { states: this.options.hotReloadState.exportStates() }),
          });
          for (const surface of this.#surfaces.values()) surface.render();
          for (const surface of this.#surfaces.values()) {
            const document = surface.getDocument();
            if (document !== undefined) await this.#send({ type: "snapshot", messageId: this.#messageId("reload-snapshot"), snapshot: { document, reason: "hot-reload" } });
          }
        } else if (message.type === "dispose") {
          const surface = this.#surfaces.get(message.dispose.documentId);
          if (surface?.getDocument()?.revision === message.dispose.revision) {
            surface.dispose();
            this.#surfaces.delete(message.dispose.documentId);
            await this.#sendContributions();
          }
        } else if (message.type === "host.ping") {
          await this.#sendControl("worker.pong", {});
        } else if (message.type === "host.dispose") {
          await this.#shutdown();
        }
        if (this.#fatal !== undefined) throw this.#fatal;
      }
    } catch (cause) {
      this.options.onError?.(cause);
      if ((this.#state as string) !== "disposed") {
        try { await this.#send(remoteError(cause)); } catch { /* transport is already unavailable */ }
        await this.#shutdown(false);
      }
      throw cause;
    }
  }

  #validateSelection(selection: UiCapabilitySelection, host: UiCapabilities): void {
    const offered = this.options.capabilityOffer;
    if (!offered.protocolVersions.some((version) => version.major === selection.protocolVersion.major && version.minor >= selection.protocolVersion.minor)
      || !host.protocolVersions.some((version) => version.major === selection.protocolVersion.major && version.minor >= selection.protocolVersion.minor)) {
      throw new Error("host selected an unoffered protocol version");
    }
    const offeredCapabilities = new Set(offered.capabilities ?? []);
    const hostCapabilities = new Set(host.capabilities ?? []);
    if (selection.capabilities.some((capability) => !offeredCapabilities.has(capability) || !hostCapabilities.has(capability))) throw new Error("host selected a capability not offered by both peers");
    const offeredPoints = new Set(offered.contributionPoints ?? []);
    const hostPoints = new Set(host.contributionPoints ?? []);
    if (selection.contributionPoints.some((point) => !offeredPoints.has(point) || !hostPoints.has(point))) throw new Error("host selected a contribution point not offered by both peers");
    const supportsPrimitive = (capabilities: UiCapabilities, primitive: string): boolean => capabilities.primitives === "*" || capabilities.primitives.includes(primitive);
    if (selection.primitives.some((primitive) => primitive !== "*" && (!supportsPrimitive(offered, primitive) || !supportsPrimitive(host, primitive)))) throw new Error("host selected an unoffered primitive");
    if (selection.primitives.includes("*") && (offered.primitives !== "*" || host.primitives !== "*")) throw new Error("host selected a primitive wildcard not offered by both peers");
    const offeredImages = new Set(offered.terminalGraphics ?? []);
    const hostImages = new Set(host.terminalGraphics ?? []);
    if (selection.imageProtocols.some((protocol) => !offeredImages.has(protocol as never) || !hostImages.has(protocol as never))) throw new Error("host selected an unoffered terminal image protocol");
    const rank = (depth: UiCapabilities["colorDepth"]): number => ({ monochrome: 1, ansi16: 4, ansi256: 8, trueColor: 24 })[depth];
    if (![1, 4, 8, 24].includes(selection.colorDepth) || selection.colorDepth > rank(offered.colorDepth) || selection.colorDepth > rank(host.colorDepth)) throw new Error("host selected an unsupported color depth");
    if (selection.unicode && (!offered.daemon.unicode || !host.daemon.unicode)) throw new Error("host selected unavailable Unicode support");
    if (selection.mouse && (!offered.daemon.mouse || !host.daemon.mouse)) throw new Error("host selected unavailable mouse support");
    if (selection.screenReader && (!offered.screenReader || !host.screenReader)) throw new Error("host selected unavailable screen-reader support");
    if (selection.viewport !== undefined && (!Number.isSafeInteger(selection.viewport.width) || !Number.isSafeInteger(selection.viewport.height)
      || selection.viewport.width <= 0 || selection.viewport.height <= 0 || selection.viewport.width > host.viewport.width || selection.viewport.height > host.viewport.height
      || (selection.viewport.density !== undefined && (!Number.isFinite(selection.viewport.density) || selection.viewport.density <= 0)))) throw new Error("host selected an invalid viewport");
    const workerLimits = offered.limits ?? DEFAULT_UI_HARD_LIMITS;
    const hostLimits = host.limits ?? DEFAULT_UI_HARD_LIMITS;
    for (const field of Object.keys(workerLimits) as Array<keyof UiHardLimits>) {
      if (!Number.isSafeInteger(selection.limits[field]) || selection.limits[field] <= 0
        || selection.limits[field] > workerLimits[field] || selection.limits[field] > hostLimits[field]) {
        throw new Error(`host selected invalid ${field} limit`);
      }
    }
  }

  async #nextWithTimeout(iterator: AsyncIterator<unknown>, timeoutMs: number): Promise<unknown> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      return await Promise.race([
        iterator.next().then((next) => { if (next.done) throw new Error("transport ended during handshake"); return next.value; }),
        new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error(`UI worker handshake timed out after ${timeoutMs}ms`)), timeoutMs); }),
      ]);
    } finally { if (timer !== undefined) clearTimeout(timer); }
  }

  #messageId(prefix: string): string { this.#sequence += 1; return `${prefix}-${this.#sequence}`; }

  async #sendContributions(empty = false): Promise<void> {
    const definitions = this.options.contributions ?? [];
    if (definitions.length === 0) return;
    const owner = this.options.pluginId;
    const selection = this.#selection;
    if (owner === undefined || selection === undefined) throw new Error("cannot register UI contributions before negotiation");
    const activeDocuments = new Set(empty ? [] : this.#surfaces.keys());
    const registrations: UiContributionRegistration[] = definitions
      .filter((definition) => activeDocuments.has(definition.documentId) && selection.contributionPoints.includes(definition.point))
      .filter((definition) => (definition.requires ?? []).every((capability) => selection.capabilities.includes(capability)))
      .map((definition) => ({
        id: definition.id,
        extensionId: owner,
        point: definition.point,
        slot: definition.slot ?? definition.point,
        documentId: definition.documentId,
        ...(definition.priority === undefined ? {} : { priority: definition.priority }),
        ...(definition.when === undefined ? {} : { when: definition.when }),
        ...(definition.requires === undefined ? {} : { requires: definition.requires }),
        metadata: { ...(definition.metadata ?? {}), renderer: definition.renderer },
      }));
    await this.#send({
      type: "contributions",
      messageId: this.#messageId("contributions"),
      contributions: registrations,
      extensions: { contributionOwner: owner },
    });
  }

  async #sendControl(type: `worker.${string}`, control: Record<string, import("../protocol.js").UiJsonValue>, handshake = false): Promise<void> {
    await this.#send({ type, messageId: this.#messageId(type), extensions: { control } }, handshake);
  }

  async #send(message: UiWireMessage, handshake = false): Promise<void> {
    assertUiWireMessage(message, handshake ? "handshake" : "worker-to-host", this.#selection?.limits);
    const now = Date.now();
    this.#recent = this.#recent.filter((time) => now - time < 1_000);
    const rate = this.options.messagesPerSecond ?? 240;
    const burst = this.options.messageBurst ?? 1_000;
    if (this.#recent.length >= rate + burst) throw new Error(`UI worker exceeded ${rate}/s + ${burst} burst message budget`);
    this.#recent.push(now);
    this.#sent += 1;
    if (this.#sent > (this.options.maximumMessages ?? 1_000_000)) throw new Error("UI worker exceeded its lifetime message budget");
    this.options.onDiagnostic?.({ direction: "out", message });
    await this.transport.send(message);
  }

  #fail(cause: unknown): void { this.#fatal ??= cause; }

  async #shutdown(acknowledge = true): Promise<void> {
    if (this.#state === "disposed" || this.#state === "disposing") return;
    this.#state = "disposing";
    if (this.#selection !== undefined && (this.options.contributions?.length ?? 0) > 0) {
      try { await this.#sendContributions(true); } catch (cause) { this.options.onError?.(cause); }
    }
    for (const surface of this.#surfaces.values()) {
      const document = surface.getDocument();
      if (document !== undefined) {
        try {
          await this.#send({
            type: "dispose",
            messageId: this.#messageId("dispose"),
            dispose: { documentId: document.documentId, revision: document.revision },
          });
        } catch (cause) { this.options.onError?.(cause); }
      }
    }
    for (const surface of this.#surfaces.values()) surface.dispose();
    this.#surfaces.clear();
    this.#bridge?.dispose();
    if (acknowledge) await this.#sendControl("worker.disposed", {}, true);
    await this.transport.close?.();
    this.#state = "disposed";
  }
}

export async function runUiWorker(transport: UiWorkerTransport, options: UiWorkerRuntimeOptions): Promise<void> {
  await new UiWorkerRuntime(transport, options).run();
}
