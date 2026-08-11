/** @jsxImportSource react */
import { describe, expect, it } from "vitest";
import { Stack, Text } from "../../src/react/primitives.js";
import { createReactUiRoot } from "../../src/react/renderer.js";
import { auditAccessibility } from "../../src/accessibility.js";
import type { UiDocument, UiElementNode, UiNode } from "../../src/protocol.js";
import { renderForTest, stableUiJson } from "../../src/testing.js";
import {
  AgentManagement,
  ApprovalReview,
  ConversationTranscript,
  CostQuotaView,
  DiffReview,
  DoctorReport,
  MediaViewer,
  MetricsDashboard,
  NotificationCenter,
  Surface,
  ToolCallLifecycle,
  WorkflowGraphView,
  WorktreeDashboard,
  coreOnlyData,
  toUiJson,
} from "../../src/first-party/index.js";

function render(element: React.ReactNode, documentId = "first-party-test"): UiDocument {
  const root = createReactUiRoot({ documentId });
  root.render(element);
  const document = root.getDocument();
  if (document === undefined) throw new Error("React UI root did not produce a document");
  return document;
}

function allElements(root: UiNode): UiElementNode[] {
  if (root.kind === "text") return [];
  return [root, ...root.children.flatMap(allElements), ...(root.fallback === undefined ? [] : allElements(root.fallback))];
}

function elementBy(root: UiNode, predicate: (element: UiElementNode) => boolean): UiElementNode {
  const found = allElements(root).find(predicate);
  if (found === undefined) throw new Error("Expected semantic element was not rendered");
  return found;
}

const approveIntent = { action: "core.approval.approve", label: "Approve", tone: "warning" } as const;
const denyIntent = { action: "core.approval.deny", label: "Deny", tone: "critical" } as const;

