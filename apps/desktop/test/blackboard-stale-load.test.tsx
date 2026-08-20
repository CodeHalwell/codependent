/**
 * A blackboard read that has been superseded must not land.
 *
 * Reads are not cancellable, so switching runs quickly leaves two in flight and
 * whichever the daemon answers last wins. When that was the older one, the
 * previous run's board appeared under the newer run's selection — and because
 * the panel's live subscription keys off the board's own run id, it then
 * followed posts for the run the operator had just navigated away from.
 */
import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { BlackboardView } from "../src/components/BlackboardView.js";
import type { DesktopTransport } from "../src/transport.js";

/** A transport whose reads resolve only when the test releases them. */
function deferredTransport() {
  const gates = new Map<string, (items: unknown[]) => void>();
  const transport = {
    readBlackboard: (runId: string) =>
      new Promise((resolve) => {
        gates.set(runId, resolve as (items: unknown[]) => void);
      }),
  } as unknown as DesktopTransport;
  return { transport, gates };
}

function item(runId: string, id: string) {
  return {
    id,
    workflow_run_id: runId,
    kind: "finding",
    author: "agent",
    payload: {},
    evidence: [],
    superseded: false,
  };
}

describe("the blackboard ignores a superseded read", () => {
  it("keeps the newer run's board when an older read answers last", async () => {
    const { transport, gates } = deferredTransport();
    const view = render(<BlackboardView transport={transport} workflowRunId="run-old" />);
    await waitFor(() => expect(gates.has("run-old")).toBe(true));

    // Navigate to a second run before the first read has answered.
    view.rerender(<BlackboardView transport={transport} workflowRunId="run-new" />);
    await waitFor(() => expect(gates.has("run-new")).toBe(true));

    // The newer read answers first, then the older one — the out-of-order case.
    gates.get("run-new")!([item("run-new", "item-new")]);
    await waitFor(() => expect(screen.queryByTestId("blackboard-item-item-new")).not.toBeNull());
    gates.get("run-old")!([item("run-old", "item-old")]);

    // Give the superseded resolution every chance to overwrite.
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(screen.queryByTestId("blackboard-item-item-old")).toBeNull();
    expect(screen.queryByTestId("blackboard-item-item-new")).not.toBeNull();
  });
});
