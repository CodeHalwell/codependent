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

describe("a failure card offers to retry its OWN run", () => {
  it("resubmits the failed sibling's objective, not the one on screen", () => {
    // run-1 is displayed; run-2 starts alongside it and is deliberately not
    // allowed to hijack the surface, so its objective is never staged as the
    // active one. run-1 finishes, the surface adopts run-2, and run-2 fails.
    // Retry must offer run-2's work — offering run-1's would relaunch the
    // wrong objective and pay for it.
    let state = reduce(
      { ...watchingRunOne(), activeObjective: "the displayed objective" },
      event({ type: "RunStarted", run_id: "run-2", objective: "the sibling objective" }, 2),
    );
    expect(state.activeObjective).toBe("the displayed objective");

    state = reduce(
      state,
      event({ type: "RunCompleted", run_id: "run-1", disposition: { type: "Completed" } }, 3),
    );
    // The surface adopts run-2 as the only thing still going.
    state = reduce(
      state,
      event({ type: "RunStateChanged", run_id: "run-2", state: { type: "Running" } }, 4),
    );
    expect(state.activeRunId).toBe("run-2");

    state = reduce(
      state,
      event(
        {
          type: "RunCompleted",
          run_id: "run-2",
          disposition: { type: "Failed", reason: "the provider refused" },
        },
        5,
      ),
    );
    const failure = state.transcript.filter((item) => item.type === "failure").at(-1);
    expect(failure?.objective).toBe("the sibling objective");
  });

  it("forgets a run's objective once it has finished", () => {
    let state = reduce(
      { ...initialState, activeSessionId: "session-1" },
      event({ type: "RunStarted", run_id: "run-1", objective: "ship it" }, 1),
    );
    expect(state.objectivesByRun["run-1"]).toBe("ship it");
    state = reduce(
      state,
      event({ type: "RunCompleted", run_id: "run-1", disposition: { type: "Completed" } }, 2),
    );
    expect(state.objectivesByRun).toEqual({});
  });
});
