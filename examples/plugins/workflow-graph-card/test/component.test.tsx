import { describe, expect, it } from "vitest";
import { createElement } from "react";
import type { UiElementNode, UiNode } from "@codypendent/ui";
import { UiProvider, createReactUiRoot } from "@codypendent/ui/react";
import { MediatedUiBridge } from "@codypendent/ui/worker";
import { WorkflowGraphCard, inferEdges, toGraphNodes } from "../src/component.js";

function elements(node: UiNode): UiElementNode[] {
  if (node.kind === "text") return [];
  return [node, ...node.children.flatMap(elements), ...(node.fallback === undefined ? [] : elements(node.fallback))];
}

function render(bridge: MediatedUiBridge, workflowRunId: string): UiNode {
  const root = createReactUiRoot({ documentId: "golden" });
  root.render(createElement(UiProvider, {
    state: bridge, actions: bridge, meta: bridge.meta,
    children: createElement(WorkflowGraphCard, { workflowRunId }),
  }));
  const document = root.getDocument();
  if (document === undefined) throw new Error("the React root produced no document");
  return document.root;
}

describe("WorkflowGraphCard", () => {
  it("shows an empty state until a workflow projection arrives", () => {
    const bridge = new MediatedUiBridge(async () => undefined);
    const document = render(bridge, "workflow-run-1");
    expect(elements(document).map((element) => element.type)).toContain("EmptyState");
    expect(document).toMatchSnapshot();
  });

  it("draws the run's nodes as a graph once the projection lands", () => {
    const sent: { kind?: string; subscriptionId?: string }[] = [];
    const bridge = new MediatedUiBridge(async (message) => {
      if (message.type === "subscription") sent.push({ kind: message.subscription.kind, subscriptionId: message.subscription.subscriptionId });
    });
    // Mount once so the card subscribes, then deliver the projection.
    render(bridge, "workflow-run-1");
    const subscription = sent.find((entry) => entry.kind === "workflow");
    expect(subscription?.subscriptionId).toBeDefined();
    bridge.applyProjection({
      subscriptionId: subscription?.subscriptionId as string,
      revision: 1,
      value: {
        workflowRunId: "workflow-run-1",
        phase: "running",
        nodes: [
          { workflowRunId: "workflow-run-1", nodeId: "plan", state: "completed", attempt: 1, warnings: [] },
          { workflowRunId: "workflow-run-1", nodeId: "build", state: "running", attempt: 1, warnings: [] },
          { workflowRunId: "workflow-run-1", nodeId: "verify", state: "idle", attempt: 1, warnings: [] },
        ],
      },
    });

    const document = render(bridge, "workflow-run-1");
    const graph = elements(document).find((element) => element.type === "Graph");
    expect(graph?.props.direction).toBe("vertical");
    expect(graph?.props.nodes).toHaveLength(3);
    // Edges are the top-level {from,to} array both hosts lay out as a DAG.
    expect(graph?.props.edges).toEqual([
      { id: "plan->build", from: "plan", to: "build" },
      { id: "build->verify", from: "build", to: "verify" },
    ]);
    // The Graph keeps a semantic list fallback for hosts that cannot draw it.
    expect(graph?.fallback).toMatchObject({ type: "List" });
  });

  it("maps unknown node states onto the declared status vocabulary", () => {
    expect(toGraphNodes([{ nodeId: "a", state: "running" }, { nodeId: "b", state: "wedged" }])).toEqual([
      { id: "a", label: "a", kind: "agent", status: "running" },
      { id: "b", label: "b", kind: "agent", status: "idle" },
    ]);
    expect(inferEdges([])).toEqual([]);
    expect(inferEdges(toGraphNodes([{ nodeId: "only", state: "idle" }]))).toEqual([]);
  });
});
