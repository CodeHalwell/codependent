/**
 * DaemonClient: a thin, reconnecting, host-neutral client for the Codypendent daemon.
 *
 * Lifecycle of one connection:
 *   connect -> send `ClientHello` -> receive `ServerHello`
 *           -> send `Command(AttachSession { requested_role: Approver })`
 *           -> receive `Catchup` and a live stream of `Event`s.
 *
 * Wire semantics:
 *   - Injected duplex byte-stream factory (Node net.connect, Tauri invoke + Channel, tests).
 *   - Length-prefixed framing via encodeEnvelope and FrameDecoder.
 *   - Request/reply correlation by correlation_id with interleaved responses.
 *   - Watermark tracking and resume token presentation on reconnect.
 *   - Bounded offline command queue (MAX_QUEUED_COMMANDS = 256).
 *   - Exponential backoff with configurable ceiling and factor.
 *   - Session store integration with paginated catch-up and live event dedup.
 *   - Paging state reset on query/filter changes for session search.
 */

import { encodeEnvelope, FrameDecoder, FrameError, MAX_FRAME_BYTES } from "./framing.js";
import { PROTOCOL_V1, type ProtocolVersion } from "./version.js";
import { IDE_CAPABILITIES, type ClientCapabilities } from "./capabilities.js";
import type { ClientHello, ServerHello, ClientRole, Subscription, ResumeToken } from "./handshake.js";
import type { Envelope, Payload } from "./envelope.js";
import type { Command, CommandBody } from "./commands.js";
import type { SessionEvent } from "./events.js";
import type { Catchup } from "./catchup.js";
import type { CodypendentError, ProtocolError } from "./error.js";
import type { Uuid } from "./ids.js";
import type {
  EditorActionContext,
  EditorNativeAction,
  SessionLifecycleAction,
  SessionSearchFilters,
  SessionSearchPage,
  SessionSearchQuery,
  SessionSearchResult,
} from "./session.js";
import type { IdeContextUpdate } from "./ide.js";
import type { AgentMode, ApprovalDecision, ApprovalScope } from "./run.js";
import type { ArtifactRef } from "./artifact.js";
import type { InboxEntry, InboxListQuery, InboxMutation, InboxPage } from "./inbox.js";
import type {
  AnalyticsExportRequest,
  AnalyticsExportResult,
  AnalyticsPage,
  AnalyticsQuery,
} from "./analytics.js";
import { SessionStore, type SessionStoreSnapshot } from "./session-store.js";

/** Remote UI wire message type extracted from Payload::RemoteUi. */
export type UiWireMessage = Extract<Payload, { type: "RemoteUi" }>["message"];

/** Minimal duplex byte-stream surface the client needs. */
export interface SocketLike {
  write(data: Uint8Array): boolean;
  on(event: "data", listener: (chunk: Uint8Array) => void): this;
  on(event: "connect", listener: () => void): this;
  on(event: "close", listener: (hadError?: boolean) => void): this;
  on(event: "error", listener: (err: Error) => void): this;
  removeAllListeners?(): this;
  destroy(error?: Error): void;
}

/** Factory that opens a connection to a target/socket path. */
export type ConnectionFactory = (target: string) => SocketLike;

export interface BackoffConfig {
  /** First reconnect delay in ms. */
  initialMs: number;
  /** Ceiling for the delay in ms. */
  maxMs: number;
  /** Multiplier applied per attempt. */
  factor: number;
}

export const DEFAULT_BACKOFF: BackoffConfig = {
  initialMs: 500,
  maxMs: 15_000,
  factor: 2,
};

/**
 * Exponential backoff for reconnect `attempt` (0-based).
 * `delay(attempt) = min(maxMs, initialMs * factor^attempt)`.
 */
export function computeBackoff(attempt: number, config: BackoffConfig = DEFAULT_BACKOFF): number {
  const raw = config.initialMs * Math.pow(config.factor, Math.max(0, attempt));
  return Math.min(config.maxMs, Math.round(raw));
}

/**
 * Offline-queue bound (FP-5): the queue must stay finite so a session left
 * offline for a long time cannot accumulate unbounded memory.
 */
export const MAX_QUEUED_COMMANDS = 256;

export type ConnectionStatus =
  | "connecting"
  | "handshaking"
  | "attaching"
  | "attached"
  | "reconnecting"
  | "closed";

