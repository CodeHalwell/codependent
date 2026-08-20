/**
 * A hole in the live event stream must be read back, not just noticed.
 *
 * A jump in `sequence` means events this client never received — a lagging
 * subscriber, a frame dropped under load. The reducer detected it, wrote a line
 * to the console and carried on, which left the transcript permanently short by
 * exactly that range with nothing on screen marking where. A short transcript
 * that looks complete is the failure: the operator has no way to tell.
 */
import { describe, expect, it } from "vitest";

import { initialState, reduce } from "../src/daemonState.js";
import type { DaemonState } from "../src/daemonState.js";

function deliver(state: DaemonState, sequence: number): DaemonState {
  return reduce(state, {
    type: "frame",
    frame: {
      kind: "event" as const,
      session_id: "s-1",
      event: {
        sequence,
        occurred_at: "2026-01-01T00:00:00Z",
        body: { type: "RunStarted", run_id: "r-1" } as never,
      },
    } as never,
  });
}

describe("a live stream gap is recorded for repair", () => {
  it("names the exact missing range", () => {
    let state = deliver(initialState, 1);
    expect(state.pendingGap).toBeNull();
    state = deliver(state, 2);
    expect(state.pendingGap).toBeNull();
    // 3, 4 and 5 never arrive.
    state = deliver(state, 6);
    expect(state.pendingGap).toEqual({ after: 2, through: 5 });
  });

  it("widens rather than replaces when a second gap opens first", () => {
    let state = deliver(deliver(initialState, 1), 5);
    expect(state.pendingGap).toEqual({ after: 1, through: 4 });
    // A second hole before the first was repaired: the earlier lower bound must
    // survive, or those events are never asked for again.
    state = deliver(state, 9);
    expect(state.pendingGap).toEqual({ after: 1, through: 8 });
  });

  it("clears only when the repair covered the recorded gap", () => {
    let state = deliver(deliver(initialState, 1), 5);
    expect(state.pendingGap).toEqual({ after: 1, through: 4 });

    // A repair for an older, narrower range must not clear a wider gap.
    state = reduce(state, { type: "gap-repaired", through: 2 });
    expect(state.pendingGap).toEqual({ after: 1, through: 4 });

    state = reduce(state, { type: "gap-repaired", through: 4 });
    expect(state.pendingGap).toBeNull();
  });

  it("does not report a gap for the first event of a session", () => {
    // `lastSequence` starts at zero, so a session whose first delivered event
    // is high is a fresh attach, not a hole.
    const state = deliver(initialState, 4200);
    expect(state.pendingGap).toBeNull();
  });
});
