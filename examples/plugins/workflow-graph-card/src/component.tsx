import { Text as semanticText } from "@codypendent/ui";
import { EmptyState, useWorkflow } from "@codypendent/ui/react";
import { WorkflowGraphView, type WorkflowGraphEdge, type WorkflowGraphNode } from "@codypendent/ui/first-party";

/** The dependency edges a workflow run's node ids imply, in declared order. */
export function inferEdges(nodes: readonly WorkflowGraphNode[]): WorkflowGraphEdge[] {
  return nodes.slice(1).map((node, index) => ({
    id: `${nodes[index]?.id ?? "start"}->${node.id}`,
    from: nodes[index]?.id ?? "start",
    to: node.id,
  }));
}

/**
 * Map a live `workflow` projection onto the first-party graph vocabulary. The
 * projection carries node state only, so `kind` stays `agent` — the graph
 * renders status and shape, and the inspector rail carries the detail.
 */
export function toGraphNodes(nodes: readonly { nodeId: string; state: string }[]): WorkflowGraphNode[] {
  const statuses = new Set(["idle", "queued", "running", "waiting", "completed", "failed", "skipped"]);
  return nodes.map((node) => ({
    id: node.nodeId,
    label: node.nodeId,
    kind: "agent" as const,
    status: (statuses.has(node.state) ? node.state : "idle") as WorkflowGraphNode["status"],
  }));
}

export interface WorkflowGraphCardProps {
  /** The workflow run this card observes. Supplied by the host's slot context. */
  workflowRunId: string;
  selectedNodeId?: string;
}

export function WorkflowGraphCard({ workflowRunId, selectedNodeId }: WorkflowGraphCardProps) {
  const workflow = useWorkflow(workflowRunId);
  if (workflow === undefined) {
    return (
      <EmptyState
        id="root"
        title="No workflow run"
        message={`Waiting for a projection of ${workflowRunId}.`}
        accessibleLabel="No workflow run"
        fallback={semanticText({ value: "No workflow run" })}
      />
    );
  }
  const nodes = toGraphNodes(workflow.nodes);
  return (
    <WorkflowGraphView
      id="root"
      title={`Workflow ${workflow.workflowRunId}`}
      description={`Phase: ${workflow.phase}`}
      workflowId={workflow.workflowRunId}
      nodes={nodes}
      edges={inferEdges(nodes)}
      direction="vertical"
      selectNodeAction="workflow.node.select"
      {...(selectedNodeId === undefined ? {} : { selectedNodeId })}
    />
  );
}
