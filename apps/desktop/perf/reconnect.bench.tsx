// Measurement only: the cost of catch-up replay after a reconnect, which
// arrives as one frame per event and is entirely duplicates.
import { describe, it } from "vitest";
import { initialState, reduce, type DaemonState } from "../src/daemonState.js";

function eventFrame(sequence: number) {
  return {
    type: "frame" as const,
    frame: {
      kind: "event" as const,
      session_id: "s",
      event: {
        sequence,
        occurred_at: "2026-01-01T00:00:00Z",
        body: { type: "ModelStreamDelta", run_id: "run-1", text: `chunk ${sequence} ` } as never,
      },
    } as never,
  };
}

describe("reconnect catch-up replay", () => {
  for (const n of [400, 1500]) {
    it(`history=${n}`, () => {
      // Build the session once.
      let state: DaemonState = { ...initialState, activeSessionId: "s", activeRunId: "run-1" };
      for (let i = 1; i <= n; i++) state = reduce(state, eventFrame(i));

      // Reconnect: the daemon replays every event again, one frame each.
      const t0 = performance.now();
      for (let i = 1; i <= n; i++) state = reduce(state, eventFrame(i));
      const ms = performance.now() - t0;
      console.log(`  history=${String(n).padStart(5)}  replay ${ms.toFixed(0).padStart(6)} ms  (${(ms / n).toFixed(3)} ms/event)`);
    });
  }
});
