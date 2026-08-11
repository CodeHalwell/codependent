/** @jsxImportSource react */
import type { ReactNode } from "react";
import {
  Alert,
  ApprovalCard,
  Checkbox,
  Code,
  Details,
  JsonTree,
  KeyValue,
  LogViewer,
  Markdown,
  PermissionDiff,
  Progress,
  Row,
  Stack,
  Text,
  Timeline,
  ToolCard,
} from "../react/primitives.js";
import { coreOnlyData, IntentButton, StatusBadge, SurfaceFrame, VirtualizedCollection } from "./foundation.js";
import { toUiJson, type SemanticIntent, type SurfaceOptions } from "./types.js";

export interface RunStage {
  id: string;
  label: string;
  status: "queued" | "running" | "completed" | "failed" | "cancelled" | "blocked";
  detail?: string;
  startedAt?: string;
  completedAt?: string;
}

export interface RunProgressProps extends SurfaceOptions {
  runId: string;
  status: RunStage["status"];
  stages: readonly RunStage[];
  completedUnits?: number;
  totalUnits?: number;
  elapsed: string;
  cancelIntent?: SemanticIntent<{ runId: string }>;
  steerIntent?: SemanticIntent<{ runId: string }>;
}

export function RunProgress({
  runId,
  status,
  stages,
  completedUnits,
  totalUnits,
  elapsed,
  cancelIntent,
  steerIntent,
  ...surface
}: RunProgressProps): ReactNode {
  const determinate = completedUnits !== undefined && totalUnits !== undefined;
  const actions: SemanticIntent[] = [];
  if (steerIntent !== undefined) actions.push({ ...steerIntent, payload: { runId } });
  if (cancelIntent !== undefined) actions.push({ ...cancelIntent, payload: { runId } });
  return (
    <SurfaceFrame {...surface} actions={actions}>
      <Stack gap="sm">
        <Row align="spaceBetween">
          <StatusBadge status={status} label={`Run ${runId}`} />
          <Text value={`Elapsed ${elapsed}`} role="status" accessibleLabel={`Run elapsed time: ${elapsed}`} />
        </Row>
        <Progress
          indeterminate={!determinate && status === "running"}
          label={determinate ? `${completedUnits} of ${totalUnits}` : status}
          accessibleLabel={`Run progress: ${determinate ? `${completedUnits} of ${totalUnits}` : status}`}
          {...(determinate ? { value: completedUnits, maximum: totalUnits } : {})}
        />
        <Timeline
          items={stages.map((stage) => toUiJson(stage))}
          virtualized={stages.length > 50}
          emptyMessage="No run stages"
          accessibleLabel={`${stages.length} run stages`}
        >
          {stages.slice(-25).map((stage) => (
            <Row key={stage.id} align="spaceBetween" accessibleLabel={`${stage.label}: ${stage.status}`}>
              <Stack gap="xs">
                <Text value={stage.label} weight="medium" />
                {stage.detail === undefined ? null : <Text value={stage.detail} role="caption" tone="muted" />}
              </Stack>
              <StatusBadge status={stage.status} />
            </Row>
          ))}
        </Timeline>
      </Stack>
    </SurfaceFrame>
  );
}

export interface ToolLogLine {
  sequence: number;
  timestamp: string;
  level: "debug" | "info" | "warning" | "error";
  message: string;
}

export interface ToolCallLifecycleProps extends SurfaceOptions {
  toolCallId: string;
  toolName: string;
  status: "queued" | "running" | "waiting" | "succeeded" | "failed" | "cancelled";
  input: unknown;
  result?: unknown;
  resultFormat?: "json" | "markdown" | "code";
  logs: readonly ToolLogLine[];
  startedAt?: string;
  duration?: string;
  cancelIntent?: SemanticIntent<{ toolCallId: string }>;
  retryIntent?: SemanticIntent<{ toolCallId: string }>;
}

export function ToolCallLifecycle({
  toolCallId,
  toolName,
  status,
  input,
  result,
  resultFormat = "json",
  logs,
  startedAt,
  duration,
  cancelIntent,
  retryIntent,
  ...surface
}: ToolCallLifecycleProps): ReactNode {
  const actions: string[] = [];
  if (cancelIntent !== undefined) actions.push(cancelIntent.action);
  if (retryIntent !== undefined) actions.push(retryIntent.action);
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "wide"}>
      <ToolCard
        resourceId={toolCallId}
        title={toolName}
        status={status}
        actions={actions}
        data={toUiJson({ toolCallId, input, startedAt, duration })}
        accessibleLabel={`${toolName} tool call: ${status}`}
      >
        <Stack gap="sm">
          <Row align="spaceBetween">
            <StatusBadge status={status} />
            <Row gap="xs">
              {cancelIntent === undefined ? null : <IntentButton intent={{ ...cancelIntent, payload: { toolCallId } }} />}
              {retryIntent === undefined ? null : <IntentButton intent={{ ...retryIntent, payload: { toolCallId } }} />}
            </Row>
          </Row>
          <Details title="Tool input" accessibleLabel={`${toolName} input`}>
            <JsonTree value={toUiJson(input)} expandedDepth={2} accessibleLabel={`${toolName} input data`} />
          </Details>
          <Details title={`Logs (${logs.length})`} accessibleLabel={`${toolName} logs`}>
            <LogViewer
              lines={logs.map((line) => toUiJson(line))}
              level="all"
              accessibleLabel={`${logs.length} log lines from ${toolName}`}
              description="Use Page Up and Page Down to move through the log."
            />
          </Details>
          {result === undefined ? null : (
            <Details title="Tool result" open accessibleLabel={`${toolName} result`}>
              {resultFormat === "json" ? <JsonTree value={toUiJson(result)} expandedDepth={3} /> : null}
              {resultFormat === "markdown" ? <Markdown source={String(result)} /> : null}
              {resultFormat === "code" ? <Code value={String(result)} lineNumbers /> : null}
            </Details>
          )}
        </Stack>
      </ToolCard>
    </SurfaceFrame>
  );
}

