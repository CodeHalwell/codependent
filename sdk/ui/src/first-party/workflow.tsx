/** @jsxImportSource react */
import type { ReactNode } from "react";
import {
  Button,
  Graph,
  KeyValue,
  Row,
  Stack,
  Text,
  Timeline,
  WorkflowNode,
} from "../react/primitives.js";
import { StatusBadge, SurfaceFrame, VirtualizedCollection } from "./foundation.js";
import { toUiJson, type SemanticIntent, type SurfaceOptions } from "./types.js";

export interface WorkflowGraphNode {
  id: string;
  label: string;
  kind: "agent" | "tool" | "decision" | "approval" | "artifact" | "terminal";
  status: "idle" | "queued" | "running" | "waiting" | "completed" | "failed" | "skipped";
  agentId?: string;
}

export interface WorkflowGraphEdge {
  id: string;
  from: string;
  to: string;
  label?: string;
  condition?: string;
  status?: string;
}

export interface WorkflowGraphViewProps extends SurfaceOptions {
  workflowId: string;
  nodes: readonly WorkflowGraphNode[];
  edges: readonly WorkflowGraphEdge[];
  direction: "horizontal" | "vertical";
  selectedNodeId?: string;
  selectNodeAction: string;
  runIntent?: SemanticIntent<{ workflowId: string }>;
  editIntent?: SemanticIntent<{ workflowId: string }>;
}

export function WorkflowGraphView({ workflowId, nodes, edges, direction, selectedNodeId, selectNodeAction, runIntent, editIntent, ...surface }: WorkflowGraphViewProps): ReactNode {
  const actions: SemanticIntent[] = [];
  if (editIntent !== undefined) actions.push({ ...editIntent, payload: { workflowId } });
  if (runIntent !== undefined) actions.push({ ...runIntent, payload: { workflowId } });
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "full"} actions={actions}>
      <Stack gap="sm">
        <Graph
          nodes={nodes.map((node) => toUiJson(node))}
          edges={edges.map((edge) => toUiJson(edge))}
          direction={direction}
          accessibleLabel={`Workflow graph with ${nodes.length} nodes and ${edges.length} connections`}
          fallback={{
            kind: "element",
            type: "List",
            props: { items: nodes.map((node) => toUiJson(node)), virtualized: true, accessibleLabel: "Workflow nodes" },
            children: [],
          }}
        />
        <VirtualizedCollection
          id={`${surface.id}-node-list`}
          label="Workflow nodes"
          items={nodes}
          selectedKey={selectedNodeId}
          emptyMessage="This workflow has no nodes"
          itemKey={(node) => node.id}
        >
          {nodes.slice(0, 30).map((node) => (
            <WorkflowNode
              key={node.id}
              resourceId={node.id}
              title={node.label}
              status={node.status}
              data={toUiJson(node)}
              accessibleLabel={`${node.label}, ${node.kind}, ${node.status}`}
            >
              <Row align="spaceBetween">
                <StatusBadge status={node.status} />
                <Button action={selectNodeAction} label="Inspect" payload={toUiJson({ nodeId: node.id })} accessibleLabel={`Inspect ${node.label}`} />
              </Row>
            </WorkflowNode>
          ))}
        </VirtualizedCollection>
      </Stack>
    </SurfaceFrame>
  );
}

export interface WorkflowEvent {
  id: string;
  nodeId?: string;
  timestamp: string;
  label: string;
  status: string;
  detail?: string;
  durationMs?: number;
}

export interface WorkflowTimelineProps extends SurfaceOptions {
  events: readonly WorkflowEvent[];
  selectedEventId?: string;
  selectAction: string;
}

export function WorkflowTimeline({ events, selectedEventId, selectAction, ...surface }: WorkflowTimelineProps): ReactNode {
  return (
    <SurfaceFrame {...surface}>
      <Timeline
        items={events.map((event) => toUiJson(event))}
        virtualized
        emptyMessage="No workflow events"
        accessibleLabel={`${events.length} workflow timeline events`}
        {...(selectedEventId === undefined ? {} : { selectedKey: selectedEventId })}
      >
        {events.slice(-50).map((event) => (
          <Row key={event.id} align="spaceBetween">
            <Stack gap="xs">
              <Text value={`${event.timestamp} · ${event.label}`} weight="medium" />
              {event.detail === undefined ? null : <Text value={event.detail} role="caption" tone="muted" />}
            </Stack>
            <Row gap="xs">
              <StatusBadge status={event.status} />
              <Button action={selectAction} label="Inspect" payload={toUiJson({ eventId: event.id })} accessibleLabel={`Inspect ${event.label} event`} />
            </Row>
          </Row>
        ))}
      </Timeline>
    </SurfaceFrame>
  );
}

export interface WorkflowNodeInspectorProps extends SurfaceOptions {
  node: WorkflowGraphNode;
  configuration: Readonly<Record<string, unknown>>;
  inputs: Readonly<Record<string, unknown>>;
  outputs?: Readonly<Record<string, unknown>>;
  actions?: readonly SemanticIntent<{ nodeId: string }>[];
}

export function WorkflowNodeInspector({ node, configuration, inputs, outputs, actions = [], ...surface }: WorkflowNodeInspectorProps): ReactNode {
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "narrow"} actions={actions.map((intent) => ({ ...intent, payload: { nodeId: node.id } }))}>
      <WorkflowNode resourceId={node.id} title={node.label} status={node.status} data={toUiJson(node)} accessibleLabel={`${node.label} inspector`}>
        <Stack gap="sm">
          <StatusBadge status={node.status} />
          <KeyValue entries={{ kind: node.kind, ...(node.agentId === undefined ? {} : { agentId: node.agentId }) }} />
          <KeyValue entries={{ configuration: toUiJson(configuration), inputs: toUiJson(inputs), ...(outputs === undefined ? {} : { outputs: toUiJson(outputs) }) }} accessibleLabel="Node inputs, configuration, and outputs" />
        </Stack>
      </WorkflowNode>
    </SurfaceFrame>
  );
}
