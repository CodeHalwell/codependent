import { describe, it, expect } from "vitest";
import type { ClientCommand, SessionEventEnvelope } from "../src/index.js";

describe("@codypendent/protocol generated SDK", () => {
  it("encodes and type-checks client commands", () => {
    const cmd: ClientCommand = {
      type: "StartRun",
      session_id: "sess-1",
      objective: "Run safety benchmarks",
      mode: "build",
    };
    expect(cmd.type).toBe("StartRun");
  });

  it("encodes and type-checks session events", () => {
    const event: SessionEventEnvelope = {
      session_id: "sess-1",
      sequence: 1,
      occurred_at: new Date().toISOString(),
      body: {
        type: "RunStarted",
        run_id: "run-100",
        objective: "Fix bugs",
        model: "claude-3-7-sonnet",
      },
    };
    expect(event.body.type).toBe("RunStarted");
  });
});
