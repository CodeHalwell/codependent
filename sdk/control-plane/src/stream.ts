import type {
  StreamEvent,
  StreamName,
  StreamSubscriptionOptions,
  UUID,
} from "./types/index.js";
import type { StreamEvent as GeneratedStreamEvent } from "./generated/stream.js";

export interface StreamClientConfig {
  baseUrl: string;
  token?: string | null | undefined;
  apiKey?: string | null | undefined;
  fetch?: typeof globalThis.fetch | undefined;
  WebSocketClass?: typeof WebSocket | undefined;
  reconnectInitialDelayMs?: number | undefined;
  reconnectMaxDelayMs?: number | undefined;
  maxReconnectAttempts?: number | undefined;
}

export type StreamEventCallback = (event: StreamEvent) => void;
export type StreamErrorCallback = (error: Error) => void;
export type StreamStateCallback = (connected: boolean) => void;

interface ActiveStreamSubscription {
  wireOptions: StreamSubscriptionOptions;
  listeners: Map<symbol, StreamSubscriptionOptions>;
  ws: WebSocket | null;
  connecting: boolean;
  generation: number;
  reconnectAttempt: number;
  reconnectTimer: ReturnType<typeof setTimeout> | null;
}

interface StreamTicketResponse {
  ticket: string;
  expires_at: string;
}

const STREAM_NAMES = new Set<StreamName>([
  "notifications",
  "approvals",
  "schedules",
  "runner-events",
  "policy",
  "sessions",
  "sync",
]);

/**
 * Resumable control-plane streams.
 *
 * Browser WebSockets cannot attach an Authorization header. Each logical
 * stream therefore mints a short-lived, one-time ticket over authenticated
 * HTTPS and opens one query-scoped socket containing only that opaque ticket.
 * Listeners for the exact same scope share that ticket/socket by reference
 * count; different scopes never multiplex on an unauthoritative client frame.
 */
export class ControlPlaneStreamClient {
  private readonly baseUrl: string;
  private token: string | null;
  private apiKey: string | null;
  private readonly customFetch: typeof globalThis.fetch;
  private readonly WebSocketClass: typeof WebSocket;
  private readonly reconnectInitialDelayMs: number;
  private readonly reconnectMaxDelayMs: number;
  private readonly maxReconnectAttempts: number;

  private activeSubscriptions = new Map<string, ActiveStreamSubscription>();
  private lastEventIds = new Map<string, number>();
  private isExplicitlyClosed = false;

  constructor(config: StreamClientConfig) {
    this.baseUrl = config.baseUrl.replace(/^ws/, "http").replace(/\/+$/, "");
    this.token = config.token ?? null;
    this.apiKey = config.apiKey ?? null;
    this.customFetch = config.fetch ?? globalThis.fetch.bind(globalThis);
    this.WebSocketClass =
      config.WebSocketClass ??
      (typeof WebSocket !== "undefined" ? WebSocket : (null as unknown as typeof WebSocket));
    this.reconnectInitialDelayMs = config.reconnectInitialDelayMs ?? 1000;
    this.reconnectMaxDelayMs = config.reconnectMaxDelayMs ?? 30000;
    this.maxReconnectAttempts = config.maxReconnectAttempts ?? Infinity;
  }

  public setToken(token: string | null): void {
    this.token = token;
    this.restartActiveSubscriptions();
  }

  public setApiKey(apiKey: string | null): void {
    this.apiKey = apiKey;
    this.restartActiveSubscriptions();
  }

  public subscribe(options: StreamSubscriptionOptions): () => void {
    this.assertStream(options?.stream);
    const subKey = this.getSubscriptionKey(options);
    const listenerId = Symbol(subKey);
    let subscription = this.activeSubscriptions.get(subKey);

    if (!subscription) {
      subscription = {
        wireOptions: options,
        listeners: new Map(),
        ws: null,
        connecting: false,
        generation: 0,
        reconnectAttempt: 0,
        reconnectTimer: null,
      };
      this.activeSubscriptions.set(subKey, subscription);

      const cursor = this.normalizeCursor(options.cursor);
      if (cursor !== undefined) this.lastEventIds.set(subKey, cursor);
    }
    subscription.listeners.set(listenerId, options);

    if (subscription.ws?.readyState === this.WebSocketClass.OPEN) {
      this.notifyConnected(options);
    } else if (!this.isExplicitlyClosed) {
      void this.connectSubscription(subKey, subscription);
    }

    return () => this.unsubscribeListener(subKey, listenerId);
  }