/** Strongly-typed event map the client emits. */
export interface DaemonClientEvents {
  status: (status: ConnectionStatus) => void;
  serverHello: (hello: ServerHello) => void;
  catchup: (catchup: Catchup) => void;
  event: (event: SessionEvent) => void;
  remoteUi: (message: UiWireMessage) => void;
  commandAccepted: (info: { command_id: Uuid; sequence?: number; created_run?: Uuid }) => void;
  commandRejected: (error: CodypendentError) => void;
  protocolError: (error: ProtocolError) => void;
  error: (error: Error) => void;
  approvalDropped: (info: { approvalId: Uuid }) => void;
}

export interface DaemonClientOptions {
  socketPath?: string;
  sessionId?: Uuid;
  /** Stable client identity for the connection lifetime. Generated if absent. */
  clientId?: Uuid;
  clientName?: string;
  clientVersion?: string;
  capabilities?: ClientCapabilities;
  subscriptions?: Subscription[];
  role?: ClientRole;
  backoff?: BackoffConfig;
  /** Injectable transport factory; tests and platforms supply their own. */
  createConnection?: ConnectionFactory;
  /** Injectable delay; defaults to setTimeout. */
  wait?: (ms: number) => Promise<void>;
  /** Optional SessionStore instance for state tracking. */
  store?: SessionStore;
}

type EventListener<T = any> = (...args: T[]) => void;

/** Host-neutral typed event emitter. */
export class TypedEventEmitter<Events extends Record<string, any>> {
  private _listeners = new Map<keyof Events, Set<EventListener>>();

  on<E extends keyof Events>(event: E, listener: Events[E]): this {
    let set = this._listeners.get(event);
    if (!set) {
      set = new Set();
      this._listeners.set(event, set);
    }
    set.add(listener as EventListener);
    return this;
  }

  once<E extends keyof Events>(event: E, listener: Events[E]): this {
    const wrapper = ((...args: any[]) => {
      this.off(event, wrapper as unknown as Events[E]);
      (listener as Function)(...args);
    }) as unknown as Events[E];
    return this.on(event, wrapper);
  }

  off<E extends keyof Events>(event: E, listener: Events[E]): this {
    const set = this._listeners.get(event);
    if (set) {
      set.delete(listener as EventListener);
      if (set.size === 0) {
        this._listeners.delete(event);
      }
    }
    return this;
  }

  emit<E extends keyof Events>(event: E, ...args: Parameters<Events[E]>): boolean {
    const set = this._listeners.get(event);
    if (!set || set.size === 0) return false;
    for (const listener of [...set]) {
      listener(...args);
    }
    return true;
  }

  removeAllListeners<E extends keyof Events>(event?: E): this {
    if (event !== undefined) {
      this._listeners.delete(event);
    } else {
      this._listeners.clear();
    }
    return this;
  }

  addListener<E extends keyof Events>(event: E, listener: Events[E]): this {
    return this.on(event, listener);
  }

  removeListener<E extends keyof Events>(event: E, listener: Events[E]): this {
    return this.off(event, listener);
  }
}

function generateUuid(): string {
  if (typeof globalThis.crypto?.randomUUID === "function") {
    return globalThis.crypto.randomUUID();
  }
  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, (c) => {
    const r = (Math.random() * 16) | 0;
    const v = c === "x" ? r : (r & 0x3) | 0x8;
    return v.toString(16);
  });
}

