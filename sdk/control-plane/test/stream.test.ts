import { describe, it, expect, beforeEach } from "vitest";
import { ControlPlaneStreamClient } from "../src/stream.js";
import type { StreamEvent } from "../src/types/stream.js";

// Mock WebSocket class
class MockWebSocket {
  static OPEN = 1;
  static CLOSED = 3;

  public readyState = MockWebSocket.OPEN;
  public url: string;
  public sentMessages: string[] = [];

  public onopen: (() => void) | null = null;
  public onmessage: ((event: { data: string }) => void) | null = null;
  public onerror: ((event: unknown) => void) | null = null;
  public onclose: ((event: unknown) => void) | null = null;

  constructor(url: string) {
    this.url = url;
    setTimeout(() => {
      if (this.onopen) this.onopen();
    }, 0);
  }

  public send(msg: string): void {
    this.sentMessages.push(msg);
  }

  public close(): void {
    this.readyState = MockWebSocket.CLOSED;
    if (this.onclose) this.onclose({});
  }

  // Helper to simulate server sending an event
  public emitMessage(data: unknown): void {
    if (this.onmessage) {
      this.onmessage({ data: JSON.stringify(data) });
    }
  }
}

describe("ControlPlaneStreamClient", () => {
  let client: ControlPlaneStreamClient;

  beforeEach(() => {
    client = new ControlPlaneStreamClient({
      baseUrl: "https://control-plane.example.com",
      token: "test-token-stream",
      WebSocketClass: MockWebSocket as unknown as typeof WebSocket,
      reconnectInitialDelayMs: 5,
    });
  });

  it("subscribes to stream events and receives dispatched events", async () => {
    const receivedEvents: StreamEvent[] = [];

    const unsubscribe = client.subscribe({
      organizationId: "org-1",
      stream: "notifications",
      onEvent: (event) => {
        receivedEvents.push(event);
      },
    });

    // Wait for connect
    await new Promise((r) => setTimeout(r, 10));

    // Access the created mock ws
    const wsInstance = (client as unknown as { ws: MockWebSocket }).ws;
    expect(wsInstance).toBeDefined();
    expect(wsInstance.url).toContain("token=test-token-stream");

    // Emit event matching subscription
    wsInstance.emitMessage({
      id: 101,
      organizationId: "org-1",
      repositoryId: null,
      stream: "notifications",
      payload: { title: "Approval Requested", body: "Please review" },
      createdAt: "2026-08-17T10:00:00Z",
    });

    expect(receivedEvents).toHaveLength(1);
    expect(receivedEvents[0].id).toBe(101);
    expect(receivedEvents[0].payload).toEqual({ title: "Approval Requested", body: "Please review" });

    // Deduplication check: re-emitting event 101 is ignored
    wsInstance.emitMessage({
      id: 101,
      organizationId: "org-1",
      repositoryId: null,
      stream: "notifications",
      payload: { title: "Duplicate" },
      createdAt: "2026-08-17T10:00:00Z",
    });
    expect(receivedEvents).toHaveLength(1);

    // Event with new id is received
    wsInstance.emitMessage({
      id: 102,
      organizationId: "org-1",
      repositoryId: null,
      stream: "notifications",
      payload: { title: "Second Event" },
      createdAt: "2026-08-17T10:01:00Z",
    });
    expect(receivedEvents).toHaveLength(2);

    unsubscribe();
  });

  it("filters events by repositoryId when specified", async () => {
    const receivedEvents: StreamEvent[] = [];

    client.subscribe({
      organizationId: "org-1",
      repositoryId: "repo-allowed",
      onEvent: (event) => {
        receivedEvents.push(event);
      },
    });

    await new Promise((r) => setTimeout(r, 10));
    const wsInstance = (client as unknown as { ws: MockWebSocket }).ws;

    // Event for different repository should be filtered out
    wsInstance.emitMessage({
      id: 1,
      organizationId: "org-1",
      repositoryId: "repo-other",
      stream: "sessions",
      payload: {},
      createdAt: "2026-08-17T10:00:00Z",
    });
    expect(receivedEvents).toHaveLength(0);

    // Event for matching repository should be delivered
    wsInstance.emitMessage({
      id: 2,
      organizationId: "org-1",
      repositoryId: "repo-allowed",
      stream: "sessions",
      payload: {},
      createdAt: "2026-08-17T10:01:00Z",
    });
    expect(receivedEvents).toHaveLength(1);

    client.disconnect();
  });
});
