import { act, render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { App } from "../src/App.js";
import {
  BlockingWorkNotifier,
  blockingWorkOf,
  pendingBlockingWork,
  type BlockingWorkNotice,
} from "../src/osNotifications.js";
import type {
  AnalyticsExportRequest,
  AnalyticsExportResult,
  AnalyticsPage,
  AnalyticsQuery,
  EventBody,
  InboxEntry,
  InboxListQuery,
  InboxMutation,
  InboxPage,
  SessionEvent,
} from "@codypendent/protocol";
import type {
  ConnectionInfo,
  DaemonFrame,
  DesktopTransport,
  RunHandle,
  SessionRow,
} from "../src/transport.js";

/**
 * These tests drive the app the way the shell does — frames in — and assert on
 * the notification sink, so what a user would be shown at the OS level is an
 * assertion rather than a claim. The sink is the only injected part; the path
 * from frame to notice is the production one in `useDaemon`.
 */

const SESSION = "11111111-1111-7111-8111-111111111111";
const APPROVAL = "22222222-2222-7222-8222-222222222222";
const QUESTION = "33333333-3333-7333-8333-333333333333";
const RUN = "44444444-4444-7444-8444-444444444444";

class StubTransport implements DesktopTransport {
  private frames: ((frame: DaemonFrame) => void) | null = null;

  socketPath(): Promise<string> {
    return Promise.resolve("/tmp/codypendent/daemon.sock");
  }

  connect(onFrame: (frame: DaemonFrame) => void): Promise<ConnectionInfo> {
    this.frames = onFrame;
    return Promise.resolve({
      socket_path: "/tmp/codypendent/daemon.sock",
      protocol_version: "1.4",
      daemon_version: "0.11.0",
      daemon_instance: "instance-1",
      build_id: "build-1",
    });
  }

  disconnect(): Promise<void> {
    return Promise.resolve();
  }

  listSessions(): Promise<SessionRow[]> {
    return Promise.resolve([
      {
        session_id: SESSION,
        title: "Refactor the parser",
        state: "Active",
        created_at: "2026-08-16T10:00:00Z",
        updated_at: "2026-08-16T10:00:00Z",
      } as unknown as SessionRow,
    ]);
  }

  startObjective(): Promise<RunHandle> {
    return Promise.resolve({ session_id: SESSION, run_id: RUN });
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

  listInbox(_query?: InboxListQuery): Promise<InboxPage> {
    return Promise.resolve({ items: [] as InboxEntry[], next_cursor: null });
  }

  mutateInbox(_mutation: InboxMutation): Promise<InboxEntry> {
    return Promise.reject(new Error("not used"));
  }

  queryAnalytics(_query?: AnalyticsQuery): Promise<AnalyticsPage> {
    return Promise.resolve({ items: [], next_cursor: null });
  }

  exportAnalytics(_request: AnalyticsExportRequest): Promise<AnalyticsExportResult> {
    return Promise.reject(new Error("not used"));
  }

  async push(frame: DaemonFrame): Promise<void> {
    await act(async () => {
      this.frames?.(frame);
    });
  }
}

function sessionEvent(sequence: number, body: unknown): SessionEvent {
  return {
    sequence,
    actor: { type: "System" },
    occurred_at: "2026-08-16T10:00:00Z",
    body: body as EventBody,
  } as unknown as SessionEvent;
}

function eventFrame(sequence: number, body: unknown): DaemonFrame {
  return { kind: "event", session_id: SESSION, event: sessionEvent(sequence, body) };
}

const approvalRequested = {
  type: "ApprovalRequested",
  approval_id: APPROVAL,
  action: { type: "ExecuteCommand", program: "rm", args: ["-rf", "build"], cwd: "/repo", environment: [] },
  risk: { level: { type: "High" }, reasons: ["destructive"] },
};

const approvalResolved = {
  type: "ApprovalResolved",
  approval_id: APPROVAL,
  decision: { type: "Approve" },
};

const questionAsked = {
  type: "QuestionAsked",
  question_id: QUESTION,
  run_id: RUN,
  questions: [{ header: "Schema", question: "Add the column?", options: [], multiple: false, custom: true }],
};

async function renderApp(): Promise<{ transport: StubTransport; notices: BlockingWorkNotice[] }> {
  const transport = new StubTransport();
  const notices: BlockingWorkNotice[] = [];
  render(<App makeTransport={() => transport as DesktopTransport} notify={(notice) => notices.push(notice)} />);
  await act(async () => undefined);
  return { transport, notices };
}

describe("desktop OS notifications", () => {
  it("raises one notification when the daemon asks for an approval", async () => {
    const { transport, notices } = await renderApp();

    await transport.push(eventFrame(1, approvalRequested));

    expect(notices).toHaveLength(1);
    expect(notices[0]?.kind).toBe("approval");
    expect(notices[0]?.id).toBe(APPROVAL);
    expect(notices[0]?.sessionId).toBe(SESSION);
    expect(notices[0]?.title).toBe("Approval needed");
    expect(notices[0]?.body).toContain("rm -rf build");
    expect(notices[0]?.body).toContain("High");
  });

  it("raises one notification when the run parks on a question", async () => {
    const { transport, notices } = await renderApp();

    await transport.push(eventFrame(1, questionAsked));

    expect(notices).toHaveLength(1);
    expect(notices[0]?.kind).toBe("question");
    expect(notices[0]?.body).toContain("Add the column?");
  });

  it("stays silent for every event that does not block a human", async () => {
    const { transport, notices } = await renderApp();

    await transport.push(eventFrame(1, { type: "RunStarted", run_id: RUN, objective: "do it" }));
    await transport.push(eventFrame(2, { type: "ModelStreamDelta", run_id: RUN, text: "thinking" }));
    await transport.push(eventFrame(3, { type: "ToolStarted", run_id: RUN, tool: "bash" }));
    await transport.push(eventFrame(4, { type: "ToolCompleted", run_id: RUN, tool: "bash", outcome: { type: "Succeeded" } }));
    await transport.push(eventFrame(5, { type: "BudgetWarning", run_id: RUN, dimension: { type: "Tokens" }, used: 9, limit: 10 }));
    await transport.push(eventFrame(6, { type: "RunCompleted", run_id: RUN, disposition: { type: "Completed" }, chronicle: { id: "a", sha256: "b", media_type: "application/json", byte_length: 1, sensitivity: { type: "Public" } } }));

    expect(notices).toEqual([]);
  });

  it("never announces the same approval twice", async () => {
    const { transport, notices } = await renderApp();

    await transport.push(eventFrame(1, approvalRequested));
    await transport.push(eventFrame(1, approvalRequested));
    await transport.push({
      kind: "history",
      session_id: SESSION,
      through: 1,
      events: [sessionEvent(1, approvalRequested)],
    });

    expect(notices).toHaveLength(1);
  });

  it("says nothing about a replayed request the daemon already resolved", async () => {
    const { transport, notices } = await renderApp();

    await transport.push({
      kind: "history",
      session_id: SESSION,
      through: 2,
      events: [sessionEvent(1, approvalRequested), sessionEvent(2, approvalResolved)],
    });

    expect(notices).toEqual([]);
  });

  it("announces a compacted snapshot's pending approvals, and nothing about questions it cannot see", async () => {
    const { transport, notices } = await renderApp();

    await transport.push({
      kind: "catchup",
      session_id: SESSION,
      snapshot: {
        type: "Snapshot",
        through: 12,
        projection: {
          session_id: SESSION,
          title: "Refactor the parser",
          last_sequence: 12,
          closed: false,
          pending_approvals: [
            {
              approval_id: APPROVAL,
              run_id: RUN,
              action: { type: "GitPush", remote: "origin", branch: "main" },
              risk: { level: { type: "Medium" } },
            },
          ],
        },
      },
    } as unknown as DaemonFrame);

    expect(notices).toHaveLength(1);
    expect(notices[0]?.id).toBe(APPROVAL);
    expect(notices.filter((notice) => notice.kind === "question")).toEqual([]);
  });

  it("coalesces a backlog into a single notification", async () => {
    const { transport, notices } = await renderApp();

    await transport.push({
      kind: "history",
      session_id: SESSION,
      through: 3,
      events: [
        sessionEvent(1, approvalRequested),
        sessionEvent(2, questionAsked),
        sessionEvent(3, { ...approvalRequested, approval_id: "55555555-5555-7555-8555-555555555555" }),
      ],
    });

    expect(notices).toHaveLength(1);
    expect(notices[0]?.title).toBe("3 items need you");
  });

  it("announces nothing on a dropped connection", async () => {
    const { transport, notices } = await renderApp();

    await transport.push({ kind: "disconnected", reason: "socket closed" });

    expect(notices).toEqual([]);
  });
});

describe("blocking-work fold", () => {
  it("classifies only approvals and questions as blocking", () => {
    expect(blockingWorkOf(sessionEvent(1, approvalRequested), SESSION)?.kind).toBe("approval");
    expect(blockingWorkOf(sessionEvent(2, questionAsked), SESSION)?.kind).toBe("question");
    expect(blockingWorkOf(sessionEvent(3, { type: "SomethingNewerDaemonsSend" }), SESSION)).toBeNull();
  });

  it("drops a request whose resolution is in the same range", () => {
    expect(
      pendingBlockingWork([sessionEvent(1, approvalRequested), sessionEvent(2, approvalResolved)], SESSION),
    ).toEqual([]);
  });

  it("keeps a request whose resolution is not in the range", () => {
    const pending = pendingBlockingWork([sessionEvent(1, approvalRequested)], SESSION);
    expect(pending.map((item) => item.id)).toEqual([APPROVAL]);
  });

  it("does not announce work for a frame that carries no session", () => {
    const notices: BlockingWorkNotice[] = [];
    new BlockingWorkNotifier((notice) => notices.push(notice)).observeFrame({
      kind: "event",
      session_id: null,
      event: sessionEvent(1, approvalRequested),
    });
    // It is still blocking work — the session is simply unknown, and reported
    // as unknown rather than guessed.
    expect(notices).toHaveLength(1);
    expect(notices[0]?.sessionId).toBeNull();
  });
});
