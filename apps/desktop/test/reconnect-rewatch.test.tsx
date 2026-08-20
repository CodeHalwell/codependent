/**
 * A live watch must survive a reconnect.
 *
 * A subscription belongs to the connection that grew it, and a reconnect builds
 * a new client — so after one, the daemon is streaming this run to nobody. The
 * panels showed no sign of it: the workflow graph sat at its last transition
 * and the board at its last read, both looking merely quiet rather than
 * disconnected. `connectionEpoch` counts established connections, and a panel
 * holding a watch re-establishes it when that number moves.
 */
import { render, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { WorkflowView } from "../src/components/WorkflowView.js";
import { BlackboardView } from "../src/components/BlackboardView.js";
import { initialState, reduce } from "../src/daemonState.js";
import type { DesktopTransport } from "../src/transport.js";

describe("panels re-establish their watch on a new connection", () => {
  it("re-watches a workflow run when the connection epoch moves", async () => {
    const watched: string[] = [];
    const transport = {
      watchWorkflow: async (runId: string) => {
        watched.push(runId);
        return { snapshot: { workflow_run_id: runId, nodes: [], phase: "Running" }, blackboard: [] };
      },
    } as unknown as DesktopTransport;

    const view = render(
      <WorkflowView transport={transport} initialRunId="wf-1" connectionEpoch={1} />,
    );
    await waitFor(() => expect(watched).toEqual(["wf-1"]));

    // Same run, new connection.
    view.rerender(<WorkflowView transport={transport} initialRunId="wf-1" connectionEpoch={2} />);
    await waitFor(() => expect(watched).toEqual(["wf-1", "wf-1"]));
  });

  it("re-reads a board when the connection epoch moves", async () => {
    const reads: string[] = [];
    const transport = {
      readBlackboard: async (runId: string) => {
        reads.push(runId);
        return [];
      },
    } as unknown as DesktopTransport;

    const view = render(
      <BlackboardView transport={transport} workflowRunId="wf-1" connectionEpoch={1} />,
    );
    await waitFor(() => expect(reads).toEqual(["wf-1"]));

    view.rerender(<BlackboardView transport={transport} workflowRunId="wf-1" connectionEpoch={2} />);
    await waitFor(() => expect(reads).toEqual(["wf-1", "wf-1"]));
  });

  it("counts each connection, so a reconnect is always a change", () => {
    const info = { daemon_version: "0.0.0", socket_path: "/tmp/sock" } as never;
    let state = initialState;
    expect(state.connectionEpoch).toBe(0);
    state = reduce(state, { type: "connected", info });
    expect(state.connectionEpoch).toBe(1);
    state = reduce(state, { type: "connect-failed", detail: "socket closed" });
    state = reduce(state, { type: "connected", info });
    expect(state.connectionEpoch).toBe(2);
  });
});
