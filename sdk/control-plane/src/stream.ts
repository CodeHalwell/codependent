import type {
  StreamEvent,
  StreamName,
  StreamSubscriptionOptions,
  UUID,
} from "./types/index.js";

export interface StreamClientConfig {
  baseUrl: string;
  token?: string | null | undefined;
  apiKey?: string | null | undefined;
  WebSocketClass?: typeof WebSocket | undefined;
  reconnectInitialDelayMs?: number | undefined;
  reconnectMaxDelayMs?: number | undefined;
  maxReconnectAttempts?: number | undefined;
}

export type StreamEventCallback = (event: StreamEvent) => void;
export type StreamErrorCallback = (error: Error) => void;
export type StreamStateCallback = (connected: boolean) => void;

export class ControlPlaneStreamClient {
  private readonly baseUrl: string;
  private token: string | null;
  private apiKey: string | null;
  private readonly WebSocketClass: typeof WebSocket;
  private readonly reconnectInitialDelayMs: number;
  private readonly reconnectMaxDelayMs: number;
  private readonly maxReconnectAttempts: number;

  private ws: WebSocket | null = null;
  private activeSubscriptions = new Map<string, StreamSubscriptionOptions>();
  private lastEventIds = new Map<string, number>();
  private reconnectAttempt = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private isExplicitlyClosed = false;
  private isConnecting = false;

  constructor(config: StreamClientConfig) {
    this.baseUrl = config.baseUrl.replace(/^http/, "ws").replace(/\/+$/, "");
    this.token = config.token ?? null;
    this.apiKey = config.apiKey ?? null;
    this.WebSocketClass = config.WebSocketClass ?? (typeof WebSocket !== "undefined" ? WebSocket : (null as unknown as typeof WebSocket));
    this.reconnectInitialDelayMs = config.reconnectInitialDelayMs ?? 1000;
    this.reconnectMaxDelayMs = config.reconnectMaxDelayMs ?? 30000;
    this.maxReconnectAttempts = config.maxReconnectAttempts ?? Infinity;
  }