describe("first-party semantic component catalogue", () => {
  it("renders explicit load, empty, error, streaming, density, and width variants deterministically", () => {
    const document = render(
      <Stack id="states" gap="sm">
        <Surface.Loading id="loading" title="Loading surface" label="Loading workspace" density="compact" width="narrow" />
        <Surface.Empty id="empty" title="Empty surface" emptyTitle="Nothing here" emptyMessage="Create a resource to begin" />
        <Surface.Error id="error" title="Error surface" errorTitle="Could not load" errorMessage="Connection closed" />
        <Surface.Streaming id="streaming" title="Streaming surface" streamLabel="Receiving tokens" completed={25} total={100} width="full">
          <Text value="Partial response" />
        </Surface.Streaming>
      </Stack>,
      "surface-variants",
    );
    expect(stableUiJson(document.root)).toMatchSnapshot();
  });

  it("keeps approvals core-only, intent-only, and disabled until controlled confirmation", () => {
    const component = (confirmation: "unconfirmed" | "confirmed") => (
      <ApprovalReview
        id="approval"
        title="Deployment approval"
        approvalId="approval-1"
        requestedBy="release-agent"
        summary="Deploy production"
        rationale="Changes production traffic"
        risk="high"
        permissions={[{ resource: "production", operation: "deploy", before: "denied", after: "allowed", reason: "release 42" }]}
        confirmation={confirmation}
        confirmationAction="core.approval.confirmation.change"
        approveIntent={approveIntent}
        denyIntent={denyIntent}
      />
    );
    const unconfirmed = render(component("unconfirmed"), "approval-unconfirmed");
    const card = elementBy(unconfirmed.root, (element) => element.type === "ApprovalCard");
    expect(card.props.data).toMatchObject({ governance: "core-only", authority: "intent-only", kind: "approval" });
    const approve = elementBy(unconfirmed.root, (element) => element.type === "Button" && element.props.action === approveIntent.action);
    expect(approve.props).toMatchObject({ disabled: true, action: approveIntent.action });

    const confirmed = render(component("confirmed"), "approval-confirmed");
    const enabledApprove = elementBy(confirmed.root, (element) => element.type === "Button" && element.props.action === approveIntent.action);
    expect(enabledApprove.props.disabled).toBe(false);
    expect(allElements(confirmed.root).filter((element) => element.type === "Button").every((button) => typeof button.props.action === "string")).toBe(true);
  });

  it("preserves the complete data set in a virtualized transcript while bounding rendered rows", () => {
    const messages = Array.from({ length: 120 }, (_, index) => ({
      id: `message-${index}`,
      role: index % 2 === 0 ? "user" as const : "assistant" as const,
      author: index % 2 === 0 ? "Daniel" : "Codypendent",
      content: `Message ${index}`,
      format: "text" as const,
      createdAt: `2026-08-11T10:${String(index % 60).padStart(2, "0")}:00Z`,
      status: "complete" as const,
    }));
    const document = render(
      <ConversationTranscript id="transcript" title="Conversation" messages={messages} selectAction="conversation.message.select" />,
      "virtual-transcript",
    );
    const list = elementBy(document.root, (element) => element.type === "VirtualList");
    expect(list.props.virtualized).toBe(true);
    expect(list.props.items).toHaveLength(120);
    expect(list.children).toHaveLength(50);
  });

  it("provides terminal-safe semantic fallbacks for rich diff and media views", () => {
    const artifact = { id: "diagram", title: "Architecture", mediaType: "image/png", revision: 1, status: "ready" as const };
    const document = render(
      <Stack>
        <DiffReview
          id="diff"
          title="Diff"
          artifactId="patch-1"
          path="src/app.ts"
          patch="@@ -1 +1 @@\n-old\n+new"
          mode="sideBySide"
          hunks={[{ id: "hunk-1", header: "app.ts:1", patch: "-old\n+new" }]}
          selectHunkAction="artifact.hunk.select"
        />
        <MediaViewer id="media" title="Media" artifact={artifact} source="artifact://diagram" kind="image" alt="System architecture diagram" />
      </Stack>,
      "fallbacks",
    );
    const diff = elementBy(document.root, (element) => element.type === "Diff");
    const image = elementBy(document.root, (element) => element.type === "Image");
    expect(diff.fallback?.kind).toBe("element");
    expect(image.fallback?.kind).toBe("element");
    expect(diff.requires).toContainEqual({ feature: "diffView", optional: true });
    expect(image.requires).toContainEqual({ feature: "imageDisplay" });
  });

  it("emits revision-bound semantic action events without executing authority in the component", () => {
    const document = render(
      <ApprovalReview
        id="approval-event"
        title="Approval"
        approvalId="approval-2"
        requestedBy="agent"
        summary="Write file"
        rationale="Apply reviewed patch"
        risk="medium"
        permissions={[]}
        confirmation="confirmed"
        confirmationAction="core.approval.confirmation.change"
        approveIntent={approveIntent}
        denyIntent={denyIntent}
      />,
      "approval-event-document",
    );
    const button = elementBy(document.root, (element) => element.type === "Button" && element.props.action === approveIntent.action);
    const harness = renderForTest(document.root, { documentId: document.documentId });
    const event = harness.dispatch(button.id as string, "action", toUiJson({ action: button.props.action, payload: button.props.payload ?? null }));
    expect(event).toMatchObject({ documentId: document.documentId, revision: 0, targetId: button.id, type: "action" });
    expect(harness.events).toHaveLength(1);
  });

  it("renders every major operational family as accessible semantic nodes", () => {
    const document = render(
      <Stack id="catalogue" gap="md">
        <AgentManagement
          id="agents"
          title="Agents"
          agents={[{ id: "reviewer", name: "Reviewer", description: "Reviews changes", capabilities: ["review"], status: "available", source: "built-in" }]}
          selectAction="agents.select"
          configureIntent={{ action: "agents.configure", label: "Configure" }}
          invokeIntent={{ action: "agents.invoke", label: "Invoke" }}
        />
        <WorkflowGraphView
          id="workflow"
          title="Workflow"
          workflowId="workflow-1"
          nodes={[{ id: "node-1", label: "Review", kind: "agent", status: "running" }]}
          edges={[]}
          direction="horizontal"
          selectNodeAction="workflow.node.select"
        />
        <WorktreeDashboard
          id="worktrees"
          title="Worktrees"
          worktrees={[{ id: "wt-1", path: "/workspace", branch: "codex/feature", head: "abc1234", status: "modified", ahead: 1, behind: 0, changeCount: 2 }]}
          selectAction="git.worktree.select"
        />
        <CostQuotaView
          id="cost"
          title="Cost and quota"
          period="24h"
          totalCost={1.25}
          currency="GBP"
          usage={[{ timestamp: "2026-08-11T10:00:00Z", inputTokens: 100, outputTokens: 50, cost: 1.25 }]}
          quotas={[{ id: "tokens", label: "Tokens", used: 150, limit: 1000, unit: "tokens" }]}
        />
        <MetricsDashboard
          id="metrics"
          title="Metrics"
          period="1h"
          periodAction="metrics.period.change"
          metrics={[{ id: "latency", label: "Latency", unit: "ms", status: "healthy", current: 20, points: [{ timestamp: "2026-08-11T10:00:00Z", value: 20 }] }]}
        />
        <DoctorReport
          id="doctor"
          title="Doctor"
          generatedAt="2026-08-11T10:00:00Z"
          checks={[{ id: "daemon", category: "runtime", label: "Daemon", status: "healthy", detail: "Reachable" }]}
          rerunIntent={{ action: "doctor.run", label: "Run diagnostics" }}
        />
        <NotificationCenter
          id="notifications"
          title="Notifications"
          notifications={[{ id: "notice-1", title: "Run complete", message: "Review is ready", tone: "positive", createdAt: "2026-08-11T10:00:00Z", read: false }]}
          selectAction="notifications.select"
          markReadAction="notifications.read"
          dismissAction="notifications.dismiss"
        />
      </Stack>,
      "catalogue-families",
    );
    const issues = auditAccessibility(document.root).filter((issue) => issue.severity === "error");
    expect(issues).toEqual([]);
    const renderedTypes = new Set(allElements(document.root).map((element) => element.type));
    for (const type of ["AgentCard", "Graph", "CostView", "Chart", "Toast"]) {
      expect(renderedTypes.has(type)).toBe(true);
    }
  });

  it("renders tool lifecycle input, virtual logs, and results without leaking non-JSON values", () => {
    const document = render(
      <ToolCallLifecycle
        id="tool"
        title="Tool call"
        toolCallId="tool-1"
        toolName="search"
        status="succeeded"
        input={{ query: "semantic UI" }}
        result={{ hits: 3 }}
        logs={[{ sequence: 1, timestamp: "2026-08-11T10:00:00Z", level: "info", message: "complete" }]}
      />,
      "tool-lifecycle",
    );
    expect(elementBy(document.root, (element) => element.type === "ToolCard").props.status).toBe("succeeded");
    expect(elementBy(document.root, (element) => element.type === "JsonTree").props.value).toEqual({ query: "semantic UI" });
  });

  it("serializes domain data deterministically and rejects cycles/non-finite values", () => {
    expect(stableUiJson(toUiJson({ z: 1, omitted: undefined, nested: { b: true, a: "first" } }))).toBe(`{
  "nested": {
    "a": "first",
    "b": true
  },
  "z": 1
}`);
    const cyclic: { self?: unknown } = {};
    cyclic.self = cyclic;
    expect(() => toUiJson(cyclic)).toThrow("contains a cycle");
    expect(() => toUiJson({ value: Number.NaN })).toThrow("non-finite number");
    expect(coreOnlyData("policy", { scope: "workspace" })).toMatchObject({ governance: "core-only", authority: "intent-only" });
  });
});
