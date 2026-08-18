import { beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import * as vscode from "vscode";

import {
  BlockingWorkNotifier,
  FOCUS_SESSION_ACTION,
  blockingWorkOf,
  pendingBlockingWork,
  subscribeBlockingWork,
  type BlockingWorkStream,
  type NotifierHost,
} from "../src/notifications.js";
import type { Catchup, EventBody, SessionEvent } from "@codypendent/protocol";

// The notifier is the extension's only notification surface, so the test drives
// the real module against a mocked editor API: what the user is shown, and what
// their click actually sends, are assertions here rather than claims.
vi.mock("vscode", () => {
  const showWarningMessage = vi.fn(() => Promise.resolve(undefined));
  const showInformationMessage = vi.fn(() => Promise.resolve(undefined));
  return { window: { showWarningMessage, showInformationMessage } };
});

const SESSION = "11111111-1111-7111-8111-111111111111";
const APPROVAL = "22222222-2222-7222-8222-222222222222";
const QUESTION = "33333333-3333-7333-8333-333333333333";
const RUN = "44444444-4444-7444-8444-444444444444";

const warn = vscode.window.showWarningMessage as unknown as Mock;
const info = vscode.window.showInformationMessage as unknown as Mock;

function event(sequence: number, body: EventBody): SessionEvent {
  return {
    actor: { type: "Agent", agent_id: "agent", model: "m", run_id: RUN },
    body,
    occurred_at: "2026-01-01T00:00:00Z",
    sequence,
  };
}

const approvalRequested: EventBody = {
  type: "ApprovalRequested",
  approval_id: APPROVAL,
  action: { type: "ExecuteCommand", program: "rm", args: ["-rf", "build"], cwd: "/repo", environment: [] },
  risk: { level: { type: "High" }, reasons: ["destructive"] },
} as unknown as EventBody;

const approvalResolved: EventBody = {
  type: "ApprovalResolved",
  approval_id: APPROVAL,
  decision: { type: "Approve" },
} as unknown as EventBody;

const questionAsked: EventBody = {
  type: "QuestionAsked",
  question_id: QUESTION,
  run_id: RUN,
  questions: [{ header: "Schema", question: "Add the column?", options: [], multiple: false, custom: true }],
} as unknown as EventBody;

function makeHost(): NotifierHost & {
  decisions: Array<{ sessionId: string; approvalId: string; decision: string }>;
  focused: string[];
} {
  const decisions: Array<{ sessionId: string; approvalId: string; decision: string }> = [];
  const focused: string[] = [];
  return {
    decisions,
    focused,
    resolveApproval: (sessionId, approvalId, decision) => decisions.push({ sessionId, approvalId, decision }),
    focusSession: (sessionId) => focused.push(sessionId),
  };
}

/** Make the next notification resolve as if the user clicked `choice`. */
function userClicks(choice: string | undefined): void {
  warn.mockImplementationOnce(() => Promise.resolve(choice));
}

describe("blocking-work notifications", () => {
  beforeEach(() => {
    warn.mockReset();
    info.mockReset();
    warn.mockImplementation(() => Promise.resolve(undefined));
    info.mockImplementation(() => Promise.resolve(undefined));
  });

  it("raises a notification for a live approval and relays the decision", async () => {
    const host = makeHost();
    const notifier = new BlockingWorkNotifier(host);
    userClicks("Approve");

    notifier.observeLiveEvent(event(1, approvalRequested), SESSION);
    await Promise.resolve();
    await Promise.resolve();

    expect(warn).toHaveBeenCalledTimes(1);
    const [message, , ...actions] = warn.mock.calls[0] as [string, unknown, ...string[]];
    expect(message).toContain("Approval required");
    expect(message).toContain("rm -rf build");
    expect(message).toContain("High");
    expect(actions).toEqual(["Approve", "Reject", FOCUS_SESSION_ACTION]);
    expect(host.decisions).toEqual([{ sessionId: SESSION, approvalId: APPROVAL, decision: "Approve" }]);
  });

  it("focuses the parked session when the user picks the focus action", async () => {
    const host = makeHost();
    const notifier = new BlockingWorkNotifier(host);
    userClicks(FOCUS_SESSION_ACTION);

    notifier.observeLiveEvent(event(1, questionAsked), SESSION);
    await Promise.resolve();
    await Promise.resolve();

    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain("Add the column?");
    expect(host.focused).toEqual([SESSION]);
    // A question cannot be answered from a toast, so the only action offered
    // is the one that takes the user to where it can be.
    expect((warn.mock.calls[0] as unknown[]).slice(2)).toEqual([FOCUS_SESSION_ACTION]);
  });

  it("stays silent for every event that does not block a human", () => {
    const notifier = new BlockingWorkNotifier(makeHost());
    const noisy: EventBody[] = [
      { type: "ToolStarted", run_id: RUN, tool: "bash" },
      { type: "ToolCompleted", run_id: RUN, tool: "bash", outcome: { type: "Succeeded" } },
      { type: "BudgetWarning", run_id: RUN, dimension: { type: "Tokens" }, used: 9, limit: 10 },
      { type: "RunCompleted", run_id: RUN, disposition: { type: "Completed" }, chronicle: { id: "a", sha256: "b", media_type: "application/json", byte_length: 1, sensitivity: { type: "Public" } } },
      { type: "ModelStreamDelta", run_id: RUN, text: "hello" },
      { type: "NoteAppended", text: "note" },
    ] as unknown as EventBody[];

    noisy.forEach((body, index) => notifier.observeLiveEvent(event(index + 1, body), SESSION));

    expect(warn).not.toHaveBeenCalled();
    expect(noisy.map((body) => blockingWorkOf(event(1, body), SESSION))).toEqual(noisy.map(() => null));
  });

  it("never announces a replayed request the daemon already resolved", () => {
    const notifier = new BlockingWorkNotifier(makeHost());

    notifier.observeReplay(
      [event(1, approvalRequested), event(2, approvalResolved)],
      SESSION,
    );

    expect(pendingBlockingWork([event(1, approvalRequested), event(2, approvalResolved)], SESSION)).toEqual([]);
    expect(warn).not.toHaveBeenCalled();
  });

  it("announces the still-pending item in a replayed range exactly once", () => {
    const notifier = new BlockingWorkNotifier(makeHost());

    notifier.observeReplay([event(1, approvalRequested), event(2, questionAsked), event(3, approvalResolved)], SESSION);
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain("Add the column?");

    // A reconnect replays the same range: the user is not told twice.
    notifier.observeReplay([event(1, approvalRequested), event(2, questionAsked), event(3, approvalResolved)], SESSION);
    expect(warn).toHaveBeenCalledTimes(1);
  });

  it("coalesces a backlog into one notification instead of a toast per item", async () => {
    const host = makeHost();
    const notifier = new BlockingWorkNotifier(host);
    userClicks(FOCUS_SESSION_ACTION);

    notifier.observePendingApprovals(
      [
        { approval_id: APPROVAL, summary: "run rm" },
        { approval_id: "55555555-5555-7555-8555-555555555555", summary: "git push" },
        { approval_id: "66666666-6666-7666-8666-666666666666", summary: "network request" },
      ],
      SESSION,
    );
    await Promise.resolve();
    await Promise.resolve();

    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain("3 items are waiting for you");
    expect(host.focused).toEqual([SESSION]);
  });

  it("refuses to send a decision for an item it has seen resolved elsewhere", async () => {
    const host = makeHost();
    const notifier = new BlockingWorkNotifier(host);
    let click: (choice: string | undefined) => void = () => undefined;
    warn.mockImplementationOnce(() => new Promise<string | undefined>((resolve) => (click = resolve)));

    notifier.observeLiveEvent(event(1, approvalRequested), SESSION);
    // The TUI resolves it while the toast is still on screen.
    notifier.observeLiveEvent(event(2, approvalResolved), SESSION);
    click("Approve");
    await Promise.resolve();
    await Promise.resolve();

    expect(host.decisions).toEqual([]);
    expect(String(info.mock.calls[0]?.[0])).toContain("already resolved");
  });

  it("does not re-announce a live approval that was already announced", () => {
    const notifier = new BlockingWorkNotifier(makeHost());

    notifier.observeLiveEvent(event(1, approvalRequested), SESSION);
    notifier.observeLiveEvent(event(1, approvalRequested), SESSION);
    notifier.observeReplay([event(1, approvalRequested)], SESSION);

    expect(warn).toHaveBeenCalledTimes(1);
  });
});

/**
 * A stand-in for the connected `DaemonClient`: the same two listener
 * registrations `subscribeBlockingWork` makes on the real one, so this drives
 * the production subscription rather than the notifier's methods directly.
 */
class StubStream implements BlockingWorkStream {
  private events: Array<(event: SessionEvent) => void> = [];
  private catchups: Array<(catchup: Catchup) => void> = [];

  on(name: "event", listener: (event: SessionEvent) => void): unknown;
  on(name: "catchup", listener: (catchup: Catchup) => void): unknown;
  on(name: "event" | "catchup", listener: ((event: SessionEvent) => void) & ((catchup: Catchup) => void)): unknown {
    if (name === "event") this.events.push(listener);
    else this.catchups.push(listener);
    return this;
  }

  emitEvent(event: SessionEvent): void {
    for (const listener of this.events) listener(event);
  }

  emitCatchup(catchup: Catchup): void {
    for (const listener of this.catchups) listener(catchup);
  }
}

describe("subscribeBlockingWork (the wiring the extension uses)", () => {
  beforeEach(() => {
    warn.mockReset();
    info.mockReset();
    warn.mockImplementation(() => Promise.resolve(undefined));
    info.mockImplementation(() => Promise.resolve(undefined));
  });

  it("turns a daemon event stream into exactly one notification for a pending approval", () => {
    const stream = new StubStream();
    subscribeBlockingWork(stream, SESSION, new BlockingWorkNotifier(makeHost()), () => "unused");

    stream.emitEvent(event(1, { type: "ToolStarted", run_id: RUN, tool: "bash" } as unknown as EventBody));
    stream.emitEvent(event(2, approvalRequested));

    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain("Approval required");
  });

  it("announces a compacted snapshot's pending approvals with the host's own renderer", () => {
    const stream = new StubStream();
    subscribeBlockingWork(
      stream,
      SESSION,
      new BlockingWorkNotifier(makeHost()),
      (approval) => `rendered ${approval.approval_id}`,
    );

    stream.emitCatchup({
      type: "Snapshot",
      through: 9,
      projection: {
        session_id: SESSION,
        title: "session",
        last_sequence: 9,
        closed: false,
        pending_approvals: [
          { approval_id: APPROVAL, run_id: RUN, action: { type: "ReadFiles", paths: [] }, risk: { level: { type: "Low" } } },
        ],
      },
    } as unknown as Catchup);

    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain(`rendered ${APPROVAL}`);
  });

  it("says nothing when the catch-up range resolved everything it requested", () => {
    const stream = new StubStream();
    subscribeBlockingWork(stream, SESSION, new BlockingWorkNotifier(makeHost()), () => "unused");

    stream.emitCatchup({
      type: "Events",
      from: 1,
      through: 2,
      events: [event(1, approvalRequested), event(2, approvalResolved)],
    } as unknown as Catchup);

    expect(warn).not.toHaveBeenCalled();
  });
});
