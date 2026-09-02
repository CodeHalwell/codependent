import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { SessionEvent } from "@codypendent/protocol";
import type {
  ConnectionInfo,
  DaemonFrame,
  DesktopTransport,
} from "../src/transport.js";
import { useDaemon } from "../src/useDaemon.js";

const INFO: ConnectionInfo = {
  socket_path: "/tmp/codypendent/daemon.sock",
  protocol_version: "1.4",
  daemon_version: "0.13.0",
  daemon_instance: "boundary-test",
  build_id: "boundary-test",
};

type Deferred = {
  promise: Promise<void>;
  resolve: () => void;
};

function deferred(): Deferred {
  let resolve: () => void = () => undefined;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function event(sessionId: string, sequence: number, text = `${sessionId}-${sequence}`): DaemonFrame {
  return {
    kind: "event",
    session_id: sessionId,
    event: {
      sequence,
      actor: { type: "System" },
      occurred_at: "2026-08-29T00:00:00Z",
      body: { type: "ModelStreamDelta", run_id: "run-1", text },
    },
  };
}

class BoundaryTransport {
  private handler: ((frame: DaemonFrame) => void) | null = null;
  private activeGeneration = -1;
  readonly failedAttachments = new Set<string>();
  readonly failHistoryAfterAcceptedAttach = new Set<string>();
  readonly rangeCalls: Array<{ sessionId: string; after: number; through: number }> = [];
  readonly attachmentCalls: string[] = [];
  readonly queuedPrompts: string[] = [];
  readonly deferredAttachments = new Map<string, Deferred>();
  failNextRange = false;

  connect(handler: (frame: DaemonFrame) => void, generation = 0): Promise<ConnectionInfo> {
    this.handler = handler;
    this.activeGeneration = generation;
    return Promise.resolve(INFO);
  }

  disconnect(generation?: number): Promise<void> {
    if (generation === undefined || generation === this.activeGeneration) {
      this.handler = null;
    }
    return Promise.resolve();
  }

  socketPath(): Promise<string> {
    return Promise.resolve(INFO.socket_path);
  }

  listSessions(): Promise<[]> {
    return Promise.resolve([]);
  }

  listInbox() {
    return Promise.resolve({ items: [], next_cursor: null });
  }

  attachSession(sessionId: string): Promise<void> {
    this.attachmentCalls.push(sessionId);
    if (this.failedAttachments.has(sessionId)) {
      return Promise.reject(new Error(`cannot attach ${sessionId}`));
    }
    if (this.failHistoryAfterAcceptedAttach.has(sessionId)) {
      this.handler?.({
        kind: "catchup",
        session_id: sessionId,
        snapshot: {
          type: "Snapshot",
          through: 1,
          projection: {
            session_id: sessionId,
            title: sessionId,
            last_sequence: 1,
            closed: false,
          },
        },
      });
      return Promise.reject(new Error(`history failed for ${sessionId}`));
    }
    return this.deferredAttachments.get(sessionId)?.promise ?? Promise.resolve();
  }

  queuePrompt(text: string): Promise<void> {
    this.queuedPrompts.push(text);
    return Promise.resolve();
  }

  readSessionEventRange(
    sessionId: string,
    after: number,
    through: number,
  ): Promise<SessionEvent[]> {
    this.rangeCalls.push({ sessionId, after, through });
    if (this.failNextRange) {
      this.failNextRange = false;
      return Promise.reject(new Error("range temporarily unavailable"));
    }
    return Promise.resolve([
      {
        sequence: after + 1,
        actor: { type: "System" },
        occurred_at: "2026-08-29T00:00:00Z",
        body: { type: "ModelStreamDelta", run_id: "run-1", text: "restored" },
      },
    ]);
  }

  async push(frame: DaemonFrame): Promise<void> {
    await act(async () => this.handler?.(frame));
  }
}

function renderDaemon(transport: BoundaryTransport) {
  return renderHook(() => useDaemon(() => transport as unknown as DesktopTransport));
}

describe("desktop session attachment boundaries", () => {
  it("keeps the last confirmed session when a new attach is refused", async () => {
    const transport = new BoundaryTransport();
    const daemon = renderDaemon(transport);
    await waitFor(() => expect(daemon.result.current.state.status).toBe("connected"));

    await act(async () => daemon.result.current.selectSession("session-a"));
    expect(daemon.result.current.state.activeSessionId).toBe("session-a");

    transport.failedAttachments.add("session-b");
    await act(async () => daemon.result.current.selectSession("session-b"));

    expect(daemon.result.current.state.activeSessionId).toBe("session-a");
    expect(daemon.result.current.state.attachingSessionId).toBeNull();
    expect(daemon.result.current.state.error).toBe("cannot attach session-b");
  });

  it("commits an accepted session when only its paged history restoration fails", async () => {
    const transport = new BoundaryTransport();
    const daemon = renderDaemon(transport);
    await waitFor(() => expect(daemon.result.current.state.status).toBe("connected"));
    await act(async () => daemon.result.current.selectSession("session-a"));

    transport.failHistoryAfterAcceptedAttach.add("session-b");
    await act(async () => daemon.result.current.selectSession("session-b"));

    expect(daemon.result.current.state.activeSessionId).toBe("session-b");
    expect(daemon.result.current.state.sessionAttachmentConfirmed).toBe(true);
    expect(daemon.result.current.state.error).toContain(
      "attached, but its complete history could not be restored yet",
    );
    await waitFor(() =>
      expect(transport.rangeCalls).toContainEqual({
        sessionId: "session-b",
        after: 0,
        through: 1,
      }),
    );
  });

  it("gates session commands until reconnect reattachment is confirmed", async () => {
    const transport = new BoundaryTransport();
    const daemon = renderDaemon(transport);
    await waitFor(() => expect(daemon.result.current.state.status).toBe("connected"));
    await act(async () => daemon.result.current.selectSession("session-a"));

    const resume = deferred();
    transport.deferredAttachments.set("session-a", resume);
    // Not awaited INSIDE `act`: `reconnect()` now resolves only once the
    // attempt it starts has connected, and that connection happens in an
    // effect `act` flushes after its callback returns. Awaiting it here would
    // deadlock the two. These tests assert on the state that follows, which
    // `waitFor` below is the right tool for.
    await act(async () => {
      void daemon.result.current.reconnect();
    });
    await waitFor(() => expect(daemon.result.current.state.connectionEpoch).toBe(2));
    await waitFor(() => expect(daemon.result.current.state.attachingSessionId).toBe("session-a"));
    expect(daemon.result.current.state.sessionAttachmentConfirmed).toBe(false);

    let queued = true;
    await act(async () => {
      queued = await daemon.result.current.queuePrompt("must wait");
    });
    expect(queued).toBe(false);
    expect(transport.queuedPrompts).toEqual([]);

    await act(async () => resume.resolve());
    await waitFor(() => expect(daemon.result.current.state.attachingSessionId).toBeNull());
    expect(daemon.result.current.state.sessionAttachmentConfirmed).toBe(true);
    expect(daemon.result.current.state.activeSessionId).toBe("session-a");
  });

  it("keeps session commands gated when reconnect reattachment is refused", async () => {
    const transport = new BoundaryTransport();
    const daemon = renderDaemon(transport);
    await waitFor(() => expect(daemon.result.current.state.status).toBe("connected"));
    await act(async () => daemon.result.current.selectSession("session-a"));

    transport.failedAttachments.add("session-a");
    await act(async () => {
      void daemon.result.current.reconnect();
    });
    await waitFor(() => expect(daemon.result.current.state.connectionEpoch).toBe(2));
    await waitFor(() => expect(daemon.result.current.state.attachingSessionId).toBeNull());

    expect(daemon.result.current.state.activeSessionId).toBe("session-a");
    expect(daemon.result.current.state.sessionAttachmentConfirmed).toBe(false);
    let queued = true;
    await act(async () => {
      queued = await daemon.result.current.queuePrompt("must not send");
    });
    expect(queued).toBe(false);
    expect(transport.queuedPrompts).toEqual([]);
  });

  it("retries an unrepaired gap after a new connection is established", async () => {
    const transport = new BoundaryTransport();
    transport.failNextRange = true;
    const daemon = renderDaemon(transport);
    await waitFor(() => expect(daemon.result.current.state.status).toBe("connected"));
    await act(async () => daemon.result.current.selectSession("session-a"));

    await transport.push(event("session-a", 1));
    await transport.push(event("session-a", 3));
    await waitFor(() => expect(transport.rangeCalls).toHaveLength(1));
    expect(daemon.result.current.state.pendingGap?.sessionId).toBe("session-a");

    await act(async () => {
      void daemon.result.current.reconnect();
    });
    await waitFor(() => expect(daemon.result.current.state.connectionEpoch).toBe(2));
    await waitFor(() => expect(transport.rangeCalls).toHaveLength(2));
    await waitFor(() => expect(daemon.result.current.state.pendingGap).toBeNull());
    expect(transport.rangeCalls.map((call) => call.sessionId)).toEqual([
      "session-a",
      "session-a",
    ]);
  });

  it("drops late catch-up and history from A after B is committed", async () => {
    const transport = new BoundaryTransport();
    const daemon = renderDaemon(transport);
    await waitFor(() => expect(daemon.result.current.state.status).toBe("connected"));
    await act(async () => daemon.result.current.selectSession("session-a"));
    await transport.push(event("session-a", 1, "from A"));
    expect(daemon.result.current.state.transcript).toHaveLength(1);

    await act(async () => daemon.result.current.selectSession("session-b"));
    await transport.push({
      kind: "history",
      session_id: "session-a",
      through: 1,
      events: [(event("session-a", 1, "late A") as Extract<DaemonFrame, { kind: "event" }>).event],
    });
    await transport.push({
      kind: "catchup",
      session_id: "session-a",
      snapshot: { type: "Events", from: 1, through: 0, events: [] },
    });

    expect(daemon.result.current.state.activeSessionId).toBe("session-b");
    expect(daemon.result.current.state.transcript).toEqual([]);
    expect(daemon.result.current.state.durableEvents).toEqual([]);
  });
});
