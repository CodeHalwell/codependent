import { workbenchHotReloadState } from "@codypendent/ui";
import { defaultWorkerCapabilities, runStdioUiWorker } from "@codypendent/ui/worker";
import { createReactUiSurface } from "@codypendent/ui/worker/react";
import { WorkflowGraphCard } from "./component.js";

/**
 * The workflow run this instance observes. The host passes it in the
 * environment when it launches an instance for a slot; the placeholder keeps
 * the smoke test (which boots the worker with no run attached) rendering the
 * card's empty state instead of failing.
 */
const workflowRunId = process.env.CODYPENDENT_WORKFLOW_RUN_ID ?? "workflow-run-unbound";

await runStdioUiWorker({
  pluginId: "workflow-graph-card",
  capabilityOffer: defaultWorkerCapabilities({
    capabilities: ["workflow-read"],
    contributionPoints: ["dashboard-card", "workflow-inspector"],
  }),
  surfaces: [createReactUiSurface({
    documentId: "main",
    strictMode: true,
    render: () => <WorkflowGraphCard workflowRunId={workflowRunId} />,
  })],
  contributions: [
    { id: "workflow-graph-card.dashboard", point: "dashboard-card", renderer: "workflow-graph-card.WorkflowGraphCard", documentId: "main" },
    { id: "workflow-graph-card.inspector", point: "workflow-inspector", renderer: "workflow-graph-card.WorkflowGraphCard", documentId: "main" },
  ],
  hotReloadState: workbenchHotReloadState,
});