  /** Remove every listener registered for an exact public subscription key. */
  public unsubscribe(subKey: string): void {
    const subscription = this.activeSubscriptions.get(subKey);
    if (!subscription) return;
    this.activeSubscriptions.delete(subKey);
    this.stopSubscription(subscription, false);
  }

  /** Start any retained subscriptions after an explicit disconnect. */
  public connect(): void {
    if (!this.WebSocketClass) {
      throw new Error("No WebSocket implementation found in current environment");
    }
    this.isExplicitlyClosed = false;
    for (const [subKey, subscription] of this.activeSubscriptions) {
      void this.connectSubscription(subKey, subscription);
    }
  }

  /** Close sockets but retain listener registrations for a later connect. */
  public disconnect(): void {
    this.isExplicitlyClosed = true;
    for (const subscription of this.activeSubscriptions.values()) {
      this.stopSubscription(subscription, true);
    }
  }

  public reconnect(): void {
    this.isExplicitlyClosed = false;
    for (const [subKey, subscription] of this.activeSubscriptions) {
      this.stopSubscription(subscription, true);
      subscription.reconnectAttempt = 0;
      void this.connectSubscription(subKey, subscription);
    }
  }

  public getLastEventId(organizationId: UUID, stream: StreamName): number | undefined {
    this.assertStream(stream);
    const subKey = `${organizationId}:${stream}:all`;
    return this.lastEventIds.get(subKey);
  }

  private restartActiveSubscriptions(): void {
    if (this.isExplicitlyClosed) return;
    for (const [subKey, subscription] of this.activeSubscriptions) {
      this.stopSubscription(subscription, true);
      subscription.reconnectAttempt = 0;
      void this.connectSubscription(subKey, subscription);
    }
  }

  private unsubscribeListener(subKey: string, listenerId: symbol): void {
    const subscription = this.activeSubscriptions.get(subKey);
    if (!subscription || !subscription.listeners.delete(listenerId)) return;
    if (subscription.listeners.size === 0) {
      this.activeSubscriptions.delete(subKey);
      this.stopSubscription(subscription, false);
    }
  }

  private async connectSubscription(
    subKey: string,
    subscription: ActiveStreamSubscription
  ): Promise<void> {
    if (
      this.isExplicitlyClosed ||
      subscription.connecting ||
      subscription.ws?.readyState === this.WebSocketClass.OPEN ||
      this.activeSubscriptions.get(subKey) !== subscription
    ) {
      return;
    }
    if (!this.WebSocketClass) {
      this.broadcastSubscriptionError(
        subscription,
        new Error("No WebSocket implementation found in current environment")
      );
      return;
    }

    subscription.connecting = true;
    const generation = ++subscription.generation;
    try {
      const ticket = await this.mintTicket(subKey, subscription.wireOptions);
      if (!this.isCurrent(subKey, subscription, generation)) return;

      const url = new URL(`${this.baseUrl.replace(/^http/, "ws")}/v1/events/stream`);
      url.searchParams.set("ticket", ticket.ticket);
      const ws = new this.WebSocketClass(url.toString());
      subscription.ws = ws;

      ws.onopen = () => {
        if (!this.isCurrent(subKey, subscription, generation) || subscription.ws !== ws) {
          ws.close();
          return;
        }
        subscription.connecting = false;
        subscription.reconnectAttempt = 0;
        for (const listener of [...subscription.listeners.values()]) {
          this.notifyConnected(listener);
        }
      };

      ws.onmessage = (message) => {
        if (!this.isCurrent(subKey, subscription, generation) || subscription.ws !== ws) return;
        try {
          const raw = typeof message.data === "string" ? message.data : message.data.toString();
          const event = this.parseWireEvent(raw);
          this.handleIncomingEvent(subKey, subscription, event);
        } catch (error) {
          this.broadcastSubscriptionError(
            subscription,
            new Error(`Failed to parse stream event: ${this.errorMessage(error)}`)
          );
        }
      };

      ws.onerror = () => {
        if (!this.isCurrent(subKey, subscription, generation) || subscription.ws !== ws) return;
        this.broadcastSubscriptionError(subscription, new Error("WebSocket encountered an error"));
      };

      ws.onclose = () => {
        if (!this.isCurrent(subKey, subscription, generation) || subscription.ws !== ws) return;
        subscription.ws = null;
        subscription.connecting = false;
        for (const listener of [...subscription.listeners.values()]) {
          this.notifyDisconnected(listener);
        }
        this.scheduleReconnect(subKey, subscription);
      };
    } catch (error) {
      if (!this.isCurrent(subKey, subscription, generation)) return;
      subscription.connecting = false;
      this.broadcastSubscriptionError(
        subscription,
        error instanceof Error ? error : new Error(this.errorMessage(error))
      );
      this.scheduleReconnect(subKey, subscription);
    }
  }

