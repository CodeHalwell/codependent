import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type {
  AnalyticsExportRequest,
  AnalyticsExportResult,
  AnalyticsPage,
  AnalyticsQuery,
  ArtifactRef,
  EventBody,
  InboxEntry,
  InboxListQuery,
  InboxMutation,
  InboxPage,
  SessionEvent,
  SessionSummary,
} from "@codypendent/protocol";
import { App } from "../src/App.js";
import type {
  ApprovalChoice,
  ConnectionInfo,
  DaemonFrame,
  DesktopTransport,
  RunHandle,
} from "../src/transport.js";

class MockDesktopTransport implements DesktopTransport {
  readonly socketPathValue: string;
  readonly connectionInfo: ConnectionInfo;
  readonly sessionsList: SessionSummary[] = [];
  readonly startObjectiveCalls: string[] = [];
  readonly attachSessionCalls: string[] = [];
  readonly cancelRunCalls: string[] = [];
  readonly resolveApprovalCalls: Array<{ approvalId: string; decision: ApprovalChoice }> = [];
  readonly artifacts: Map<string, string> = new Map();

  private frameHandler: ((frame: DaemonFrame) => void) | null = null;

  constructor(
    options: {
      socketPath?: string;
      connectionInfo?: ConnectionInfo;
      sessions?: SessionSummary[];
      artifacts?: Record<string, string>;
    } = {},
  ) {
    this.socketPathValue = options.socketPath ?? "/tmp/codypendent/daemon.sock";
    this.connectionInfo = options.connectionInfo ?? {
      socket_path: this.socketPathValue,
      protocol_version: "1.4",
      daemon_version: "0.10.0",
      daemon_instance: "inst-001",
      build_id: "build-test",
    };
    if (options.sessions) {
      this.sessionsList.push(...options.sessions);
    }
    if (options.artifacts) {
      for (const [id, content] of Object.entries(options.artifacts)) {
        this.artifacts.set(id, content);
      }
    }
  }

  async socketPath(): Promise<string> {
    return this.socketPathValue;
  }

  async connect(onFrame: (frame: DaemonFrame) => void): Promise<ConnectionInfo> {
    this.frameHandler = onFrame;
    return this.connectionInfo;
  }

  async disconnect(): Promise<void> {
    this.frameHandler = null;
  }

  async listSessions(): Promise<SessionSummary[]> {
    return [...this.sessionsList];
  }

  async startObjective(objective: string): Promise<RunHandle> {
    this.startObjectiveCalls.push(objective);
    const session_id = `session-${this.startObjectiveCalls.length}`;
    const run_id = `run-${this.startObjectiveCalls.length}`;
    this.sessionsList.push({
      session_id,
      title: objective,
      state: "open",
      created_at: "2026-08-17T10:00:00Z",
      updated_at: "2026-08-17T10:00:00Z",
    });
    return { session_id, run_id };
  }

  async attachSession(sessionId: string): Promise<void> {
    this.attachSessionCalls.push(sessionId);
  }

  async cancelRun(runId: string): Promise<void> {
    this.cancelRunCalls.push(runId);
  }

  async resolveApproval(approvalId: string, decision: ApprovalChoice): Promise<void> {
    this.resolveApprovalCalls.push({ approvalId, decision });
  }

  async listInbox(_query?: InboxListQuery): Promise<InboxPage> {
    return { items: [], next_cursor: null };
  }

  async mutateInbox(mutation: InboxMutation): Promise<InboxEntry> {
    return {
      id: "entry_id" in mutation ? mutation.entry_id : "inbox-1",
      repository_id: "repo-1",
      kind: { type: "ApprovalRequest" },
      state: mutation.type === "Acknowledge" ? { type: "Acknowledged" } : { type: "Dismissed" },
      title: "Inbox item",
      deep_link: { type: "Session", session_id: "session-1" },
      source: { dedup_key: "k", identity: { type: "Unknown" } },
      created_at: "2026-08-17T10:00:00Z",
    };
  }

  async queryAnalytics(_query?: AnalyticsQuery): Promise<AnalyticsPage> {
    return { items: [], next_cursor: null };
  }

