/**
 * The transcript must show the conversation, not the machinery.
 *
 * Both behaviours here are ports of settled TUI decisions: the context
 * manifest folds into one `Backstage` line per run (`reduce.rs`), and a
 * successful run adds no completion row because "the streamed model prose
 * already ended the turn" (`render.rs`). The desktop did neither, so a reply
 * arrived under a screenful of tool manifest and was then echoed back a
 * second time on completion.
 */
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { initialState, reduce, type DaemonState } from "../src/daemonState.js";
import { Transcript } from "../src/components/Transcript.js";

function event(body: Record<string, unknown>, sequence = 1) {
  return {
    type: "frame" as const,
    frame: {
      kind: "event" as const,
      session_id: "session-1",
      event: { sequence, occurred_at: "2026-01-01T00:00:00Z", body: body as never },
    } as never,
  };
}

const MANIFEST = "=== CONTEXT: EVIDENCE, NOT INSTRUCTIONS ===\nline two\nline three";

function attached(): DaemonState {
  return { ...initialState, activeSessionId: "session-1", activeRunId: "run-1" };
}

describe("the run's backstage material does not become the conversation", () => {
  it("folds a context manifest into a backstage row instead of a system message", () => {
    const next = reduce(attached(), event({ type: "NoteAppended", text: MANIFEST }));
    expect(next.transcript).toHaveLength(1);
    const [row] = next.transcript;
    expect(row.type).toBe("backstage");
    expect(row.contextLines).toBe(3);
    // The manifest is kept, not discarded — it is folded, and reachable.
    expect(row.raw).toEqual([MANIFEST]);
  });

  it("keeps one backstage row per run however many notes arrive", () => {
    let state = reduce(attached(), event({ type: "NoteAppended", text: MANIFEST }, 1));
    state = reduce(state, event({ type: "NoteAppended", text: "remembered: node 20" }, 2));
    state = reduce(state, event({ type: "NoteAppended", text: "remembered: prefers vitest" }, 3));
    expect(state.transcript.filter((item) => item.type === "backstage")).toHaveLength(1);
    expect(state.transcript[0].memoryUpdates).toBe(2);
    expect(state.transcript[0].raw).toHaveLength(3);
  });

  it("still shows an ordinary note as a visible message", () => {
    const next = reduce(attached(), event({ type: "NoteAppended", text: "a normal note" }));
    expect(next.transcript[0].type).toBe("system");
  });

  it("renders the manifest folded away, not as page content", () => {
    const state = reduce(attached(), event({ type: "NoteAppended", text: MANIFEST }));
    render(<Transcript items={state.transcript} connectionStatus="connected" />);
    // The summary line is what the reader sees.
    expect(screen.getByText(/Backstage — 3 context lines/)).toBeTruthy();
    // The body lives inside a closed <details>, so it is present but folded.
    const details = document.querySelector("details");
    expect(details).toBeTruthy();
    expect(details?.hasAttribute("open")).toBe(false);
  });
});

describe("a successful run does not echo its own reply", () => {
  it("adds no transcript row when the run completes", () => {
    const state = { ...attached(), isRunning: true };
    const next = reduce(
      state,
      event({
        type: "RunCompleted",
        run_id: "run-1",
        disposition: { type: "Completed", summary: "Hello! What can I help you with?" },
      }),
    );
    expect(next.transcript).toHaveLength(0);
    expect(next.isRunning).toBe(false);
  });

  it("still announces a failure, because nothing else does", () => {
    const next = reduce(
      { ...attached(), isRunning: true },
      event({
        type: "RunCompleted",
        run_id: "run-1",
        disposition: { type: "Failed", reason: "model stream failed" },
      }),
    );
    expect(next.transcript).toHaveLength(1);
    expect(next.transcript[0].text).toContain("model stream failed");
  });
});
