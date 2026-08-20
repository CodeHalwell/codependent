/**
 * Catch-up after a reconnect arrives as one frame per event, and every one of
 * those is a duplicate of something already folded in. Answering "do I have
 * this already?" used to rebuild a Map of the whole history and re-sort it, so
 * the replay cost grew with the square of the session length — 36ms for a
 * 400-event history on a fast machine, worse the longer you had been working,
 * and paid exactly when the operator had finished waiting out the backoff.
 *
 * These tests hold the behaviour the binary search must not change.
 */
import { describe, expect, it } from "vitest";
import { initialState, reduce, type DaemonState } from "../src/daemonState.js";

function frame(sequence: number, text: string) {
  return {
    type: "frame" as const,
    frame: {
      kind: "event" as const,
      session_id: "s",
      event: {
        sequence,
        occurred_at: "2026-01-01T00:00:00Z",
        body: { type: "ModelStreamDelta", run_id: "run-1", text } as never,
      },
    } as never,
  };
}

function attached(): DaemonState {
  return { ...initialState, activeSessionId: "s", activeRunId: "run-1" };
}

describe("reconnect replay", () => {
  it("folds a replayed event exactly once, however many times it arrives", () => {
    let state = reduce(attached(), frame(1, "a"));
    state = reduce(state, frame(2, "b"));
    const settled = state;

    // The daemon replays the whole session after a reconnect.
    state = reduce(state, frame(1, "a"));
    state = reduce(state, frame(2, "b"));

    expect(state).toBe(settled); // identity: a duplicate changes nothing at all
    expect(state.durableEvents).toHaveLength(2);
    expect(state.transcript.map((item) => item.text).join("")).toBe("ab");
  });

  it("still folds a genuinely out-of-order event that was never seen", () => {
    let state = reduce(attached(), frame(1, "a"));
    state = reduce(state, frame(3, "c"));
    // Sequence 2 arrives late — below `lastSequence`, and NOT a duplicate.
    state = reduce(state, frame(2, "b"));

    expect(state.durableEvents.map((event) => event.sequence)).toEqual([1, 2, 3]);
    // Rebuilt in sequence order, not arrival order.
    expect(state.transcript.map((item) => item.text).join("")).toBe("abc");
  });

  it("keeps the retained set ordered so the search stays valid", () => {
    let state = attached();
    for (const seq of [5, 1, 9, 3, 7]) {
      state = reduce(state, frame(seq, `${seq}`));
    }
    const sequences = state.durableEvents.map((event) => event.sequence);
    expect(sequences).toEqual([...sequences].sort((a, b) => a - b));
  });
});
