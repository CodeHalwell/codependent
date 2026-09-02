import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type {
  ConnectionInfo,
  DaemonFrame,
  DesktopTransport,
  RunHandle,
} from "../src/transport.js";
import { useDaemon } from "../src/useDaemon.js";

const INFO: ConnectionInfo = {
  socket_path: "/tmp/codypendent/daemon.sock",
  protocol_version: "1.4",
  daemon_version: "0.14.0",
  daemon_instance: "launch-guard",
  build_id: "launch-guard",
};

/**
 * A transport whose launch never settles until the test says so — the window
 * in which the daemon has not yet answered and `isRunning` is therefore still
 * false.
 */
class LaunchTransport {
  readonly objectives: string[] = [];
  private release: ((handle: RunHandle) => void) | null = null;

  connect(_handler: (frame: DaemonFrame) => void): Promise<ConnectionInfo> {
    return Promise.resolve(INFO);
  }

  disconnect(): Promise<void> {
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

  startObjective(objective: string): Promise<RunHandle> {
    this.objectives.push(objective);
    return new Promise<RunHandle>((resolve) => {
      this.release = resolve;
    });
  }

  /** Let the daemon answer the launch that is waiting. */
  answer(): void {
    this.release?.({ session_id: "session-1", run_id: "run-1" });
  }
}

function renderDaemon(transport: LaunchTransport) {
  return renderHook(() => useDaemon(() => transport as unknown as DesktopTransport));
}

describe("a launch already in flight", () => {
  it("refuses a second concurrent submit instead of starting a second paid run", async () => {
    const transport = new LaunchTransport();
    const daemon = renderDaemon(transport);
    await waitFor(() => expect(daemon.result.current.state.status).toBe("connected"));

    // Two clicks inside the window before the daemon answers — a double-click
    // on Send, or on a failure card's Retry, which is enabled precisely while
    // `isRunning` is false.
    let first: Promise<boolean> = Promise.resolve(false);
    let second: Promise<boolean> = Promise.resolve(false);
    await act(async () => {
      first = daemon.result.current.submit("ship the thing");
      second = daemon.result.current.submit("ship the thing");
      transport.answer();
      await Promise.all([first, second]);
    });

    expect(await first).toBe(true);
    expect(await second).toBe(false);
    expect(transport.objectives).toEqual(["ship the thing"]);
  });

  it("accepts the next submit once the daemon has answered the last one", async () => {
    const transport = new LaunchTransport();
    const daemon = renderDaemon(transport);
    await waitFor(() => expect(daemon.result.current.state.status).toBe("connected"));

    await act(async () => {
      const launch = daemon.result.current.submit("first objective");
      transport.answer();
      await launch;
    });
    await act(async () => {
      const launch = daemon.result.current.submit("second objective");
      transport.answer();
      await launch;
    });

    expect(transport.objectives).toEqual(["first objective", "second objective"]);
  });
});
