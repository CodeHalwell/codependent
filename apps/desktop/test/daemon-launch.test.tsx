/**
 * Starting the daemon from the app.
 *
 * Every first launch used to land on a raw socket error and a reconnect loop
 * that could never succeed, because nothing had started a daemon and nothing
 * said one was needed. The banner now says what is missing, starts it through
 * the shell, retries on demand, and names the terminal command when it cannot.
 */
import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { App } from "../src/App.js";
import type {
  ConnectionInfo,
  DaemonFrame,
  DaemonLaunchStatus,
  DaemonStartOutcome,
  DesktopTransport,
  RunHandle,
  SessionRow,
} from "../src/transport.js";
import type {
  AnalyticsExportRequest,
  AnalyticsExportResult,
  AnalyticsPage,
  InboxEntry,
  InboxMutation,
  InboxPage,
} from "@codypendent/protocol";

class LaunchStub implements DesktopTransport {
  /** Whether a daemon answers; flipped by `startDaemon`. */
  listening = false;
  connectAttempts = 0;
  startCalls = 0;
  /** Set to make `startDaemon` fail the way the shell does with no binary. */
  startFailure: string | null = null;
  invocation: DaemonLaunchStatus["invocation"] = { program: "/usr/local/bin/codypendent", args: ["__daemon"] };

  socketPath(): Promise<string> {
    return Promise.resolve("/tmp/codypendent/daemon.sock");
  }

  /** How many times the banner has asked the shell for launch status. */
  launchStatusCalls = 0;

  connect(_onFrame: (frame: DaemonFrame) => void): Promise<ConnectionInfo> {
    this.connectAttempts += 1;
    if (!this.listening) {
      return Promise.reject(new Error("No such file or directory (os error 2)"));
    }
    return Promise.resolve({
      socket_path: "/tmp/codypendent/daemon.sock",
      protocol_version: "1.4",
      daemon_version: "0.13.0",
      daemon_instance: "instance-1",
      build_id: "build-1",
      client_version: "0.14.0",
    });
  }

  disconnect(): Promise<void> {
    return Promise.resolve();
  }

  daemonLaunchStatus(): Promise<DaemonLaunchStatus> {
    this.launchStatusCalls += 1;
    return Promise.resolve({
      socketPath: "/tmp/codypendent/daemon.sock",
      listening: this.listening,
      invocation: this.invocation,
      source: this.invocation ? "path" : undefined,
      manualCommand: "codypendent daemon start",
      logPath: "/tmp/codypendent/logs/daemon.log",
      searched: this.invocation ? [] : ["/usr/local/bin/codypendent"],
    });
  }

  startDaemon(): Promise<DaemonStartOutcome> {
    this.startCalls += 1;
    if (this.startFailure) {
      return Promise.reject(new Error(this.startFailure));
    }
    this.listening = true;
    return Promise.resolve({ outcome: "started", pid: 4242, program: "/usr/local/bin/codypendent" });
  }

  listSessions(): Promise<SessionRow[]> {
    return Promise.resolve([]);
  }

  startObjective(): Promise<RunHandle> {
    return Promise.resolve({ session_id: "session-1", run_id: "run-1" });
  }

  attachSession(): Promise<void> {
    return Promise.resolve();
  }

  cancelRun(): Promise<void> {
    return Promise.resolve();
  }

  resolveApproval(): Promise<void> {
    return Promise.resolve();
  }

  listInbox(): Promise<InboxPage> {
    return Promise.resolve({ items: [], next_cursor: null });
  }

  mutateInbox(_mutation: InboxMutation): Promise<InboxEntry> {
    return Promise.reject(new Error("not in this test"));
  }

  queryAnalytics(): Promise<AnalyticsPage> {
    return Promise.resolve({ items: [], next_cursor: null });
  }

  exportAnalytics(_request: AnalyticsExportRequest): Promise<AnalyticsExportResult> {
    return Promise.reject(new Error("not in this test"));
  }

  readArtifact(): Promise<Uint8Array> {
    return Promise.resolve(new Uint8Array());
  }
}

async function settle() {
  await act(async () => undefined);
  await act(async () => undefined);
}

describe("starting the daemon from the app", () => {
  it("explains what is missing and what the shell would run", async () => {
    const stub = new LaunchStub();
    render(<App makeTransport={() => stub} />);
    await settle();
    const banner = screen.getByTestId("connection-banner");
    expect(banner.textContent).toContain("No daemon is listening on /tmp/codypendent/daemon.sock");
    expect(banner.textContent).toContain("/usr/local/bin/codypendent");
    expect(banner.textContent).toContain("codypendent daemon start");
    expect(screen.getByTestId("start-daemon")).toBeTruthy();
    expect(screen.getByTestId("retry-connect")).toBeTruthy();
  });

  it("probes for launch status once, not again on every unrelated render", async () => {
    // `ConnectionBanner` keys an effect on the `launchStatus` callback, so
    // binding it during render gave the banner a new identity on EVERY parent
    // render. While disconnected each one re-ran the effect and fired another
    // probe; against a silent socket those calls stay pending for the whole
    // ping budget, so they overlap and accumulate.
    const stub = new LaunchStub();
    render(<App makeTransport={() => stub} />);
    await settle();
    expect(stub.launchStatusCalls).toBe(1);

    // Opening the palette is an ordinary parent render that says nothing about
    // the link, and it is one of the interactions that used to re-probe.
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Open command palette"));
    });
    await settle();
    expect(stub.launchStatusCalls).toBe(1);
  });

  it("starts the daemon and connects as soon as it answers", async () => {
    const stub = new LaunchStub();
    render(<App makeTransport={() => stub} />);
    await settle();
    const before = stub.connectAttempts;
    await act(async () => {
      fireEvent.click(screen.getByTestId("start-daemon"));
    });
    await settle();
    expect(stub.startCalls).toBe(1);
    expect(stub.connectAttempts).toBeGreaterThan(before);
    // Connected: the banner is gone and the footer names both versions — and
    // that they differ.
    expect(screen.queryByTestId("connection-banner")).toBeNull();
    const footer = screen.getByTestId("version-footer");
    expect(footer.textContent).toContain("daemon 0.13.0");
    expect(footer.textContent).toContain("desktop 0.14.0");
    expect(footer.textContent).toContain("different version");
  });

  it("reports why the shell could not start one, with the manual command", async () => {
    const stub = new LaunchStub();
    stub.invocation = undefined;
    stub.startFailure =
      "no codypendent program was found to start the daemon with. Run `codypendent daemon start` in a terminal.";
    render(<App makeTransport={() => stub} />);
    await settle();
    expect(screen.getByTestId("connection-banner").textContent).toContain(
      "program was not found on this machine",
    );
    await act(async () => {
      fireEvent.click(screen.getByTestId("start-daemon"));
    });
    await settle();
    expect(screen.getByTestId("start-daemon-outcome").textContent).toContain(
      "no codypendent program was found",
    );
    expect(screen.getByTestId("connection-banner")).toBeTruthy();
  });

  it("retries the connection on demand", async () => {
    const stub = new LaunchStub();
    render(<App makeTransport={() => stub} />);
    await settle();
    const before = stub.connectAttempts;
    // Something started a daemon outside the app.
    stub.listening = true;
    await act(async () => {
      fireEvent.click(screen.getByTestId("retry-connect"));
    });
    await settle();
    expect(stub.connectAttempts).toBeGreaterThan(before);
    expect(screen.queryByTestId("connection-banner")).toBeNull();
  });
});
