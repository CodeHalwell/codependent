/**
 * What the operator sees while a run is doing something other than streaming
 * text, and when it fails.
 *
 * Every assertion is about an event the daemon already sends and the desktop
 * used to drop on the floor: `ModelRetrying` (a rate-limit backoff looked like
 * a hang), `ToolDenied` (a policy block vanished), `RunUsage` (tokens and cost
 * were never shown), and a `Failed` disposition (rendered as dim grey centred
 * text with no way forward).
 */
import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { App } from "../src/App.js";
import type {
  ConnectionInfo,
  DaemonFrame,
  DesktopTransport,
  RunHandle,
  SessionRow,
} from "../src/transport.js";
import type {
  AnalyticsExportRequest,
  AnalyticsExportResult,
  AnalyticsPage,
  EventBody,
  InboxEntry,
  InboxMutation,
  InboxPage,
  SessionEvent,
} from "@codypendent/protocol";

class FeedbackStub implements DesktopTransport {
  readonly objectives: string[] = [];
  /** Set to make the next `startObjective` fail the way a daemon refusal does. */
  refusal: string | null = null;
  private frames: ((frame: DaemonFrame) => void) | null = null;

  socketPath(): Promise<string> {
    return Promise.resolve("/tmp/codypendent/daemon.sock");
  }

  connect(onFrame: (frame: DaemonFrame) => void): Promise<ConnectionInfo> {
    this.frames = onFrame;
    return Promise.resolve({
      socket_path: "/tmp/codypendent/daemon.sock",
      protocol_version: "1.4",
      daemon_version: "0.14.0",
      daemon_instance: "instance-1",
      build_id: "build-1",
    });
  }

  disconnect(): Promise<void> {
    return Promise.resolve();
  }

  listSessions(): Promise<SessionRow[]> {
    return Promise.resolve([]);
  }

  startObjective(objective: string): Promise<RunHandle> {
    if (this.refusal) {
      return Promise.reject(new Error(this.refusal));
    }
    this.objectives.push(objective);
    return Promise.resolve({ session_id: "session-1", run_id: `run-${this.objectives.length}` });
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

  async emit(sequence: number, body: EventBody): Promise<void> {
    const event: SessionEvent = {
      sequence,
      actor: { type: "System" },
      occurred_at: "2026-09-01T10:00:00Z",
      body,
    };
    await act(async () => {
      this.frames?.({ kind: "event", session_id: "session-1", event });
    });
  }
}

const CHRONICLE = {
  id: "art-chronicle",
  sha256: "0000000000000000000000000000000000000000000000000000000000000000",
  byte_length: 10,
  media_type: "application/json",
  sensitivity: { type: "Public" as const },
};

async function startRun(stub: FeedbackStub, objective = "Fix the flaky test") {
  render(<App makeTransport={() => stub} />);
  await act(async () => undefined);
  const textarea = screen.getByRole("textbox") as HTMLTextAreaElement;
  fireEvent.change(textarea, { target: { value: objective } });
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
  });
  await stub.emit(1, { type: "RunStarted", run_id: "run-1", objective, mode: { type: "Build" } });
  return textarea;
}

