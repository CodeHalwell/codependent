import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App.js";
import { NO_SHELL_DETAIL } from "../src/useDaemon.js";
import type { ConnectionInfo, DaemonFrame, DesktopTransport, RunHandle, SessionRow } from "../src/transport.js";

/**
 * A stand-in for the Tauri shell bridge. It records what the UI actually sent
 * and lets a test push the frames a daemon would emit, so "the transcript only
 * ever shows what the daemon said" is an assertion rather than a claim.
 */
class StubTransport implements DesktopTransport {
  readonly objectives: string[] = [];
  readonly cancelled: string[] = [];
  readonly attached: string[] = [];
  private frames: ((frame: DaemonFrame) => void) | null = null;

  constructor(
    private readonly options: {
      connect?: () => Promise<ConnectionInfo>;
      sessions?: SessionRow[];
      run?: RunHandle;
    } = {},
  ) {}

  socketPath(): Promise<string> {
    return Promise.resolve("/tmp/codypendent/daemon.sock");
  }

  connect(onFrame: (frame: DaemonFrame) => void): Promise<ConnectionInfo> {
    this.frames = onFrame;
    if (this.options.connect) {
      return this.options.connect();
    }
    return Promise.resolve({
      socket_path: "/tmp/codypendent/daemon.sock",
      protocol_version: "1.4",
      daemon_version: "0.9.0",
      daemon_instance: "instance-1",
      build_id: "build-1",
    });
  }

  disconnect(): Promise<void> {
    return Promise.resolve();
  }

  listSessions(): Promise<SessionRow[]> {
    return Promise.resolve(this.options.sessions ?? []);
  }

  startObjective(objective: string): Promise<RunHandle> {
    this.objectives.push(objective);
    return Promise.resolve(this.options.run ?? { session_id: "session-1", run_id: "run-1" });
  }

  attachSession(sessionId: string): Promise<void> {
    this.attached.push(sessionId);
    return Promise.resolve();
  }

  cancelRun(runId: string): Promise<void> {
    this.cancelled.push(runId);
    return Promise.resolve();
  }

  /** Push a frame the way the shell's channel would. */
  async push(frame: DaemonFrame): Promise<void> {
    await act(async () => {
      this.frames?.(frame);
    });
  }
}

function event(sequence: number, body: Record<string, unknown>): DaemonFrame {
  return {
    kind: "event",
    session_id: "session-1",
    event: {
      sequence,
      occurred_at: "2026-08-16T10:00:00Z",
      body: body as { type: string },
    },
  };
}

async function renderWith(transport: StubTransport) {
  const factory = () => transport as DesktopTransport;
  render(<App makeTransport={factory} />);
  // Let the connect promise (and the session listing behind it) settle.
  await act(async () => undefined);
}

