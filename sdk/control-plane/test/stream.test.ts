import { beforeEach, describe, expect, it, vi } from "vitest";
import { ControlPlaneStreamClient } from "../src/stream.js";
import type { StreamEvent, StreamSubscriptionOptions } from "../src/types/stream.js";

class MockWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSED = 3;
  static instances: MockWebSocket[] = [];

  public readyState = MockWebSocket.CONNECTING;
  public readonly url: string;
  public readonly sentMessages: string[] = [];
  public onopen: (() => void) | null = null;
  public onmessage: ((event: { data: string }) => void) | null = null;
  public onerror: ((event: unknown) => void) | null = null;
  public onclose: ((event: unknown) => void) | null = null;

  constructor(url: string) {
    this.url = url;
    MockWebSocket.instances.push(this);
  }

  public send(message: string): void {
    this.sentMessages.push(message);
  }

  public close(): void {
    this.readyState = MockWebSocket.CLOSED;
    this.onclose?.({});
  }

  public open(): void {
    this.readyState = MockWebSocket.OPEN;
    this.onopen?.();
  }

  public serverClose(): void {
    this.readyState = MockWebSocket.CLOSED;
    this.onclose?.({});
  }

  public emitMessage(data: unknown): void {
    this.onmessage?.({ data: JSON.stringify(data) });
  }

  public emitRaw(data: string): void {
    this.onmessage?.({ data });
  }
}