function parseBase64Chunk(bytesBase64: string): Uint8Array {
  if (typeof Buffer !== "undefined") {
    return Buffer.from(bytesBase64, "base64");
  }
  const binary = atob(bytesBase64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

function concatUint8Arrays(arrays: Uint8Array[]): Uint8Array {
  const totalLength = arrays.reduce((acc, curr) => acc + curr.length, 0);
  const result = new Uint8Array(totalLength);
  let offset = 0;
  for (const arr of arrays) {
    result.set(arr, offset);
    offset += arr.length;
  }
  return result;
}

async function computeSha256(bytes: Uint8Array): Promise<string> {
  if (typeof globalThis.crypto?.subtle?.digest === "function") {
    const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes as unknown as BufferSource);
    return Array.from(new Uint8Array(digest))
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("");
  }
  try {
    const { createHash } = await import("node:crypto");
    return createHash("sha256").update(bytes).digest("hex");
  } catch {
    throw new Error("No SHA-256 implementation available in this environment");
  }
}

export function isUiWireMessage(value: unknown): value is UiWireMessage {
  if (value === null || typeof value !== "object") return false;
  const obj = value as Record<string, unknown>;
  const typeOrKind = obj.type ?? obj.kind;
  if (typeof typeOrKind !== "string" || typeOrKind.length === 0 || typeOrKind.length > 256) return false;
  if (typeof obj.messageId !== "string" || obj.messageId.length === 0 || obj.messageId.length > 256) return false;
  return true;
}

export class DaemonClient extends TypedEventEmitter<DaemonClientEvents> {
  private readonly socketPath: string;
  private sessionId?: Uuid;
  private readonly clientId: Uuid;
  private readonly clientName: string;
  private readonly clientVersion: string;
  private readonly capabilities: ClientCapabilities;
  private readonly subscriptions: Subscription[];
  private readonly role: ClientRole;
  private readonly backoff: BackoffConfig;
  private readonly connect: ConnectionFactory;
  private readonly wait: (ms: number) => Promise<void>;
  private readonly sessionStore: SessionStore;

  private lastSeenSequence: number | undefined;
  private resumeToken: string | undefined;

  private readonly offlineQueue: Command[] = [];
  private readonly bufferedEvents: SessionEvent[] = [];

  private socket: SocketLike | undefined;
  private stopped = false;
  private running = false;
  private status: ConnectionStatus = "closed";
  private attached = false;
  private readonly pendingRequests = new Map<
    Uuid,
    {
      socket: SocketLike;
      resolve: (payload: Payload) => void;
      reject: (error: Error) => void;
    }
  >();

  private lastSearchQueryKey?: string;
  private lastSearchCursor?: string | null;

  constructor(options: DaemonClientOptions) {
    super();
    this.socketPath = options.socketPath ?? "";
    this.sessionId = options.sessionId;
    this.clientId = options.clientId ?? generateUuid();
    this.clientName = options.clientName ?? "codypendent-client";
    this.clientVersion = options.clientVersion ?? "0.3.2";
    this.capabilities = options.capabilities ?? IDE_CAPABILITIES;
    this.subscriptions = options.subscriptions ?? [
      { type: "SessionSummary" },
      { type: "AgentActivity" },
    ];
    this.role = options.role ?? { type: "Approver" };
    this.backoff = options.backoff ?? DEFAULT_BACKOFF;
    this.connect =
      options.createConnection ??
      ((_p: string) => {
        throw new Error("No connection factory provided for DaemonClient");
      });
    this.wait = options.wait ?? ((ms: number) => new Promise((resolve) => setTimeout(resolve, ms)));
    this.sessionStore = options.store ?? new SessionStore();
  }

  /** The highest ledger sequence observed so far (the resume cursor). */
  get sequenceCursor(): number | undefined {
    return this.lastSeenSequence;
  }

  get connectionStatus(): ConnectionStatus {
    return this.status;
  }

  get store(): SessionStore {
    return this.sessionStore;
  }

  /** Begin the connect/handshake/attach/reconnect loop. Idempotent. */
  start(): void {
    if (this.running) {
      return;
    }
    this.running = true;
    this.stopped = false;
    void this.runLoop();
  }

  /** Stop for good: close the socket and do not reconnect. */
  stop(): void {
    this.stopped = true;
    this.running = false;
    this.teardownSocket();
    this.setStatus("closed");
  }

  /** Attach to a specific session. */
  attachSession(sessionId: Uuid): void {
    this.sessionId = sessionId;
    if (this.status === "handshaking" || this.status === "attaching" || this.status === "attached") {
      this.setStatus("attaching");
      this.sendAttach();
    }
  }

  // --- command helpers ------------------------------------------------------

  /** Resolve an approval. Decision `Approve`/`Reject`, default scope `Once`. */
  resolveApproval(
    approvalId: Uuid,
    decision: ApprovalDecision["type"],
    scope: ApprovalScope["type"] = "Once",
  ): void {
    this.sendCommand(
      {
        type: "ResolveApproval",
        approval_id: approvalId,
        decision: { type: decision },
        scope: { type: scope },
      },
      { queueIfOffline: true },
    );
  }

  /** Start a run in the attached session. */
  startRun(objective: string, mode: AgentMode["type"] = "Build", repository?: string): void {
    if (!this.sessionId) {
      throw new Error("Cannot start run without an attached session");
    }
    const body: CommandBody = {
      type: "StartRun",
      session_id: this.sessionId,
      objective,
      mode: { type: mode },
    };
    if (repository !== undefined) {
      body.repository = repository;
    }
    this.sendCommand(body, { queueIfOffline: true });
  }

  /** Start an ordinary attributable run from an editor-native action. */
  runEditorAction(
    action: EditorNativeAction,
    context: EditorActionContext,
    model?: string,
  ): void {
    if (!this.sessionId) {
      throw new Error("Cannot run editor action without an attached session");
    }
    const body: CommandBody = {
      type: "RunEditorAction",
      session_id: this.sessionId,
      action,
      context,
      ...(model ? { model } : {}),
    };
    this.sendCommand(body, { queueIfOffline: true });
  }

  /** Search sessions in the Session Library via ranked, cursor-paged query. */
  async searchSessions(query: SessionSearchQuery): Promise<SessionSearchPage> {
    const queryKey = JSON.stringify({ query: query.query, filters: query.filters });
    if (
      this.lastSearchQueryKey !== undefined &&
      this.lastSearchQueryKey !== queryKey &&
      query.cursor !== undefined &&
      query.cursor === this.lastSearchCursor
    ) {
      query = { ...query, cursor: undefined };
    }
    this.lastSearchQueryKey = queryKey;
    const page = await searchSessions(this, query);
    this.lastSearchCursor = page.next_cursor;
    return page;
  }

  /** Mutate session lifecycle (Rename, Pin, Unpin, Archive, Restore). */
  mutateSessionLifecycle(sessionId: Uuid, action: SessionLifecycleAction): void {
    this.sendCommand(
      {
        type: "MutateSessionLifecycle",
        session_id: sessionId,
        action,
      },
      { queueIfOffline: true },
    );
  }

  /** Submit steering / user input into the attached session. */
  submitUserInput(text: string, mode: AgentMode["type"] = "Build"): void {
    if (!this.sessionId) {
      throw new Error("Cannot submit user input without an attached session");
    }
    this.sendCommand(
      {
        type: "SubmitUserInput",
        session_id: this.sessionId,
        text,
        mode: { type: mode },
      },
      { queueIfOffline: true },
    );
  }

  /** Push a debounced IDE context snapshot. */
  sendIdeContext(update: IdeContextUpdate): void {
    if (!this.sessionId) {
      return;
    }
    this.sendCommand({
      type: "UpdateIdeContext",
      session_id: this.sessionId,
      update,
    });
  }

  /** Retrieve and verify an artifact through correlated, bounded chunk reads. */
  async readArtifact(artifact: ArtifactRef): Promise<Uint8Array> {
    const chunks: Uint8Array[] = [];
    let offset = 0;
    for (;;) {
      const payload = await this.request({
        type: "ReadArtifact",
        artifact_id: artifact.id,
        offset,
        limit: 1024 * 1024,
        expected_sha256: artifact.sha256,
      });
      if (payload.type === "CommandRejected") throw new Error("artifact is unavailable");
      if (payload.type !== "ArtifactChunk") throw new Error("unexpected artifact reply");
      const chunk = payload as {
        type: "ArtifactChunk";
        artifact_id: Uuid;
        offset: number;
        bytes_base64: string;
        eof: boolean;
        sha256: string;
      };
      if (
        chunk.artifact_id !== artifact.id ||
        chunk.offset !== offset ||
        chunk.sha256 !== artifact.sha256
      ) {
        throw new Error("invalid artifact chunk correlation");
      }
      if (
        !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(
          chunk.bytes_base64,
        )
      ) {
        throw new Error("artifact chunk contains malformed base64");
      }
      const bytes = parseBase64Chunk(chunk.bytes_base64);
      chunks.push(bytes);
      offset += bytes.length;
      if (offset > artifact.byte_length) throw new Error("artifact exceeded declared length");
      if (chunk.eof) break;
      if (bytes.length === 0) throw new Error("artifact retrieval made no progress");
    }
    const result = concatUint8Arrays(chunks);
    if (result.length !== artifact.byte_length)
      throw new Error("artifact length verification failed");
    const actual = await computeSha256(result);
    if (actual !== artifact.sha256) throw new Error("artifact hash verification failed");
    return typeof Buffer !== "undefined" ? Buffer.from(result) : result;
  }

  /** Disable a verified Remote UI plugin after explicit host confirmation. */
  revokeUiPlugin(pluginId: string): void {
    if (pluginId.length === 0 || pluginId.length > 256) return;
    this.sendCommand({ type: "RevokeUiPlugin", plugin_id: pluginId }, { queueIfOffline: true });
  }

  /** Forward a validated semantic UI message over the RemoteUi envelope. */
  sendRemoteUi(message: UiWireMessage): boolean {
    if (!isUiWireMessage(message) || !this.attached || this.socket === undefined) return false;
    this.sendEnvelope(this.buildEnvelope({ type: "RemoteUi", message }, { withSession: true }));
    return true;
  }

  /** List notifications and approvals from the durable inbox. */
  async listInbox(query?: InboxListQuery): Promise<InboxPage> {
    return listInbox(this, query);
  }

  /** Apply an idempotent mutation to an inbox entry. */
  async mutateInbox(mutation: InboxMutation): Promise<InboxEntry> {
    return mutateInbox(this, mutation);
  }

  /** Query measured execution observations and analytics aggregates. */
  async queryAnalytics(query?: AnalyticsQuery): Promise<AnalyticsPage> {
    return queryAnalytics(this, query);
  }

  /** Request a server-bounded export of analytics observations. */
  async exportAnalytics(request: AnalyticsExportRequest): Promise<AnalyticsExportResult> {
    return exportAnalytics(this, request);
  }

  // --- connection loop ------------------------------------------------------

  private async runLoop(): Promise<void> {
    let attempt = 0;
    while (!this.stopped) {
      try {
        await this.connectOnce(() => {
          attempt = 0;
        });
      } catch (err) {
        this.emit("error", err instanceof Error ? err : new Error(String(err)));
      }
      if (this.stopped) {
        break;
      }
      const delay = computeBackoff(attempt, this.backoff);
      attempt += 1;
      this.setStatus("reconnecting");
      await this.wait(delay);
    }
    this.running = false;
  }

  private connectOnce(onAttached: () => void): Promise<void> {
    return new Promise<void>((resolve) => {
      const decoder = new FrameDecoder();
      this.attached = false;
      this.setStatus("connecting");

      let settled = false;
      const settle = (): void => {
        if (settled) {
          return;
        }
        settled = true;
        resolve();
      };

      let socket: SocketLike;
      try {
        socket = this.connect(this.socketPath);
      } catch (err) {
        this.emit("error", err instanceof Error ? err : new Error(String(err)));
        settle();
        return;
      }
      this.socket = socket;

      socket.on("connect", () => {
        this.setStatus("handshaking");
        this.sendClientHello();
      });

      socket.on("data", (chunk: Uint8Array) => {
        if (this.socket !== socket) return;
        let envelopes: Envelope[];
        try {
          envelopes = decoder.push(chunk);
        } catch (err) {
          this.emit("error", err instanceof Error ? err : new Error(String(err)));
          socket.destroy();
          return;
        }
        for (const envelope of envelopes) {
          const correlationId = envelope.correlation_id;
          const pending = correlationId ? this.pendingRequests.get(correlationId) : undefined;
          if (pending?.socket === socket) {
            this.pendingRequests.delete(correlationId!);
            pending.resolve(envelope.payload);
          } else {
            this.handlePayload(envelope.payload, onAttached);
          }
        }
      });

      socket.on("error", (err: Error) => {
        this.emit("error", err);
      });

      socket.on("close", () => {
        this.rejectPendingRequests(socket);
        if (this.socket === socket) {
          this.socket = undefined;
          this.attached = false;
        }
        settle();
      });
    });
  }

  private handlePayload(payload: Payload, onAttached: () => void): void {
    switch (payload.type) {
      case "ServerHello": {
        const hello = payload as { type: "ServerHello" } & ServerHello;
        if (typeof hello.resume_token === "string" && hello.resume_token.length > 0) {
          this.resumeToken = hello.resume_token;
        }
        this.emit("serverHello", {
          selected_protocol: hello.selected_protocol,
          daemon_version: hello.daemon_version,
          daemon_instance: hello.daemon_instance,
          heartbeat_interval_ms: hello.heartbeat_interval_ms,
          resume_token: hello.resume_token,
          build_id: hello.build_id,
        });
        if (this.sessionId !== undefined) {
          this.setStatus("attaching");
          this.sendAttach();
        }
        break;
      }
      case "Ping": {
        this.sendEnvelope(this.buildEnvelope({ type: "Pong" }, { withSession: false }));
        break;
      }
      case "Catchup": {
        const catchup = (payload as { type: "Catchup"; catchup: Catchup }).catchup;
        this.setStatus("attached");
        this.attached = true;
        onAttached();
        this.applyCatchup(catchup);
        try {
          this.sessionStore.applyCatchup(catchup);
        } catch (err) {
          this.emit("error", err instanceof Error ? err : new Error(String(err)));
        }
        this.emit("catchup", catchup);
        this.flushOfflineQueue();
        break;
      }
      case "Event": {
        const event = payload as { type: "Event" } & SessionEvent;
        const sessionEvent: SessionEvent = {
          sequence: event.sequence,
          occurred_at: event.occurred_at,
          causation_id: event.causation_id,
          correlation_id: event.correlation_id,
          actor: event.actor,
          body: event.body,
        };
        if (this.lastSeenSequence !== undefined && sessionEvent.sequence <= this.lastSeenSequence) {
          break;
        }
        this.advanceCursor(sessionEvent.sequence);
        try {
          this.sessionStore.applyEvent(sessionEvent);
        } catch (err) {
          this.emit("error", err instanceof Error ? err : new Error(String(err)));
        }
        this.emit("event", sessionEvent);
        break;
      }
      case "RemoteUi": {
        const remote = payload as { type: "RemoteUi"; message: UiWireMessage };
        if (isUiWireMessage(remote.message)) {
          this.emit("remoteUi", remote.message);
        } else {
          this.emit("error", new Error("received an invalid or oversized RemoteUi envelope"));
        }
        break;
      }
      case "CommandAccepted": {
        const accepted = payload as { command_id: Uuid; sequence?: number; created_run?: Uuid };
        this.emit("commandAccepted", {
          command_id: accepted.command_id,
          sequence: accepted.sequence,
          created_run: accepted.created_run,
        });
        break;
      }
      case "CommandRejected": {
        this.emit("commandRejected", payload as { type: "CommandRejected" } & CodypendentError);
        break;
      }
      case "Error": {
        this.emit("protocolError", payload as { type: "Error" } & ProtocolError);
        break;
      }
      default:
        break;
    }
  }

  private applyCatchup(catchup: Catchup): void {
    if (catchup.type === "Events") {
      this.advanceCursor(catchup.through);
      for (const event of catchup.events) {
        this.advanceCursor(event.sequence);
      }
    } else if (catchup.type === "Snapshot") {
      this.advanceCursor(catchup.through);
    }
  }

  private advanceCursor(sequence: number): void {
    if (typeof sequence !== "number") {
      return;
    }
    if (this.lastSeenSequence === undefined || sequence > this.lastSeenSequence) {
      this.lastSeenSequence = sequence;
    }
  }

  // --- outbound framing -----------------------------------------------------

  private sendClientHello(): void {
    const payload: Payload = {
      type: "ClientHello",
      client_name: this.clientName,
      client_version: this.clientVersion,
      supported_protocols: [PROTOCOL_V1],
      capabilities: this.capabilities,
    };
    if (this.resumeToken !== undefined) {
      (payload as { resume_token?: string }).resume_token = this.resumeToken;
    }
    this.sendEnvelope(this.buildEnvelope(payload, { withSession: false }));
  }

  private sendAttach(): void {
    if (!this.sessionId) return;
    const body: CommandBody = {
      type: "AttachSession",
      session_id: this.sessionId,
      subscriptions: this.subscriptions,
      requested_role: this.role,
    };
    if (this.lastSeenSequence !== undefined) {
      body.last_seen_sequence = this.lastSeenSequence;
    }
    this.sendCommand(body);
  }

  private static isApprovalBody(
    body: CommandBody,
  ): body is Extract<CommandBody, { type: "ResolveApproval" }> {
    return body.type === "ResolveApproval";
  }

  private sendCommand(body: CommandBody, opts: { queueIfOffline?: boolean } = {}): void {
    const command: Command = {
      command_id: generateUuid(),
      idempotency_key: generateUuid(),
      body,
    };
    if (!this.attached && opts.queueIfOffline && !this.stopped) {
      this.enqueueOffline(command);
      return;
    }
    const payload: Payload = { type: "Command", ...command };
    this.sendEnvelope(this.buildEnvelope(payload, { withSession: true }));
  }

  request(body: CommandBody): Promise<Payload> {
    const socket = this.socket;
    if (!socket || !this.attached) return Promise.reject(new Error("daemon is not attached"));
    const messageId = generateUuid();
    const command: Command = { command_id: messageId, idempotency_key: generateUuid(), body };
    const envelope = this.buildEnvelope({ type: "Command", ...command }, { withSession: true });
    envelope.message_id = messageId;
    return new Promise((resolve, reject) => {
      this.pendingRequests.set(messageId, { socket, resolve, reject });
      this.sendEnvelope(envelope);
    });
  }

  private enqueueOffline(command: Command): void {
    if (this.offlineQueue.length >= MAX_QUEUED_COMMANDS) {
      const evictIndex = this.offlineQueue.findIndex(
        (queued) => !DaemonClient.isApprovalBody(queued.body),
      );
      if (evictIndex !== -1) {
        const [dropped] = this.offlineQueue.splice(evictIndex, 1);
        this.emit(
          "error",
          new Error(
            `offline command queue is full; dropped a queued ${dropped.body.type} command to make room`,
          ),
        );
      } else if (DaemonClient.isApprovalBody(command.body)) {
        const approvalId = command.body.approval_id;
        this.emit(
          "error",
          new Error(
            `offline command queue is full of pending approval decisions; approval ${approvalId} could NOT be queued`,
          ),
        );
        this.emit("approvalDropped", { approvalId });
        return;
      } else {
        this.emit(
          "error",
          new Error(
            `offline command queue is full of pending approval decisions; dropped the incoming ${command.body.type} command instead of an approval`,
          ),
        );
        return;
      }
    }
    this.offlineQueue.push(command);
  }

  private flushOfflineQueue(): void {
    const queued = this.offlineQueue.splice(0);
    for (const command of queued) {
      const payload: Payload = { type: "Command", ...command };
      this.sendEnvelope(this.buildEnvelope(payload, { withSession: true }));
    }
  }

  private buildEnvelope(payload: Payload, opts: { withSession: boolean }): Envelope {
    const envelope: Envelope = {
      protocol_version: PROTOCOL_V1,
      message_id: generateUuid(),
      client_id: this.clientId,
      payload,
    };
    if (opts.withSession && this.sessionId !== undefined) {
      envelope.session_id = this.sessionId;
    }
    return envelope;
  }

  private sendEnvelope(envelope: Envelope): void {
    if (!this.socket) {
      return;
    }
    try {
      this.socket.write(encodeEnvelope(envelope));
    } catch (err) {
      this.emit("error", err instanceof Error ? err : new Error(String(err)));
    }
  }

  private teardownSocket(): void {
    this.bufferedEvents.length = 0;
    const socket = this.socket;
    if (socket) {
      this.rejectPendingRequests(socket);
      this.socket = undefined;
      this.attached = false;
      socket.destroy();
    }
  }

  private rejectPendingRequests(socket: SocketLike): void {
    for (const [messageId, pending] of this.pendingRequests) {
      if (pending.socket === socket) {
        this.pendingRequests.delete(messageId);
        pending.reject(new Error("daemon connection closed"));
      }
    }
  }

  private setStatus(status: ConnectionStatus): void {
    if (this.status !== status) {
      this.status = status;
      this.emit("status", status);
    }
  }
}

export { DaemonClient as Client };
export type { DaemonClientOptions as ClientOptions };
export type { DaemonClientEvents as ClientEvents };

/**
 * An object or function capable of sending a CommandBody to the daemon and returning the response Payload.
 */
export interface ProtocolCommandCaller {
  request(body: CommandBody): Promise<Payload>;
}

export type CommandExecutor =
  | ProtocolCommandCaller
  | ((body: CommandBody) => Promise<Payload>);

async function executeCommand(executor: CommandExecutor, body: CommandBody): Promise<Payload> {
  if (typeof executor === "function") {
    return executor(body);
  }
  return executor.request(body);
}

function extractErrorMessage(payload: Payload): string {
  if (payload.type === "CommandRejected" || payload.type === "Error") {
    const code = "code" in payload ? String(payload.code) : "unknown-error";
    const msg = "message" in payload ? String(payload.message) : "command was rejected";
    return `${code}: ${msg}`;
  }
  return `unexpected response payload: ${payload.type}`;
}

/** Construct a CommandBody for listing inbox entries. */
export function listInboxCommand(query?: InboxListQuery): Extract<CommandBody, { type: "ListInbox" }> {
  return {
    type: "ListInbox",
    ...(query !== undefined ? { query } : {}),
  };
}

/** Construct a CommandBody for mutating an inbox entry. */
export function mutateInboxCommand(mutation: InboxMutation): Extract<CommandBody, { type: "MutateInbox" }> {
  return {
    type: "MutateInbox",
    mutation,
  };
}

/** Construct a CommandBody for querying analytics. */
export function queryAnalyticsCommand(query?: AnalyticsQuery): Extract<CommandBody, { type: "QueryAnalytics" }> {
  return {
    type: "QueryAnalytics",
    ...(query !== undefined ? { query } : {}),
  };
}

/** Construct a CommandBody for exporting analytics. */
export function exportAnalyticsCommand(request: AnalyticsExportRequest): Extract<CommandBody, { type: "ExportAnalytics" }> {
  return {
    type: "ExportAnalytics",
    request,
  };
}

/** Construct a CommandBody for searching sessions. */
export function searchSessionsCommand(query: SessionSearchQuery): Extract<CommandBody, { type: "SearchSessions" }> {
  return {
    type: "SearchSessions",
    query,
  };
}

/** List inbox entries using a cursor query. */
export async function listInbox(
  caller: CommandExecutor,
  query?: InboxListQuery,
): Promise<InboxPage> {
  const body = listInboxCommand(query);
  const payload = await executeCommand(caller, body);

  if (payload.type === "InboxPage") {
    return payload.page;
  }
  throw new Error(extractErrorMessage(payload));
}

/** Apply an idempotent mutation (Acknowledge, Dismiss) to an inbox entry. */
export async function mutateInbox(
  caller: CommandExecutor,
  mutation: InboxMutation,
): Promise<InboxEntry> {
  const body = mutateInboxCommand(mutation);
  const payload = await executeCommand(caller, body);

  if (payload.type === "InboxEntryApplied") {
    return payload.entry;
  }
  throw new Error(extractErrorMessage(payload));
}

/** Query aggregated execution observations and metrics. */
export async function queryAnalytics(
  caller: CommandExecutor,
  query?: AnalyticsQuery,
): Promise<AnalyticsPage> {
  const body = queryAnalyticsCommand(query);
  const payload = await executeCommand(caller, body);

  if (payload.type === "AnalyticsResults") {
    return payload.page;
  }
  throw new Error(extractErrorMessage(payload));
}

/** Request a server-bounded analytics export. */
export async function exportAnalytics(
  caller: CommandExecutor,
  request: AnalyticsExportRequest,
): Promise<AnalyticsExportResult> {
  const body = exportAnalyticsCommand(request);
  const payload = await executeCommand(caller, body);

  if (payload.type === "AnalyticsExported") {
    return payload.result;
  }
  throw new Error(extractErrorMessage(payload));
}

/** Search sessions in the Session Library. */
export async function searchSessions(
  caller: CommandExecutor,
  query: SessionSearchQuery,
): Promise<SessionSearchPage> {
  const body = searchSessionsCommand(query);
  const payload = await executeCommand(caller, body);

  if (payload.type === "SessionSearchResults") {
    return payload.page;
  }
  throw new Error(extractErrorMessage(payload));
}

/**
 * Pager helper that maintains search query state and resets the pagination cursor
 * whenever the query string or filters change.
 */
export class SessionSearchPager {
  private lastQueryKey?: string;
  private currentCursor?: string | null;

  constructor(private readonly client: CommandExecutor) {}

  async search(query: SessionSearchQuery): Promise<SessionSearchPage> {
    const queryKey = JSON.stringify({ query: query.query, filters: query.filters });
    if (this.lastQueryKey !== queryKey) {
      this.lastQueryKey = queryKey;
      this.currentCursor = undefined;
    }
    const finalQuery: SessionSearchQuery = {
      ...query,
      cursor: query.cursor ?? this.currentCursor ?? undefined,
    };
    const page = await searchSessions(this.client, finalQuery);
    this.currentCursor = page.next_cursor;
    return page;
  }

  get cursor(): string | null | undefined {
    return this.currentCursor;
  }

  reset(): void {
    this.lastQueryKey = undefined;
    this.currentCursor = undefined;
  }
}