describe("desktop client with no daemon transport", () => {
  afterEach(() => vi.useRealTimers());

  it("reports disconnected when the app is not running inside the shell", () => {
    // No stub: the real factory runs, finds no Tauri shell, and returns null.
    render(<App />);

    expect(screen.getByText("codypendentd: disconnected")).toBeTruthy();
    expect(screen.getAllByText(NO_SHELL_DETAIL).length).toBeGreaterThan(0);
    expect(screen.getByText("Not connected to codypendentd")).toBeTruthy();
    expect(screen.queryByText("codypendentd: connected")).toBeNull();
  });

  it("disables run controls and never fabricates a response over time", () => {
    vi.useFakeTimers();
    render(<App />);

    const objective = screen.getByRole("textbox") as HTMLTextAreaElement;
    const send = screen.getByRole("button", { name: "Send" }) as HTMLButtonElement;
    expect(objective.disabled).toBe(true);
    expect(send.disabled).toBe(true);
    expect(objective.placeholder).toBe("Not connected to codypendentd — runs cannot be submitted.");

    fireEvent.change(objective, { target: { value: "do the thing" } });
    fireEvent.click(send);
    act(() => vi.advanceTimersByTime(60_000));

    // Nothing outside the composer's own textarea ever echoed the objective:
    // no transcript entry, no session, no run.
    const echoes = screen
      .queryAllByText(/do the thing/)
      .filter((element) => element.tagName !== "TEXTAREA");
    expect(echoes).toEqual([]);
    expect(screen.getByText("No sessions (not connected)")).toBeTruthy();
    expect(screen.getByText("Not connected to codypendentd")).toBeTruthy();
  });

  it("reports the daemon's own failure reason when the socket is unreachable", async () => {
    const transport = new StubTransport({
      connect: () => Promise.reject(new Error("No such file or directory (os error 2)")),
    });
    await renderWith(transport);

    expect(screen.getByText("codypendentd: disconnected")).toBeTruthy();
    expect(
      screen.getAllByText(
        /No daemon on \/tmp\/codypendent\/daemon\.sock: No such file or directory/,
      ).length,
    ).toBeGreaterThan(0);
    expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("desktop client connected to a daemon", () => {
  it("reports connected only after a handshake, listing the daemon it reached", async () => {
    const transport = new StubTransport({
      sessions: [
        {
          session_id: "session-1",
          title: "Earlier session",
          state: "open",
          created_at: "2026-08-16T09:00:00Z",
          updated_at: "2026-08-16T09:30:00Z",
        },
      ],
    });
    await renderWith(transport);

    expect(screen.getByText("codypendentd: connected")).toBeTruthy();
    expect(screen.getByText("codypendentd 0.9.0 on /tmp/codypendent/daemon.sock")).toBeTruthy();
    expect(screen.getByText("Earlier session")).toBeTruthy();
  });

  it("turns a submitted objective into a real outbound command and shows only daemon output", async () => {
    const transport = new StubTransport();
    await renderWith(transport);

    const objective = screen.getByRole("textbox") as HTMLTextAreaElement;
    fireEvent.change(objective, { target: { value: "refactor the parser" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Send" }));
    });

    // The command really went out...
    expect(transport.objectives).toEqual(["refactor the parser"]);
    // ...and nothing appeared in the transcript that the daemon did not emit.
    expect(screen.queryByText("refactor the parser")).toBeNull();

    await transport.push(
      event(1, { type: "RunStarted", run_id: "run-1", objective: "refactor the parser", mode: { type: "Build" } }),
    );
    expect(screen.getByText("refactor the parser")).toBeTruthy();

    await transport.push(event(2, { type: "ModelStreamDelta", run_id: "run-1", text: "Reading " }));
    await transport.push(event(3, { type: "ModelStreamDelta", run_id: "run-1", text: "the parser." }));
    expect(screen.getByText("Reading the parser.")).toBeTruthy();
  });

  it("sends a real CancelRun for the run the daemon named", async () => {
    const transport = new StubTransport();
    await renderWith(transport);

    fireEvent.change(screen.getByRole("textbox"), { target: { value: "build it" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Send" }));
    });
    await transport.push(event(1, { type: "RunStarted", run_id: "run-1", objective: "build it" }));

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Cancel Run" }));
    });
    expect(transport.cancelled).toEqual(["run-1"]);
  });

  it("falls back to disconnected when the socket drops mid-run", async () => {
    const transport = new StubTransport();
    await renderWith(transport);
    expect(screen.getByText("codypendentd: connected")).toBeTruthy();

    await transport.push({ kind: "disconnected", reason: "the daemon closed the connection" });

    expect(screen.getByText("codypendentd: disconnected")).toBeTruthy();
    expect(screen.getAllByText("the daemon closed the connection").length).toBeGreaterThan(0);
    expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("reports a rejected command without inventing agent output", async () => {
    const transport = new StubTransport();
    transport.startObjective = () => Promise.reject(new Error("StartRun rejected: policy denied"));
    await renderWith(transport);

    fireEvent.change(screen.getByRole("textbox"), { target: { value: "rm -rf /" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Send" }));
    });

    expect(screen.getByRole("alert").textContent).toContain("StartRun rejected: policy denied");
    expect(screen.getByText("Ready")).toBeTruthy();
  });
});