describe("run feedback", () => {
  it("shows a working row from the moment the run starts", async () => {
    const stub = new FeedbackStub();
    await startRun(stub);
    expect(screen.getByTestId("run-working").textContent).toContain("working…");
    expect(screen.getByTestId("composer-activity").textContent).toContain("working…");
  });

  it("names the running tool, then goes back to working when it returns", async () => {
    const stub = new FeedbackStub();
    await startRun(stub);
    await stub.emit(2, {
      type: "ToolStarted",
      run_id: "run-1",
      tool: "shell.run",
      args_digest: "abc",
      label: "cargo test",
    });
    expect(screen.getByTestId("run-working").textContent).toContain("running shell.run…");
    await stub.emit(3, {
      type: "ToolCompleted",
      run_id: "run-1",
      tool: "shell.run",
      outcome: { type: "Succeeded" },
      artifact: null,
    });
    expect(screen.getByTestId("run-working").textContent).toContain("working…");
  });

  it("shows the provider's retry reason and countdown instead of a silent stall", async () => {
    const stub = new FeedbackStub();
    await startRun(stub);
    await stub.emit(2, {
      type: "ModelRetrying",
      run_id: "run-1",
      attempt: 2,
      max_attempts: 5,
      message: "provider is overloaded",
      delay_ms: 4231,
    });
    expect(screen.getByTestId("composer-activity").textContent).toContain(
      "retrying (2/5) · provider is overloaded · next attempt in 4s",
    );
    expect(
      screen.getByText("Retrying (2/5): provider is overloaded — next attempt in 4s"),
    ).toBeTruthy();
  });

  it("keeps a policy denial in the transcript", async () => {
    const stub = new FeedbackStub();
    await startRun(stub);
    await stub.emit(2, {
      type: "ToolDenied",
      run_id: "run-1",
      action: {
        type: "ExecuteCommand",
        program: "rm",
        args: ["-rf", "/"],
        environment: [],
        cwd: null,
      },
      reasons: ["`rm` is not in the shell allow-list"],
    });
    expect(
      screen.getByText("Blocked by policy: ExecuteCommand: rm -rf / — `rm` is not in the shell allow-list"),
    ).toBeTruthy();
  });

  it("hides the working row while the reply streams and shows usage once the run ends", async () => {
    const stub = new FeedbackStub();
    await startRun(stub);
    await stub.emit(2, { type: "ModelStreamDelta", run_id: "run-1", text: "On it.", thought: false });
    expect(screen.queryByTestId("run-working")).toBeNull();
    await stub.emit(3, {
      type: "RunUsage",
      run_id: "run-1",
      prompt_tokens: 1234,
      completion_tokens: 567,
    });
    await stub.emit(4, {
      type: "RunCompleted",
      run_id: "run-1",
      disposition: { type: "Completed", summary: "On it." },
      chronicle: CHRONICLE,
    });
    expect(screen.queryByTestId("run-working")).toBeNull();
    // Tokens without a price: no `$0.00` that would read as "free".
    expect(screen.getByTestId("composer-usage").textContent).toContain("1,234 in · 567 out");
    expect(screen.getByTestId("composer-usage").textContent).not.toContain("$");
  });

  it("renders a failed run as a card with a next step, and retries with the same objective", async () => {
    const stub = new FeedbackStub();
    await startRun(stub, "Refactor the parser");
    await stub.emit(2, {
      type: "RunCompleted",
      run_id: "run-1",
      disposition: {
        type: "Failed",
        reason:
          'model driver error: OpenAI-compatible API error 401 Unauthorized: {"error":{"message":"Incorrect API key sk-live-123"}}',
      },
      chronicle: CHRONICLE,
    });
    const card = screen.getByTestId("run-failure");
    expect(card.textContent).toContain("Run failed: model error — the provider request failed");
    expect(card.textContent).toContain("Check the key under API Keys");
    // The secret in the provider's body never reaches the screen.
    expect(card.textContent).not.toContain("sk-live-123");
    expect(screen.getByRole("button", { name: "Open API keys" })).toBeTruthy();

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    });
    expect(stub.objectives).toEqual(["Refactor the parser", "Refactor the parser"]);
  });

  it("keeps the draft when the daemon refuses the run", async () => {
    const stub = new FeedbackStub();
    stub.refusal = "no model configured (no models.toml)";
    render(<App makeTransport={() => stub} />);
    await act(async () => undefined);
    const textarea = screen.getByRole("textbox") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "A long, carefully typed objective" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Send" }));
    });
    expect(textarea.value).toBe("A long, carefully typed objective");
    expect(screen.getByText("no model configured (no models.toml)")).toBeTruthy();
  });

  it("clears an accepted draft that was typed with surrounding whitespace", async () => {
    // The guard that protects a pending edit compares the box against what was
    // sent. What is SENT is trimmed; what the box HOLDS is raw. A draft ending
    // in a newline — one shift+Enter, or a paste — therefore never matched
    // itself, so an accepted objective stayed on screen looking unsent and one
    // more Enter ran it a second time.
    const stub = new FeedbackStub();
    render(<App makeTransport={() => stub} />);
    await act(async () => undefined);
    const textarea = screen.getByRole("textbox") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "  Refactor the parser\n" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Send" }));
    });
    // Trimmed on the way out, and gone from the box on the way back.
    expect(stub.objectives).toEqual(["Refactor the parser"]);
    expect(textarea.value).toBe("");
  });
});
