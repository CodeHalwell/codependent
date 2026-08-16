/** @jsxImportSource react */
import type { ReactNode } from "react";
import {
  Badge,
  Box,
  Button,
  Grid,
  Progress,
  Row,
  Sparkline,
  Stack,
  Text,
} from "../react/primitives.js";
import { StatusBadge, SurfaceFrame } from "./foundation.js";
import type { SurfaceOptions } from "./types.js";

export interface AgentMetric {
  id: string;
  name: string;
  role: "primary" | "subagent" | "planner" | "critic" | "verifier";
  status: "idle" | "running" | "waiting_approval" | "paused" | "completed" | "failed";
  model: string;
  stepCount: number;
  maxSteps: number;
  tokensUsed: number;
  contextWindow: number;
  wallClockSecs: number;
  currentTool?: string;
  recentActivity?: readonly number[];
}

export interface AgentDashboardProps extends SurfaceOptions {
  agents: readonly AgentMetric[];
  selectedAgentId?: string;
  onSelectAgent?: string;
  onPauseAgent?: string;
  onResumeAgent?: string;
  onCancelAgent?: string;
  onSteerAgent?: string;
}

export function AgentDashboard({
  agents,
  selectedAgentId,
  onSelectAgent = "select-agent",
  onPauseAgent = "pause-agent",
  onResumeAgent = "resume-agent",
  onCancelAgent = "cancel-agent",
  onSteerAgent = "steer-agent",
  ...surface
}: AgentDashboardProps): ReactNode {
  return (
    <SurfaceFrame
      {...surface}
      title={surface.title ?? "Multi-Agent Dashboard"}
      description={surface.description ?? "Real-time orchestration overview, resource tracking, and agent telemetry"}
    >
      <Grid columns={2} gap="sm">
        {agents.slice(0, 50).map((agent) => {
          const isSelected = agent.id === selectedAgentId;
          const contextUsagePct = Math.round(
            (agent.tokensUsed / Math.max(1, agent.contextWindow)) * 100
          );
          const stepUsagePct = Math.round(
            (agent.stepCount / Math.max(1, agent.maxSteps)) * 100
          );

          return (
            <Box
              key={agent.id}
              id={`${surface.id}-agent-card-${agent.id}`}
              border={isSelected ? "double" : "rounded"}
              padding="sm"
              title={`${agent.name} (${agent.role})`}
            >
              <Stack gap="xs">
                <Row align="spaceBetween">
                  <Row gap="xs">
                    <StatusBadge status={agent.status} />
                    <Text value={agent.model} tone="muted" />
                  </Row>
                  <Text value={`${agent.wallClockSecs}s`} tone="muted" />
                </Row>

                {agent.currentTool && (
                  <Row gap="xs">
                    <Text value="Tool:" role="caption" />
                    <Badge tone="warning" message={agent.currentTool} />
                  </Row>
                )}

                {/* Progress Indicators */}
                <Stack gap="xs">
                  <Row align="spaceBetween">
                    <Text value="Step Budget" role="caption" />
                    <Text
                      value={`${agent.stepCount}/${agent.maxSteps} (${stepUsagePct}%)`}
                      role="caption"
                    />
                  </Row>
                  <Progress value={stepUsagePct} maximum={100} />
                </Stack>

                <Stack gap="xs">
                  <Row align="spaceBetween">
                    <Text value="Context Window" role="caption" />
                    <Text
                      value={`${agent.tokensUsed.toLocaleString()} / ${agent.contextWindow.toLocaleString()} (${contextUsagePct}%)`}
                      role="caption"
                    />
                  </Row>
                  <Progress value={contextUsagePct} maximum={100} />
                </Stack>

                {agent.recentActivity && agent.recentActivity.length > 0 && (
                  <Sparkline values={agent.recentActivity} tone="neutral" />
                )}

                {/* Control Actions */}
                <Row gap="xs" align="end">
                  <Button
                    id={`${surface.id}-select-${agent.id}`}
                    label="Focus"
                    action={onSelectAgent}
                    payload={{ agentId: agent.id }}
                  />
                  {agent.status === "running" ? (
                    <Button
                      id={`${surface.id}-pause-${agent.id}`}
                      label="Pause"
                      action={onPauseAgent}
                      payload={{ agentId: agent.id }}
                    />
                  ) : agent.status === "paused" ? (
                    <Button
                      id={`${surface.id}-resume-${agent.id}`}
                      label="Resume"
                      action={onResumeAgent}
                      payload={{ agentId: agent.id }}
                    />
                  ) : null}
                  <Button
                    id={`${surface.id}-steer-${agent.id}`}
                    label="Steer"
                    action={onSteerAgent}
                    payload={{ agentId: agent.id }}
                  />
                  <Button
                    id={`${surface.id}-cancel-${agent.id}`}
                    label="Cancel"
                    tone="critical"
                    action={onCancelAgent}
                    payload={{ agentId: agent.id }}
                  />
                </Row>
              </Stack>
            </Box>
          );
        })}
      </Grid>
    </SurfaceFrame>
  );
}