  private async mintTicket(
    subKey: string,
    options: StreamSubscriptionOptions
  ): Promise<StreamTicketResponse> {
    const headers: Record<string, string> = {
      Accept: "application/json",
      "Content-Type": "application/json",
    };
    if (this.token) headers.Authorization = `Bearer ${this.token}`;
    else if (this.apiKey) headers["X-API-Key"] = this.apiKey;

    const lastEventId = this.lastEventIds.get(subKey) ?? this.normalizeCursor(options.cursor);
    const response = await this.customFetch(`${this.baseUrl}/v1/events/ticket`, {
      method: "POST",
      headers,
      body: JSON.stringify({
        organization_id: options.organizationId,
        repository_id: options.repositoryId ?? null,
        stream: options.stream,
        last_event_id: lastEventId,
      }),
    });
    if (!response.ok) {
      const detail = await response.text().catch(() => "");
      throw new Error(
        `Could not create stream ticket (${response.status})${detail ? `: ${detail}` : ""}`
      );
    }
    const ticket = (await response.json()) as Partial<StreamTicketResponse>;
    if (typeof ticket.ticket !== "string" || ticket.ticket.length === 0) {
      throw new Error("Stream ticket response did not contain a ticket");
    }
    return {
      ticket: ticket.ticket,
      expires_at: typeof ticket.expires_at === "string" ? ticket.expires_at : "",
    };
  }

  private stopSubscription(subscription: ActiveStreamSubscription, notify: boolean): void {
    subscription.generation += 1;
    subscription.connecting = false;
    if (subscription.reconnectTimer) {
      clearTimeout(subscription.reconnectTimer);
      subscription.reconnectTimer = null;
    }
    const ws = subscription.ws;
    subscription.ws = null;
    if (notify && ws?.readyState === this.WebSocketClass.OPEN) {
      for (const listener of [...subscription.listeners.values()]) {
        this.notifyDisconnected(listener);
      }
    }
    if (ws) {
      try {
        ws.close();
      } catch {
        // Teardown is best effort; the generation already rejects late frames.
      }
    }
  }

  private scheduleReconnect(subKey: string, subscription: ActiveStreamSubscription): void {
    if (
      this.isExplicitlyClosed ||
      this.activeSubscriptions.get(subKey) !== subscription ||
      subscription.reconnectTimer ||
      subscription.reconnectAttempt >= this.maxReconnectAttempts
    ) {
      return;
    }
    const delay = Math.min(
      this.reconnectInitialDelayMs * 2 ** subscription.reconnectAttempt,
      this.reconnectMaxDelayMs
    );
    subscription.reconnectAttempt += 1;
    subscription.reconnectTimer = setTimeout(() => {
      subscription.reconnectTimer = null;
      void this.connectSubscription(subKey, subscription);
    }, delay);
  }

  private isCurrent(
    subKey: string,
    subscription: ActiveStreamSubscription,
    generation: number
  ): boolean {
    return (
      !this.isExplicitlyClosed &&
      this.activeSubscriptions.get(subKey) === subscription &&
      subscription.generation === generation
    );
  }

