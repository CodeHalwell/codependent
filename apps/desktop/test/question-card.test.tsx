/**
 * A parked question can be answered from the desktop.
 *
 * `QuestionAsked` used to render as a title and a sentence — no options, no
 * input, no buttons — while `ResolveQuestion` went unused and the run stayed
 * blocked until somebody opened the TUI. The card now sends the protocol's
 * own outcome: chosen labels per question, a typed answer carried verbatim,
 * or a rejection with optional feedback.
 */
import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { App } from "../src/App.js";
import type { QuestionOutcomeView } from "../src/types.js";
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

class QuestionStub implements DesktopTransport {
  readonly resolved: Array<{ questionId: string; outcome: QuestionOutcomeView }> = [];
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

  resolveQuestion(questionId: string, outcome: QuestionOutcomeView): Promise<void> {
    this.resolved.push({ questionId, outcome });
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

async function askQuestion(stub: QuestionStub, multiple = false) {
  render(<App makeTransport={() => stub} />);
  await act(async () => undefined);
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Set up auth" } });
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
  });
  await stub.emit(1, { type: "RunStarted", run_id: "run-1", objective: "Set up auth", mode: { type: "Build" } });
  await stub.emit(2, {
    type: "QuestionAsked",
    question_id: "q-1",
    run_id: "run-1",
    questions: [
      {
        header: "OAuth provider",
        question: "Which OAuth provider should be configured?",
        options: [
          { label: "GitHub", description: "the repository host" },
          { label: "Google" },
        ],
        multiple,
        custom: true,
      },
    ],
  });
}

describe("answering a parked question", () => {
  it("sends the chosen option as the protocol's Answered outcome", async () => {
    const stub = new QuestionStub();
    await askQuestion(stub);
    expect(screen.getByTestId("run-working").textContent).toContain("waiting for your answer");
    const answer = screen.getByRole("button", { name: "Answer" }) as HTMLButtonElement;
    // Nothing picked yet: the button waits.
    expect(answer.disabled).toBe(true);
    fireEvent.click(screen.getByLabelText(/GitHub/));
    expect(answer.disabled).toBe(false);
    await act(async () => {
      fireEvent.click(answer);
    });
    expect(stub.resolved).toEqual([
      { questionId: "q-1", outcome: { type: "Answered", answers: [["GitHub"]] } },
    ]);
  });

  it("lets a typed answer replace the radio pick of a single-select", async () => {
    // One question, one answer. Appending would show the agent the suggestion
    // the person rejected beside the one they meant.
    const stub = new QuestionStub();
    await askQuestion(stub);
    fireEvent.click(screen.getByLabelText(/GitHub/));
    fireEvent.change(screen.getByLabelText("Your own answer to OAuth provider"), {
      target: { value: "Okta" },
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Answer" }));
    });
    expect(stub.resolved[0].outcome).toEqual({
      type: "Answered",
      answers: [["Okta"]],
    });
  });

  it("starts fresh when the agent reissues the question with more prompts", async () => {
    // `daemonState` replaces a reissued card in place under the same
    // transcript key, so this component stays mounted and its state
    // initializers do not run again. Before the reset, the added prompt had no
    // draft: its input could not update and the answer went out short.
    const stub = new QuestionStub();
    await askQuestion(stub);
    fireEvent.click(screen.getByLabelText(/GitHub/));

    await stub.emit(3, {
      type: "QuestionAsked",
      question_id: "q-1",
      run_id: "run-1",
      questions: [
        {
          header: "OAuth provider",
          question: "Which OAuth provider should be configured?",
          options: [{ label: "GitHub" }, { label: "Google" }],
          multiple: false,
          custom: true,
        },
        {
          header: "Environment",
          question: "Which environment?",
          options: [{ label: "staging" }, { label: "production" }],
          multiple: false,
          custom: true,
        },
      ],
    });

    // The earlier pick is gone, and the new prompt is answerable.
    const answer = screen.getByRole("button", { name: "Answer" }) as HTMLButtonElement;
    expect(answer.disabled).toBe(true);
    fireEvent.click(screen.getByLabelText(/Google/));
    fireEvent.click(screen.getByLabelText(/staging/));
    expect(answer.disabled).toBe(false);
    await act(async () => {
      fireEvent.click(answer);
    });
    expect(stub.resolved[0]).toEqual({
      questionId: "q-1",
      outcome: { type: "Answered", answers: [["Google"], ["staging"]] },
    });
  });

  it("carries a typed answer verbatim, alongside the picks of a multi-select", async () => {
    const stub = new QuestionStub();
    await askQuestion(stub, true);
    fireEvent.click(screen.getByLabelText(/GitHub/));
    fireEvent.click(screen.getByLabelText(/Google/));
    fireEvent.change(screen.getByLabelText("Your own answer to OAuth provider"), {
      target: { value: "Okta as well" },
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Answer" }));
    });
    expect(stub.resolved[0].outcome).toEqual({
      type: "Answered",
      answers: [["GitHub", "Google", "Okta as well"]],
    });
  });

  it("rejects with feedback, and the card leaves once the daemon resolves it", async () => {
    const stub = new QuestionStub();
    await askQuestion(stub);
    fireEvent.click(screen.getByRole("button", { name: "Reject" }));
    fireEvent.change(screen.getByLabelText(/Why you are rejecting/), {
      target: { value: "Ask me after the tests pass" },
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Send rejection" }));
    });
    expect(stub.resolved).toEqual([
      { questionId: "q-1", outcome: { type: "Rejected", feedback: "Ask me after the tests pass" } },
    ]);
    await stub.emit(3, {
      type: "QuestionResolved",
      question_id: "q-1",
      outcome: { type: "Rejected", feedback: "Ask me after the tests pass" },
    });
    expect(screen.queryByTestId("question-card")).toBeNull();
    expect(screen.getByText("Question rejected: Ask me after the tests pass")).toBeTruthy();
  });
});