export interface PermissionChange {
  resource: string;
  operation: string;
  before: string;
  after: string;
  reason?: string;
}

export interface ApprovalReviewProps extends SurfaceOptions {
  approvalId: string;
  requestedBy: string;
  summary: string;
  rationale: string;
  risk: "low" | "medium" | "high" | "critical";
  permissions: readonly PermissionChange[];
  expiresAt?: string;
  confirmation: "unconfirmed" | "confirmed";
  confirmationAction: string;
  approveIntent: SemanticIntent<{ approvalId: string }>;
  denyIntent: SemanticIntent<{ approvalId: string }>;
}

export function ApprovalReview({
  approvalId,
  requestedBy,
  summary,
  rationale,
  risk,
  permissions,
  expiresAt,
  confirmation,
  confirmationAction,
  approveIntent,
  denyIntent,
  ...surface
}: ApprovalReviewProps): ReactNode {
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "narrow"}>
      <ApprovalCard
        resourceId={approvalId}
        title={summary}
        status="pending-human-decision"
        actions={[approveIntent.action, denyIntent.action]}
        data={coreOnlyData("approval", { approvalId, requestedBy, rationale, risk, permissions, expiresAt })}
        accessibleLabel={`Approval required: ${summary}. Risk ${risk}. Requested by ${requestedBy}.`}
        description="This component only emits an intent. The trusted core validates and authorizes any decision."
      >
        <Stack gap="sm">
          <Alert
            tone={risk === "critical" || risk === "high" ? "critical" : "warning"}
            title={`${risk} risk approval`}
            message={rationale}
            accessibleLabel={`${risk} risk. ${rationale}`}
          />
          <KeyValue entries={{ requestedBy, ...(expiresAt === undefined ? {} : { expiresAt }), governance: "core-only" }} />
          <PermissionDiff
            resourceId={approvalId}
            status="review-required"
            data={coreOnlyData("permission", permissions)}
            accessibleLabel={`${permissions.length} permission changes to review`}
          >
            <VirtualizedCollection
              id={`${surface.id}-permission-changes`}
              label="Requested permission changes"
              items={permissions}
              emptyMessage="No permission changes"
              itemKey={(change) => `${change.resource}:${change.operation}`}
            >
              {permissions.slice(0, 25).map((change) => (
                <Stack key={`${change.resource}:${change.operation}`} gap="xs">
                  <Text value={`${change.operation} ${change.resource}`} weight="medium" />
                  <Text value={`${change.before} → ${change.after}`} />
                  {change.reason === undefined ? null : <Text value={change.reason} role="caption" tone="muted" />}
                </Stack>
              ))}
            </VirtualizedCollection>
          </PermissionDiff>
          <Checkbox
            name="approvalConfirmation"
            checked={confirmation === "confirmed"}
            value={confirmation}
            changeAction={confirmationAction}
            accessibleLabel="I reviewed the exact requested scope and understand the risk"
            description="Confirmation does not grant permission; it enables submitting an approval intent."
          />
          <Row align="end" gap="sm">
            <IntentButton intent={{ ...denyIntent, payload: { approvalId } }} />
            <IntentButton
              intent={{ ...approveIntent, payload: { approvalId } }}
              confirmationState={confirmation}
            />
          </Row>
        </Stack>
      </ApprovalCard>
    </SurfaceFrame>
  );
}

export interface CoreDecisionPromptProps extends SurfaceOptions {
  requestId: string;
  kind: "secret" | "policy";
  subject: string;
  scope: readonly string[];
  reason: string;
  decision: "unconfirmed" | "confirmed";
  confirmAction: string;
  allowIntent: SemanticIntent<{ requestId: string }>;
  denyIntent: SemanticIntent<{ requestId: string }>;
}

export function CoreDecisionPrompt({
  requestId,
  kind,
  subject,
  scope,
  reason,
  decision,
  confirmAction,
  allowIntent,
  denyIntent,
  ...surface
}: CoreDecisionPromptProps): ReactNode {
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "narrow"}>
      <Stack gap="sm">
        <Alert
          tone="warning"
          title={`${kind === "secret" ? "Secret access" : "Policy exception"} requires trusted-core review`}
          message={reason}
        />
        <ApprovalCard
          resourceId={requestId}
          status="core-only"
          data={coreOnlyData(kind, { requestId, subject, scope, reason })}
          accessibleLabel={`${kind} request for ${subject}`}
        >
          <KeyValue entries={{ requestId, subject, scope: toUiJson(scope), governance: "core-only", authority: "intent-only" }} />
        </ApprovalCard>
        <Checkbox
          name={`${kind}Confirmation`}
          checked={decision === "confirmed"}
          changeAction={confirmAction}
          accessibleLabel={`I reviewed the ${kind} scope: ${scope.join(", ")}`}
        />
        <Row align="end" gap="sm">
          <IntentButton intent={{ ...denyIntent, payload: { requestId } }} />
          <IntentButton intent={{ ...allowIntent, payload: { requestId } }} confirmationState={decision} />
        </Row>
      </Stack>
    </SurfaceFrame>
  );
}
