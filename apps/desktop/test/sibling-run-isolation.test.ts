// A session can hold several concurrent runs. Every one of these tests pushes an
// event for a run the client is NOT showing and asserts the on-screen run does
// not move. Each fails if its guard in `daemonState.ts` is reverted — that is
// the point of the file: the previous attempt at this fix was covered only
// "indirectly" by a suite that passed identically with the guard deleted.
import { describe, expect, it } from "vitest";

import { initialState, reduce, type DaemonState } from "../src/daemonState.js";

function event(body: Record<string, unknown>, sequence = 1) {
  return {
    type: "frame" as const,
    frame: {
      kind: "event" as const,
      session_id: "session-1",
      event: {
        sequence,
        occurred_at: "2026-01-01T00:00:00Z",
        body: body as never,
      },
    } as never,
  };
}

/** run-1 is live and on screen. */
function watchingRunOne(): DaemonState {
  return { ...initialState, activeSessionId: "session-1", activeRunId: "run-1", isRunning: true, runState: "Running" };
}

describe("a sibling run cannot move the run on screen", () => {
  it("ignores a sibling RunStateChanged", () => {
    const next = reduce(watchingRunOne(), event({ type: "RunStateChanged", run_id: "run-2", state: { type: "Paused" } }));
    expect(next.activeRunId).toBe("run-1");
    expect(next.runState).toBe("Running");
    expect(next.isRunning).toBe(true);
  });

  it("does not let a sibling RunCompleted wipe the live run off the surface", () => {
    const next = reduce(
      watchingRunOne(),
      event({ type: "RunCompleted", run_id: "run-2", disposition: { type: "Succeeded" } }),
    );
    expect(next.activeRunId).toBe("run-1");
    expect(next.runState).toBe("Running");
    expect(next.isRunning).toBe(true);
  });

  it("does not let a sibling RunStarted hijack the surface", () => {
    const next = reduce(
      watchingRunOne(),
      event({ type: "RunStarted", run_id: "run-2", objective: "something else" }),
    );
    expect(next.activeRunId).toBe("run-1");
    expect(next.runState).toBe("Running");
  });
});

describe("a run event is adopted, not half-applied, when nothing is on screen", () => {
  it("never reports a run as live without an id to address it with", () => {
    const idle: DaemonState = { ...initialState, activeSessionId: "session-1" };
    const next = reduce(idle, event({ type: "RunStateChanged", run_id: "run-2", state: { type: "Running" } }));
    expect(next.isRunning).toBe(true);
    // The regression: isRunning true while activeRunId stayed null, leaving the
    // surface claiming a live run it could neither pause nor cancel.
    expect(next.activeRunId).toBe("run-2");
  });
});
