/**
 * What the wire says about an outcome reaches the screen.
 *
 * Two silent drops, held here: a failed tool call carried its reason in
 * `ToolOutcome::Failed { message }` and the client discarded it (a red
 * "error" with no cause anywhere in the app); and `PatchProposed` carried the
 * touched files, ± line counts, and a bounded diff preview, of which the
 * client printed only the artifact id.
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

describe("tool outcomes", () => {
  it("keeps a failure's reason on the call it failed", () => {
    let state = reduce(
      attached(),
      event({ type: "ToolStarted", run_id: "run-1", tool: "shell.run", args_digest: "d", label: "cargo test" }, 1),
    );
    state = reduce(
      state,
      event(
        {
          type: "ToolCompleted",
          run_id: "run-1",
          tool: "shell.run",
          outcome: { type: "Failed", message: "exit status 101" },
        },
        2,
      ),
    );
    const call = state.transcript.find((item) => item.type === "tool_call");
    expect(call?.status).toBe("error");
    expect(call?.toolResult).toBe("exit status 101");
  });

  it("adds no reason to a success", () => {
    let state = reduce(
      attached(),
      event({ type: "ToolStarted", run_id: "run-1", tool: "shell.run", args_digest: "d" }, 1),
    );
    state = reduce(
      state,
      event({ type: "ToolCompleted", run_id: "run-1", tool: "shell.run", outcome: { type: "Succeeded" } }, 2),
    );
    const call = state.transcript.find((item) => item.type === "tool_call");
    expect(call?.status).toBe("success");
    expect(call?.toolResult).toBeUndefined();
  });
});

describe("proposed patches", () => {
  it("keeps the files, counts, and diff preview the wire carried", () => {
    const state = reduce(
      attached(),
      event(
        {
          type: "PatchProposed",
          run_id: "run-1",
          changeset_id: "cs-1",
          artifact: { id: "art-1", media_type: "text/x-diff", byte_length: 100, sha256: "x", sensitivity: { type: "Internal" } },
          files: ["src/lib.rs", "src/main.rs"],
          additions: 12,
          deletions: 3,
          // The field name the PROTOCOL uses (`EventBody::PatchProposed`).
          // This test previously sent `diff_preview`, which the client also
          // read — so both sides agreed on a name the daemon never sends, and
          // the card's diff was empty against a real daemon while the test
          // passed.
          preview: "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@",
        },
        1,
      ),
    );
    const patch = state.transcript[state.transcript.length - 1];
    expect(patch.text).toBe("Patch proposed: 2 files (+12 −3)");
    expect(patch.patchFiles).toEqual(["src/lib.rs", "src/main.rs"]);
    expect(patch.diffPreview).toContain("+++ b/src/lib.rs");
    expect(patch.artifactId).toBe("art-1");
  });

  it("reads the preview off the protocol's field and no other", () => {
    // Pins the name: a body carrying only the old invented key leaves the
    // preview absent rather than quietly working.
    const state = reduce(
      attached(),
      event(
        {
          type: "PatchProposed",
          run_id: "run-1",
          changeset_id: "cs-2",
          artifact: {
            id: "art-3",
            media_type: "text/x-diff",
            byte_length: 10,
            sha256: "x",
            sensitivity: { type: "Internal" },
          },
          files: ["src/lib.rs"],
          diff_preview: "a preview under a name the daemon does not send",
        },
        1,
      ),
    );
    expect(state.transcript[state.transcript.length - 1].diffPreview).toBeUndefined();
  });

  it("falls back to the artifact id when the wire carried no file list", () => {
    const state = reduce(
      attached(),
      event(
        {
          type: "PatchProposed",
          run_id: "run-1",
          changeset_id: "cs-1",
          artifact: { id: "art-2", media_type: "text/x-diff", byte_length: 1, sha256: "x", sensitivity: { type: "Internal" } },
        },
        1,
      ),
    );
    const patch = state.transcript[state.transcript.length - 1];
    expect(patch.text).toBe("Patch proposed: artifact art-2");
  });
});
