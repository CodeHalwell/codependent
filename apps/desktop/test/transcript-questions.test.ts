/**
 * A question card is keyed by the daemon's question id so `QuestionResolved`
 * can retire exactly it — which is also why a re-issue must REPLACE rather than
 * append: two cards under one React key stack, and reconciliation between them
 * is undefined. And a resolved question must report what actually happened; the
 * outcome was discarded, so a rejected question read "Question answered".
 */
import { describe, expect, it } from "vitest";
import { initialState, reduce, type DaemonState } from "../src/daemonState.js";

function event(body: Record<string, unknown>, sequence = 1) {
  return {
    type: "frame" as const,
    frame: {
      kind: "event" as const,
      session_id: "s",
      event: { sequence, occurred_at: "2026-01-01T00:00:00Z", body: body as never },
    } as never,
  };
}

function attached(): DaemonState {
  return { ...initialState, activeSessionId: "s", activeRunId: "run-1" };
}

const QUESTION_ID = "01a0-question";

describe("question cards", () => {
  it("replaces a re-issued question instead of stacking a duplicate key", () => {
    let state = reduce(
      attached(),
      event({
        type: "QuestionAsked",
        question_id: QUESTION_ID,
        questions: [{ header: "Pick", question: "Which one?" }],
      }, 1),
    );
    state = reduce(
      state,
      event({
        type: "QuestionAsked",
        question_id: QUESTION_ID,
        questions: [{ header: "Pick", question: "Which one, really?" }],
      }, 2),
    );

    const cards = state.transcript.filter((item) => item.type === "question");
    expect(cards).toHaveLength(1);
    expect(cards[0].text).toContain("Which one, really?");
    const ids = state.transcript.map((item) => item.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("reports a rejected question as rejected, not answered", () => {
    let state = reduce(
      attached(),
      event({ type: "QuestionAsked", question_id: QUESTION_ID, questions: [{ header: "P", question: "Q" }] }, 1),
    );
    state = reduce(
      state,
      event({
        type: "QuestionResolved",
        question_id: QUESTION_ID,
        outcome: { type: "Rejected", feedback: "not now" },
      }, 2),
    );
    const note = state.transcript.find((item) => item.type === "system");
    expect(note?.text).toContain("rejected");
    expect(note?.text).toContain("not now");
    expect(state.transcript.some((item) => item.type === "question")).toBe(false);
  });

  it("still reports an answered question as answered", () => {
    let state = reduce(
      attached(),
      event({ type: "QuestionAsked", question_id: QUESTION_ID, questions: [{ header: "P", question: "Q" }] }, 1),
    );
    state = reduce(
      state,
      event({ type: "QuestionResolved", question_id: QUESTION_ID, outcome: { type: "Answered" } }, 2),
    );
    expect(state.transcript.find((item) => item.type === "system")?.text).toBe("Question answered");
  });
});

describe("the backstage fold is per run", () => {
  it("does not fold a second run's manifest into the first run's card", () => {
    const manifest = "=== CONTEXT: EVIDENCE ===\nline\nline";
    let state = reduce(attached(), event({ type: "NoteAppended", text: manifest, run_id: "run-1" }, 1));
    state = reduce(state, event({ type: "NoteAppended", text: manifest, run_id: "run-2" }, 2));

    const rows = state.transcript.filter((item) => item.type === "backstage");
    // One row per run, as the comment has always said.
    expect(rows).toHaveLength(2);
    expect(rows[0].raw).toHaveLength(1);
    expect(rows[1].raw).toHaveLength(1);
  });
});
