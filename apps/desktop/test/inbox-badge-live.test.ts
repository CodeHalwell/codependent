/**
 * The inbox badge moves with the events, not only on refresh.
 *
 * `applyEvent` had no inbox case, so a new `ApprovalRequested` or
 * `QuestionAsked` left the sidebar badge stale from connect until a manual
 * Refresh. The count here is a running estimate — the daemon's own
 * `inbox-loaded` still replaces it with the truth whenever the list is read.
 */
import { describe, expect, it } from "vitest";

import { initialState, reduce, type DaemonState } from "../src/daemonState.js";

function event(body: Record<string, unknown>, sequence: number) {
  return {
    type: "frame" as const,
    frame: {
      kind: "event" as const,
      session_id: "session-1",
      event: { sequence, occurred_at: "2026-01-01T00:00:00Z", body: body as never },
    } as never,
  };
}

function attached(): DaemonState {
  return { ...initialState, activeSessionId: "session-1", activeRunId: "run-1" };
}

const question = (id: string) => ({
  type: "QuestionAsked",
  question_id: id,
  run_id: "run-1",
  questions: [
    {
      header: "Approach",
      question: "How should I proceed?",
      options: [{ label: "Retry", description: "" }],
      multiple: false,
      custom: true,
    },
  ],
});

describe("the live inbox badge", () => {
  it("rises on a new approval and falls when it resolves", () => {
    let state = reduce(
      attached(),
      event({ type: "ApprovalRequested", approval_id: "app-1", run_id: "run-1", action: {}, risk: {} }, 1),
    );
    expect(state.unreadInboxCount).toBe(1);

    state = reduce(
      state,
      event({ type: "ApprovalResolved", approval_id: "app-1", run_id: "run-1", decision: {} }, 2),
    );
    expect(state.unreadInboxCount).toBe(0);
  });

  it("rises on a new question, but a RE-ISSUE never double-counts", () => {
    let state = reduce(attached(), event(question("q-1"), 1));
    expect(state.unreadInboxCount).toBe(1);

    // The daemon re-issued the same question: the card replaces in place and
    // the badge must not climb.
    state = reduce(state, event(question("q-1"), 2));
    expect(state.unreadInboxCount).toBe(1);

    state = reduce(
      state,
      event({ type: "QuestionResolved", question_id: "q-1", run_id: "run-1", outcome: {} }, 3),
    );
    expect(state.unreadInboxCount).toBe(0);
  });

  it("never goes negative when a resolve arrives for an unseen request", () => {
    const state = reduce(
      attached(),
      event({ type: "ApprovalResolved", approval_id: "app-9", run_id: "run-1", decision: {} }, 1),
    );
    expect(state.unreadInboxCount).toBe(0);
  });

  it("yields to the daemon's own count when the inbox is read", () => {
    let state = reduce(
      attached(),
      event({ type: "ApprovalRequested", approval_id: "app-1", run_id: "run-1", action: {}, risk: {} }, 1),
    );
    state = reduce(state, { type: "inbox-loaded", entries: [] });
    expect(state.unreadInboxCount).toBe(0);
  });
});
