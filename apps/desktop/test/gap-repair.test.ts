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
import type { Catchup, SessionEvent } from "@codypendent/protocol";

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
    expect(state.pendingGap).toEqual({ sessionId: "s-1", after: 2, through: 5 });
  });

  it("widens rather than replaces when a second gap opens first", () => {
    let state = deliver(deliver(initialState, 1), 5);
    expect(state.pendingGap).toEqual({ sessionId: "s-1", after: 1, through: 4 });
    // A second hole before the first was repaired: the earlier lower bound must
    // survive, or those events are never asked for again.
    state = deliver(state, 9);
    expect(state.pendingGap).toEqual({ sessionId: "s-1", after: 1, through: 8 });
  });

  it("clears only when the repair covered the recorded gap", () => {
    let state = deliver(deliver(initialState, 1), 5);
    expect(state.pendingGap).toEqual({ sessionId: "s-1", after: 1, through: 4 });

    // A repair for an older, narrower range must not clear a wider gap.
    state = reduce(state, { type: "gap-repaired", sessionId: "s-1", through: 2 });
    expect(state.pendingGap).toEqual({ sessionId: "s-1", after: 1, through: 4 });

    state = reduce(state, { type: "gap-repaired", sessionId: "s-1", through: 4 });
    expect(state.pendingGap).toBeNull();
  });

  it("clears a pending gap when another session is committed", () => {
    let state = deliver(deliver(initialState, 1), 5);
    expect(state.pendingGap?.sessionId).toBe("s-1");

    state = reduce(state, { type: "session-selected", sessionId: "s-2" });

    expect(state.activeSessionId).toBe("s-2");
    expect(state.pendingGap).toBeNull();
  });

  it("does not report a gap for the first event of a session", () => {
    // `lastSequence` starts at zero, so a session whose first delivered event
    // is high is a fresh attach, not a hole.
    const state = deliver(initialState, 4200);
    expect(state.pendingGap).toBeNull();
  });

  it("detects a live gap against a snapshot while durable history is still restoring", () => {
    const snapshot: Catchup = {
      type: "Snapshot",
      through: 100,
      projection: {
        session_id: "s-1",
        title: "long session",
        last_sequence: 100,
        closed: false,
      },
    };
    let state = reduce(initialState, {
      type: "frame",
      frame: { kind: "catchup", session_id: "s-1", snapshot },
    });
    expect(state.lastSequence).toBe(100);

    // Live fan-out dropped 101-104 while the paged 1-100 history read was in
    // flight. The snapshot watermark, not the yet-empty retained history, is
    // the continuity baseline.
    state = deliver(state, 105);
    expect(state.pendingGap).toEqual({ sessionId: "s-1", after: 100, through: 104 });

    const events: SessionEvent[] = Array.from({ length: 100 }, (_, index) => ({
      sequence: index + 1,
      actor: { type: "System" },
      occurred_at: "2026-01-01T00:00:00Z",
      body: { type: "ModelStreamDelta", run_id: "r-1", text: `${index + 1}` } as never,
    }));
    state = reduce(state, {
      type: "frame",
      frame: { kind: "history", session_id: "s-1", through: 100, events },
    });

    expect(state.lastSequence).toBe(105);
    expect(state.pendingGap).toEqual({ sessionId: "s-1", after: 100, through: 104 });
  });
});