  async exportAnalytics(request: AnalyticsExportRequest): Promise<AnalyticsExportResult> {
    return {
      artifact: {
        id: "art-1",
        byte_length: 128,
        media_type: "application/json",
        sensitivity: { type: "Public" },
        sha256: "0000000000000000000000000000000000000000000000000000000000000000",
      },
      format: request.format,
      generated_at: "2026-08-17T10:00:00Z",
      row_count: 1,
    };
  }

  async readArtifact(artifact: ArtifactRef): Promise<Uint8Array> {
    // Mirrors the real bridge: `ReadArtifact` binds the content address, not a
    // bare id, and the boundary returns bytes rather than a decoded string.
    const content = this.artifacts.get(artifact.id);
    if (content === undefined) {
      throw new Error(`artifact not found: ${artifact.id}`);
    }
    return new TextEncoder().encode(content);
  }

  async push(frame: DaemonFrame): Promise<void> {
    await act(async () => {
      this.frameHandler?.(frame);
    });
  }
}

function makeEvent(
  sequence: number,
  body: EventBody,
  occurred_at = "2026-08-17T10:00:00Z",
): SessionEvent {
  return {
    sequence,
    actor: { type: "System" },
    occurred_at,
    body,
  };
}

describe("daemon transport and session lifecycle integration", () => {
  it("desktop_projects_a_full_session_lifecycle", async () => {
    // 1. Discovery: retrieve socket path
    const transport = new MockDesktopTransport({
      artifacts: {
        "art-patch-42": "--- a/src/auth.ts\n+++ b/src/auth.ts\n@@ -1 +1 @@\n-export const auth = false;\n+export const auth = true;",
      },
    });
    const socketPath = await transport.socketPath();
    expect(socketPath).toBe("/tmp/codypendent/daemon.sock");

    // 2. Connect: mount App and handshake
    render(<App makeTransport={() => transport} />);
    await act(async () => undefined);

    // A healthy connection is stated by the sidebar dot; only an unhealthy one
    // raises the banner across the main pane.
    expect(screen.queryByTestId("connection-banner")).toBeNull();
    expect(
      screen.getByLabelText("codypendentd connected").getAttribute("title"),
    ).toBe("codypendentd 0.10.0 on /tmp/codypendent/daemon.sock");

    // 3. Create: start an objective to create a new session
    const textarea = screen.getByRole("textbox") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "Implement authentication" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Send" }));
    });

    expect(transport.startObjectiveCalls).toEqual(["Implement authentication"]);

    // 4. Attach: session selected and attached
    expect(screen.getAllByText("Implement authentication").length).toBeGreaterThanOrEqual(1);

    // 5. Paginated Catchup: replay history through sequence 4
    const historyEvents: SessionEvent[] = [
      makeEvent(1, {
        type: "RunStarted",
        run_id: "run-1",
        objective: "Implement authentication",
        mode: { type: "Build" },
      }),
      makeEvent(2, {
        type: "ModelStreamDelta",
        run_id: "run-1",
        text: "Analyzing requirements. ",
      }),
      makeEvent(3, {
        type: "ModelStreamDelta",
        run_id: "run-1",
        text: "Creating auth service. ",
      }),
      makeEvent(4, {
        type: "ToolStarted",
        run_id: "run-1",
        tool: "file.write",
        args_digest: "sha256:digest-auth",
        label: "Writing src/auth.ts",
      }),
    ];

    await transport.push({
      kind: "history",
      session_id: "session-1",
      through: 4,
      events: historyEvents,
    });

    expect(screen.getAllByText("Implement authentication").length).toBeGreaterThanOrEqual(1);
    expect(screen.getByText("Analyzing requirements. Creating auth service.")).toBeTruthy();
    expect(screen.getByText("Writing src/auth.ts")).toBeTruthy();

    // 6. Live Overlap Dedup: push sequence 4 again (overlap), followed by live sequence 5
    await transport.push({
      kind: "event",
      session_id: "session-1",
      event: historyEvents[3], // sequence 4 duplicate
    });

    // Transcript should still have exactly 1 occurrence of Writing src/auth.ts
    expect(screen.getAllByText("Writing src/auth.ts")).toHaveLength(1);

    // Live sequence 5 arrives
    await transport.push({
      kind: "event",
      session_id: "session-1",
      event: makeEvent(5, {
        type: "ToolCompleted",
        run_id: "run-1",
        tool: "file.write",
        outcome: { type: "Succeeded" },
      }),
    });

    // 7. Start / Cancel: an active run can be cancelled, but the click only
    //    REQUESTS it. Cancellation is destructive and irreversible, so the
    //    command is sent from the confirmation and nowhere else
    //    (`ConfirmCancel.tsx`, porting `Overlay::ConfirmCancel`).
    expect(screen.getByRole("button", { name: "Cancel Run" })).toBeTruthy();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Cancel Run" }));
    });
    expect(transport.cancelRunCalls).toEqual([]);

    const confirmCancel = screen.getByTestId("cancel-confirm-yes");
    await act(async () => {
      fireEvent.click(confirmCancel);
    });
    expect(transport.cancelRunCalls).toContain("run-1");

    // Daemon emits RunCompleted with Cancelled disposition
    await transport.push({
      kind: "event",
      session_id: "session-1",
      event: makeEvent(6, {
        type: "RunCompleted",
        run_id: "run-1",
        disposition: { type: "Cancelled", reason: "User cancelled the run" },
        chronicle: {
          id: "art-chronicle-1",
          sha256: "0000000000000000000000000000000000000000000000000000000000000000",
          byte_length: 10,
          media_type: "application/json",
          sensitivity: { type: "Public" },
        },
      }),
    });

    expect(screen.getByText("Run cancelled: User cancelled the run")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Cancel Run" })).toBeNull();

    // 8. Approval: daemon requests approval, user approves, daemon resolves
    await transport.push({
      kind: "event",
      session_id: "session-1",
      event: makeEvent(7, {
        type: "ApprovalRequested",
        approval_id: "app-auth-1",
        action: {
          type: "ExecuteCommand",
          program: "git",
          args: ["push", "origin", "main"],
          environment: [],
          cwd: null,
        },
        risk: { level: { type: "Low" }, reasons: [] },
      }),
    });

    expect(screen.getByText("ExecuteCommand: git push origin main")).toBeTruthy();
    const approveBtn = screen.getByRole("button", { name: "Approve" });
    await act(async () => {
      fireEvent.click(approveBtn);
    });

    expect(transport.resolveApprovalCalls).toEqual([
      { approvalId: "app-auth-1", decision: "approve" },
    ]);

    await transport.push({
      kind: "event",
      session_id: "session-1",
      event: makeEvent(8, {
        type: "ApprovalResolved",
        approval_id: "app-auth-1",
        decision: { type: "Approve" },
      }),
    });

    expect(screen.queryByText("ExecuteCommand: git push origin main")).toBeNull();

    // 9. Question: daemon prompts a question and receives answer
    await transport.push({
      kind: "event",
      session_id: "session-1",
      event: makeEvent(9, {
        type: "QuestionAsked",
        question_id: "q-1",
        run_id: "run-1",
        questions: [
          {
            header: "OAuth Provider",
            question: "Which OAuth provider to configure?",
            options: [{ label: "GitHub" }, { label: "Google" }],
            multiple: false,
            custom: false,
          },
        ],
      }),
    });

    expect(screen.getByText("OAuth Provider: Which OAuth provider to configure?")).toBeTruthy();

    await transport.push({
      kind: "event",
      session_id: "session-1",
      event: makeEvent(10, {
        type: "QuestionResolved",
        question_id: "q-1",
        outcome: { type: "Answered", answers: [["GitHub"]] },
      }),
    });

    expect(screen.getByText("Question answered")).toBeTruthy();

    // 10. Artifact Read: daemon proposes a patch with an artifact reference, client reads it
    await transport.push({
      kind: "event",
      session_id: "session-1",
      event: makeEvent(11, {
        type: "PatchProposed",
        changeset_id: "cs-101",
        run_id: "run-1",
        artifact: {
          id: "art-patch-42",
          byte_length: 80,
          media_type: "text/x-diff",
          sensitivity: { type: "Public" },
          sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        },
      }),
    });

    expect(screen.getByText("Patch proposed: artifact art-patch-42")).toBeTruthy();

    const artifactBytes = await transport.readArtifact({
      id: "art-patch-42",
      byte_length: 0,
      media_type: "text/x-diff",
      sensitivity: { type: "Internal" },
      sha256: "",
    });
    const artifactContent = new TextDecoder().decode(artifactBytes);
    expect(artifactContent).toContain("--- a/src/auth.ts");
    expect(artifactContent).toContain("+export const auth = true;");
  });
});