  private handleIncomingEvent(
    subKey: string,
    subscription: ActiveStreamSubscription,
    event: StreamEvent
  ): void {
    const options = subscription.wireOptions;
    if (
      event.organizationId !== options.organizationId ||
      event.stream !== options.stream ||
      (options.repositoryId !== undefined && event.repositoryId !== options.repositoryId)
    ) {
      this.broadcastSubscriptionError(
        subscription,
        new Error("Stream delivered an event outside its authorized subscription scope")
      );
      return;
    }

    const lastSeenId = this.lastEventIds.get(subKey) ?? 0;
    if (event.id <= lastSeenId && lastSeenId !== 0) return;
    this.lastEventIds.set(subKey, event.id);

    for (const listener of [...subscription.listeners.values()]) {
      try {
        listener.onEvent(event);
      } catch (error) {
        this.notifyListenerError(
          listener,
          new Error(`Stream event listener failed: ${this.errorMessage(error)}`)
        );
      }
    }
  }

  private parseWireEvent(raw: string): StreamEvent {
    // GeneratedStreamEvent is the authoritative snake-case Rust wire shape;
    // this boundary is the one compatibility adapter into the legacy public
    // camel-case StreamEvent retained by the React bindings.
    const value = JSON.parse(raw) as Partial<GeneratedStreamEvent>;
    if (
      !Number.isSafeInteger(value.id) ||
      (value.id as number) < 0 ||
      typeof value.organization_id !== "string" ||
      typeof value.stream !== "string" ||
      !STREAM_NAMES.has(value.stream as StreamName) ||
      typeof value.created_at !== "string" ||
      typeof value.payload !== "object" ||
      value.payload === null ||
      Array.isArray(value.payload) ||
      (value.repository_id !== undefined &&
        value.repository_id !== null &&
        typeof value.repository_id !== "string")
    ) {
      throw new Error("event does not match the control-plane stream contract");
    }
    return {
      id: value.id as number,
      organizationId: value.organization_id,
      repositoryId: value.repository_id ?? null,
      stream: value.stream as StreamName,
      payload: value.payload,
      createdAt: value.created_at,
    };
  }

  private getSubscriptionKey(options: StreamSubscriptionOptions): string {
    return `${options.organizationId}:${options.stream}:${options.repositoryId ?? "all"}`;
  }

  private assertStream(stream: unknown): asserts stream is StreamName {
    if (typeof stream !== "string" || !STREAM_NAMES.has(stream as StreamName)) {
      throw new TypeError("Stream subscriptions require an explicit supported stream");
    }
  }

  private normalizeCursor(cursor: StreamSubscriptionOptions["cursor"]): number | undefined {
    if (cursor === undefined) return undefined;
    const value = typeof cursor === "number" ? cursor : Number.parseInt(cursor, 10);
    return Number.isSafeInteger(value) && value >= 0 ? value : undefined;
  }

  private broadcastSubscriptionError(
    subscription: ActiveStreamSubscription,
    error: Error
  ): void {
    for (const listener of [...subscription.listeners.values()]) {
      this.notifyListenerError(listener, error);
    }
  }

  private notifyConnected(listener: StreamSubscriptionOptions): void {
    try {
      listener.onConnect?.();
    } catch (error) {
      this.notifyListenerError(
        listener,
        new Error(`Stream connect listener failed: ${this.errorMessage(error)}`)
      );
    }
  }

  private notifyDisconnected(listener: StreamSubscriptionOptions): void {
    try {
      listener.onDisconnect?.();
    } catch (error) {
      this.notifyListenerError(
        listener,
        new Error(`Stream disconnect listener failed: ${this.errorMessage(error)}`)
      );
    }
  }

  private notifyListenerError(listener: StreamSubscriptionOptions, error: Error): void {
    try {
      listener.onError?.(error);
    } catch {
      // Consumer handlers are outside the transport's trust boundary. One
      // throwing must not prevent sibling listeners from being notified.
    }
  }

  private errorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
  }
}