describe("ControlPlaneStreamClient", () => {
  let client: ControlPlaneStreamClient;
  let ticketNumber: number;
  let mockFetch: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    MockWebSocket.instances = [];
    ticketNumber = 0;
    mockFetch = vi.fn(async () =>
      new Response(
        JSON.stringify({
          ticket: `ticket-${++ticketNumber}`,
          expires_at: "2026-08-29T18:00:30Z",
        }),
        { status: 200, headers: { "Content-Type": "application/json" } }
      )
    );
    client = new ControlPlaneStreamClient({
      baseUrl: "https://control-plane.example.com",
      token: "test-token-stream",
      fetch: mockFetch as typeof fetch,
      WebSocketClass: MockWebSocket as unknown as typeof WebSocket,
      reconnectInitialDelayMs: 5,
      maxReconnectAttempts: 0,
    });
  });

  it.each([undefined, null, "", "all", "unknown"])(
    "rejects an absent or unsupported stream scope before issuing a ticket (%s)",
    (stream) => {
      const options = {
        organizationId: "org-1",
        stream,
        onEvent: vi.fn(),
      } as unknown as StreamSubscriptionOptions;

      expect(() => client.subscribe(options)).toThrowError(
        new TypeError("Stream subscriptions require an explicit supported stream")
      );
      expect(mockFetch).not.toHaveBeenCalled();
      expect(MockWebSocket.instances).toHaveLength(0);
    }
  );

  it("mints an authenticated ticket and adapts snake-case wire events", async () => {
    const receivedEvents: StreamEvent[] = [];
    const unsubscribe = client.subscribe({
      organizationId: "org-1",
      stream: "notifications",
      cursor: 100,
      onEvent: (event) => receivedEvents.push(event),
    });

    const ws = await socketAt(0);
    const [ticketUrl, ticketInit] = mockFetch.mock.calls[0] as [string, RequestInit];
    expect(ticketUrl).toBe("https://control-plane.example.com/v1/events/ticket");
    expect(ticketInit.method).toBe("POST");
    expect(ticketInit.headers).toMatchObject({ Authorization: "Bearer test-token-stream" });
    expect(JSON.parse(ticketInit.body as string)).toEqual({
      organization_id: "org-1",
      repository_id: null,
      stream: "notifications",
      last_event_id: 100,
    });
    expect(ws.url).toBe(
      "wss://control-plane.example.com/v1/events/stream?ticket=ticket-1"
    );
    expect(ws.url).not.toContain("test-token-stream");
    expect(ws.sentMessages).toEqual([]);
    ws.open();

    ws.emitMessage(wireEvent(101));
    expect(receivedEvents).toEqual([
      {
        id: 101,
        organizationId: "org-1",
        repositoryId: null,
        stream: "notifications",
        payload: { id: 101 },
        createdAt: "2026-08-17T10:00:00Z",
      },
    ]);

    ws.emitMessage(wireEvent(101));
    ws.emitMessage(wireEvent(102));
    expect(receivedEvents.map(({ id }) => id)).toEqual([101, 102]);
    unsubscribe();
  });

  it.each([
    { name: "first listener unmounts first", firstUnmounted: 0 as const },
    { name: "second listener unmounts first", firstUnmounted: 1 as const },
  ])("shares one ticket/socket and preserves the survivor when $name", async ({ firstUnmounted }) => {
    const received: StreamEvent[][] = [[], []];
    const connected = [vi.fn(), vi.fn()];
    const subscribeListener = (listener: number) =>
      client.subscribe({
        organizationId: "org-1",
        stream: "notifications",
        onEvent: (event) => received[listener].push(event),
        onConnect: connected[listener],
      });
    const unsubscribeFirst = subscribeListener(0);
    const ws = await socketAt(0);
    ws.open();
    const unsubscribeSecond = subscribeListener(1);

    expect(mockFetch).toHaveBeenCalledTimes(1);
    expect(MockWebSocket.instances).toHaveLength(1);
    expect(connected[0]).toHaveBeenCalledTimes(1);
    expect(connected[1]).toHaveBeenCalledTimes(1);

    ws.emitMessage(wireEvent(101));
    expect(received[0].map(({ id }) => id)).toEqual([101]);
    expect(received[1].map(({ id }) => id)).toEqual([101]);

    const unsubscribers = [unsubscribeFirst, unsubscribeSecond];
    unsubscribers[firstUnmounted]();
    expect(ws.readyState).toBe(MockWebSocket.OPEN);
    ws.emitMessage(wireEvent(102));
    const survivor = firstUnmounted === 0 ? 1 : 0;
    expect(received[firstUnmounted].map(({ id }) => id)).toEqual([101]);
    expect(received[survivor].map(({ id }) => id)).toEqual([101, 102]);

    unsubscribers[survivor]();
    expect(ws.readyState).toBe(MockWebSocket.CLOSED);
  });

  it("treats duplicate callbacks as independent listener registrations", async () => {
    const callback = vi.fn<(event: StreamEvent) => void>();
    const options = {
      organizationId: "org-1",
      stream: "notifications" as const,
      onEvent: callback,
    };
    const unsubscribeFirst = client.subscribe(options);
    const unsubscribeSecond = client.subscribe(options);
    const ws = await socketAt(0);
    ws.open();

    ws.emitMessage(wireEvent(101));
    expect(callback).toHaveBeenCalledTimes(2);
    unsubscribeFirst();
    unsubscribeFirst();
    ws.emitMessage(wireEvent(102));
    expect(callback).toHaveBeenCalledTimes(3);
    expect(ws.readyState).toBe(MockWebSocket.OPEN);
    unsubscribeSecond();
    expect(ws.readyState).toBe(MockWebSocket.CLOSED);
  });

  it("opens independent query-scoped sockets for different stream keys", async () => {
    client.subscribe({
      organizationId: "org-1",
      stream: "notifications",
      onEvent: vi.fn(),
    });
    client.subscribe({
      organizationId: "org-1",
      repositoryId: "repo-1",
      stream: "approvals",
      onEvent: vi.fn(),
    });
    await socketAt(1);

    expect(mockFetch).toHaveBeenCalledTimes(2);
    expect(MockWebSocket.instances).toHaveLength(2);
    const bodies = mockFetch.mock.calls.map(([, init]) => JSON.parse(init.body as string));
    expect(bodies).toContainEqual({
      organization_id: "org-1",
      repository_id: null,
      stream: "notifications",
    });
    expect(bodies).toContainEqual({
      organization_id: "org-1",
      repository_id: "repo-1",
      stream: "approvals",
    });
  });

  it("isolates listener failures and rejects malformed or cross-scope frames", async () => {
    const firstError = vi.fn(() => {
      throw new Error("error handler failed");
    });
    const secondEvent = vi.fn<(event: StreamEvent) => void>();
    const secondConnect = vi.fn();
    const secondDisconnect = vi.fn();
    const secondError = vi.fn<(error: Error) => void>();

    client.subscribe({
      organizationId: "org-1",
      stream: "notifications",
      onEvent: () => {
        throw new Error("event handler failed");
      },
      onConnect: () => {
        throw new Error("connect handler failed");
      },
      onDisconnect: () => {
        throw new Error("disconnect handler failed");
      },
      onError: firstError,
    });
    client.subscribe({
      organizationId: "org-1",
      stream: "notifications",
      onEvent: secondEvent,
      onConnect: secondConnect,
      onDisconnect: secondDisconnect,
      onError: secondError,
    });
    const ws = await socketAt(0);
    ws.open();
    expect(secondConnect).toHaveBeenCalledTimes(1);

    ws.emitMessage(wireEvent(101));
    expect(secondEvent).toHaveBeenCalledTimes(1);
    expect(secondError).not.toHaveBeenCalledWith(
      expect.objectContaining({ message: expect.stringContaining("parse") })
    );

    ws.emitRaw("{");
    ws.emitMessage({ ...wireEvent(102), organization_id: "org-other" });
    expect(secondError).toHaveBeenCalledWith(
      expect.objectContaining({ message: expect.stringContaining("Failed to parse") })
    );
    expect(secondError).toHaveBeenCalledWith(
      expect.objectContaining({ message: expect.stringContaining("outside") })
    );

    client.disconnect();
    expect(secondDisconnect).toHaveBeenCalledTimes(1);
  });

  it("mints a fresh ticket with the latest cursor on reconnect", async () => {
    client.subscribe({
      organizationId: "org-1",
      stream: "notifications",
      onEvent: vi.fn(),
    });
    const first = await socketAt(0);
    first.open();
    first.emitMessage(wireEvent(41));

    client.reconnect();
    const second = await socketAt(1);
    expect(first.readyState).toBe(MockWebSocket.CLOSED);
    expect(second.url).toContain("ticket=ticket-2");
    const reconnectBody = JSON.parse(mockFetch.mock.calls[1]?.[1]?.body as string);
    expect(reconnectBody.last_event_id).toBe(41);
  });

  it("rejects a late ticket minted with superseded credentials", async () => {
    let resolveOldTicket: ((response: Response) => void) | undefined;
    mockFetch
      .mockImplementationOnce(
        () =>
          new Promise<Response>((resolve) => {
            resolveOldTicket = resolve;
          })
      )
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({ ticket: "new-ticket", expires_at: "2026-08-29T18:00:30Z" }),
          { status: 200 }
        )
      );

    client.subscribe({
      organizationId: "org-1",
      stream: "notifications",
      onEvent: vi.fn(),
    });
    client.setToken("replacement-token");

    const current = await socketAt(0);
    expect(current.url).toContain("ticket=new-ticket");
    expect(mockFetch.mock.calls[1]?.[1]?.headers).toMatchObject({
      Authorization: "Bearer replacement-token",
    });

    resolveOldTicket?.(
      new Response(
        JSON.stringify({ ticket: "stale-ticket", expires_at: "2026-08-29T18:00:30Z" }),
        { status: 200 }
      )
    );
    await Promise.resolve();
    await Promise.resolve();
    expect(MockWebSocket.instances).toHaveLength(1);
    expect(current.url).not.toContain("stale-ticket");
  });

  it("surfaces ticket issuance failures without opening a socket", async () => {
    mockFetch.mockResolvedValueOnce(new Response("denied", { status: 403 }));
    const onError = vi.fn<(error: Error) => void>();
    client.subscribe({
      organizationId: "org-1",
      stream: "notifications",
      onEvent: vi.fn(),
      onError,
    });

    await vi.waitFor(() => expect(onError).toHaveBeenCalledTimes(1));
    expect(onError).toHaveBeenCalledWith(
      expect.objectContaining({ message: expect.stringContaining("(403): denied") })
    );
    expect(MockWebSocket.instances).toHaveLength(0);
  });
});

function wireEvent(id: number): Record<string, unknown> {
  return {
    id,
    organization_id: "org-1",
    repository_id: null,
    stream: "notifications",
    payload: { id },
    created_at: "2026-08-17T10:00:00Z",
  };
}

async function socketAt(index: number): Promise<MockWebSocket> {
  await vi.waitFor(() => expect(MockWebSocket.instances[index]).toBeDefined());
  return MockWebSocket.instances[index] as MockWebSocket;
}