  public setToken(token: string | null): void {
    this.token = token;
    // Reconnect with new credentials if active
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.reconnect();
    }
  }

  public setApiKey(apiKey: string | null): void {
    this.apiKey = apiKey;
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.reconnect();
    }
  }

  public subscribe(options: StreamSubscriptionOptions): () => void {
    const subKey = this.getSubscriptionKey(options);
    this.activeSubscriptions.set(subKey, options);

    if (options.cursor !== undefined) {
      this.lastEventIds.set(subKey, typeof options.cursor === "number" ? options.cursor : parseInt(options.cursor, 10));
    }

    if (!this.ws || this.ws.readyState === WebSocket.CLOSED) {
      this.connect();
    } else if (this.ws.readyState === WebSocket.OPEN) {
      this.sendSubscription(options);
    }

    return () => {
      this.unsubscribe(subKey);
    };
  }

  public unsubscribe(subKey: string): void {
    const sub = this.activeSubscriptions.get(subKey);
    if (sub && this.ws && this.ws.readyState === WebSocket.OPEN) {
      try {
        this.ws.send(
          JSON.stringify({
            action: "unsubscribe",
            organizationId: sub.organizationId,
            stream: sub.stream,
            repositoryId: sub.repositoryId,
          })
        );
      } catch {
        // ignore send errors during teardown
      }
    }
    this.activeSubscriptions.delete(subKey);

    if (this.activeSubscriptions.size === 0) {
      this.disconnect();
    }
  }

  public connect(): void {
    if (this.isConnecting || (this.ws && this.ws.readyState === WebSocket.OPEN)) {
      return;
    }

    if (!this.WebSocketClass) {
      throw new Error("No WebSocket implementation found in current environment");
    }

    this.isExplicitlyClosed = false;
    this.isConnecting = true;

    try {
      const url = new URL(`${this.baseUrl}/v1/events/stream`);
      if (this.token) {
        url.searchParams.set("token", this.token);
      } else if (this.apiKey) {
        url.searchParams.set("api_key", this.apiKey);
      }

      this.ws = new this.WebSocketClass(url.toString());

      this.ws.onopen = () => {
        this.isConnecting = false;
        this.reconnectAttempt = 0;

        for (const sub of this.activeSubscriptions.values()) {
          this.sendSubscription(sub);
          sub.onConnect?.();
        }
      };

      this.ws.onmessage = (event) => {
        try {
          const raw = typeof event.data === "string" ? event.data : event.data.toString();
          const parsed = JSON.parse(raw) as StreamEvent;
          this.handleIncomingEvent(parsed);
        } catch (err) {
          this.broadcastError(new Error(`Failed to parse stream event: ${(err as Error).message}`));
        }
      };

      this.ws.onerror = () => {
        this.isConnecting = false;
        this.broadcastError(new Error("WebSocket encountered an error"));
      };

      this.ws.onclose = () => {
        this.isConnecting = false;
        for (const sub of this.activeSubscriptions.values()) {
          sub.onDisconnect?.();
        }
        if (!this.isExplicitlyClosed) {
          this.scheduleReconnect();
        }
      };
    } catch {
      this.isConnecting = false;
      this.scheduleReconnect();
    }
  }

  public disconnect(): void {
    this.isExplicitlyClosed = true;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.ws) {
      try {
        this.ws.close();
      } catch {
        // ignore
      }
      this.ws = null;
    }
  }

  public reconnect(): void {
    this.disconnect();
    this.connect();
  }

  public getLastEventId(organizationId: UUID, stream?: StreamName): number | undefined {
    const subKey = `${organizationId}:${stream ?? "all"}:all`;
    return this.lastEventIds.get(subKey);
  }

  private getSubscriptionKey(options: StreamSubscriptionOptions): string {
    return `${options.organizationId}:${options.stream ?? "all"}:${options.repositoryId ?? "all"}`;
  }

  private sendSubscription(options: StreamSubscriptionOptions): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;

    const subKey = this.getSubscriptionKey(options);
    const lastCursor = this.lastEventIds.get(subKey) ?? options.cursor;

    this.ws.send(
      JSON.stringify({
        action: "subscribe",
        organizationId: options.organizationId,
        stream: options.stream,
        repositoryId: options.repositoryId,
        cursor: lastCursor,
      })
    );
  }

  private handleIncomingEvent(event: StreamEvent): void {
    if (!event || typeof event.id !== "number") return;

    for (const [subKey, sub] of this.activeSubscriptions.entries()) {
      if (sub.organizationId !== event.organizationId) continue;
      if (sub.stream && sub.stream !== event.stream) continue;
      if (sub.repositoryId && event.repositoryId && sub.repositoryId !== event.repositoryId) continue;

      const lastSeenId = this.lastEventIds.get(subKey) ?? 0;
      // Deduplicate: ignore events already seen unless resuming
      if (event.id <= lastSeenId && lastSeenId !== 0) {
        continue;
      }

      this.lastEventIds.set(subKey, event.id);
      sub.onEvent(event);
    }
  }

  private broadcastError(error: Error): void {
    for (const sub of this.activeSubscriptions.values()) {
      sub.onError?.(error);
    }
  }

  private scheduleReconnect(): void {
    if (this.isExplicitlyClosed || this.activeSubscriptions.size === 0) return;
    if (this.reconnectAttempt >= this.maxReconnectAttempts) return;

    const backoff = Math.min(
      this.reconnectInitialDelayMs * Math.pow(1.5, this.reconnectAttempt) + Math.random() * 500,
      this.reconnectMaxDelayMs
    );
    this.reconnectAttempt++;

    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
    }

    this.reconnectTimer = setTimeout(() => {
      this.connect();
    }, backoff);
  }
}
