/** @jsxImportSource react */
import type { ReactNode } from "react";
import {
  Alert,
  Button,
  Checkbox,
  Details,
  KeyValue,
  Progress,
  Row,
  Stack,
  Text,
  Timeline,
  Toast,
} from "../react/primitives.js";
import { IntentButton, StatusBadge, SurfaceFrame, VirtualizedCollection } from "./foundation.js";
import { toUiJson, type SemanticIntent, type SurfaceOptions } from "./types.js";

export interface OnboardingStep {
  id: string;
  title: string;
  description: string;
  status: "pending" | "running" | "completed" | "blocked" | "skipped";
  optional?: boolean;
  action?: SemanticIntent<{ stepId: string }>;
}

export interface OnboardingFlowProps extends SurfaceOptions {
  steps: readonly OnboardingStep[];
  activeStepId?: string;
  selectAction: string;
  finishIntent?: SemanticIntent;
}

export function OnboardingFlow({ steps, activeStepId, selectAction, finishIntent, ...surface }: OnboardingFlowProps): ReactNode {
  const completed = steps.filter((step) => step.status === "completed" || step.status === "skipped").length;
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "narrow"} actions={finishIntent === undefined ? [] : [finishIntent]}>
      <Stack gap="sm">
        <Progress value={completed} maximum={steps.length} label={`${completed} of ${steps.length} setup steps`} accessibleLabel={`Setup progress: ${completed} of ${steps.length} steps`} />
        <Timeline items={steps.map((step) => toUiJson(step))} emptyMessage="No setup steps" accessibleLabel="Setup steps" {...(activeStepId === undefined ? {} : { selectedKey: activeStepId })}>
          {steps.map((step) => (
            <Stack key={step.id} gap="xs">
              <Row align="spaceBetween"><Text value={step.title} weight="bold" /><StatusBadge status={step.status} /></Row>
              <Text value={step.description} />
              <Row align="end" gap="xs">
                <Button action={selectAction} label="Open" payload={toUiJson({ stepId: step.id })} accessibleLabel={`Open ${step.title}`} />
                {step.action === undefined ? null : <IntentButton intent={{ ...step.action, payload: { stepId: step.id } }} />}
              </Row>
            </Stack>
          ))}
        </Timeline>
      </Stack>
    </SurfaceFrame>
  );
}

export interface DoctorCheck {
  id: string;
  category: string;
  label: string;
  status: "healthy" | "warning" | "unhealthy" | "running";
  detail: string;
  remediation?: string;
  fixIntent?: SemanticIntent<{ checkId: string }>;
}

export interface DoctorReportProps extends SurfaceOptions {
  generatedAt: string;
  checks: readonly DoctorCheck[];
  rerunIntent: SemanticIntent;
  exportIntent?: SemanticIntent;
}

export function DoctorReport({ generatedAt, checks, rerunIntent, exportIntent, ...surface }: DoctorReportProps): ReactNode {
  const actions = [rerunIntent, exportIntent].filter((intent): intent is SemanticIntent => intent !== undefined);
  return (
    <SurfaceFrame {...surface} actions={actions} description={surface.description ?? `Generated ${generatedAt}`}>
      <VirtualizedCollection id={`${surface.id}-checks`} label={`${checks.length} diagnostic checks`} items={checks} emptyMessage="No diagnostic checks" itemKey={(check) => check.id}>
        {checks.slice(0, 50).map((check) => (
          <Details key={check.id} title={`${check.category}: ${check.label}`} accessibleLabel={`${check.label}: ${check.status}`}>
            <Stack gap="xs">
              <Row align="spaceBetween"><Text value={check.detail} /><StatusBadge status={check.status} /></Row>
              {check.remediation === undefined ? null : <Alert tone={check.status === "unhealthy" ? "critical" : "warning"} title="Suggested remediation" message={check.remediation} />}
              {check.fixIntent === undefined ? null : <IntentButton intent={{ ...check.fixIntent, payload: { checkId: check.id } }} />}
            </Stack>
          </Details>
        ))}
      </VirtualizedCollection>
    </SurfaceFrame>
  );
}

export interface UpdateRelease {
  currentVersion: string;
  targetVersion: string;
  channel: "stable" | "preview" | "nightly";
  status: "available" | "downloading" | "ready" | "installing" | "complete" | "failed";
  progress?: number;
  notes: string;
  requiresRestart: boolean;
}

export interface UpdateCenterProps extends SurfaceOptions {
  release: UpdateRelease;
  channelAction: string;
  checkIntent: SemanticIntent;
  downloadIntent?: SemanticIntent;
  installIntent?: SemanticIntent;
  restartIntent?: SemanticIntent;
}

export function UpdateCenter({ release, channelAction, checkIntent, downloadIntent, installIntent, restartIntent, ...surface }: UpdateCenterProps): ReactNode {
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "narrow"} actions={[checkIntent]}>
      <Stack gap="sm">
        <Row align="spaceBetween"><Text value={`${release.currentVersion} → ${release.targetVersion}`} role="heading" weight="bold" /><StatusBadge status={release.status} /></Row>
        <KeyValue entries={{ channel: release.channel, requiresRestart: release.requiresRestart, channelAction }} />
        <Text value={release.notes} />
        {release.progress === undefined ? null : <Progress value={release.progress} maximum={100} label={`${release.progress}%`} accessibleLabel={`Update progress ${release.progress}%`} />}
        <Row align="end" gap="xs">
          {downloadIntent === undefined ? null : <IntentButton intent={downloadIntent} />}
          {installIntent === undefined ? null : <IntentButton intent={installIntent} />}
          {restartIntent === undefined ? null : <IntentButton intent={restartIntent} />}
        </Row>
      </Stack>
    </SurfaceFrame>
  );
}

export interface RecoveryPoint {
  id: string;
  label: string;
  createdAt: string;
  reason: string;
  resourceCount: number;
  status: "available" | "validating" | "invalid";
}

export interface RecoveryCenterProps extends SurfaceOptions {
  incident: string;
  safeMode: "disabled" | "enabled";
  safeModeAction: string;
  points: readonly RecoveryPoint[];
  selectedPointId?: string;
  selectAction: string;
  recoverIntent: SemanticIntent<{ recoveryPointId: string }>;
  exportIntent?: SemanticIntent<{ recoveryPointId: string }>;
}

export function RecoveryCenter({ incident, safeMode, safeModeAction, points, selectedPointId, selectAction, recoverIntent, exportIntent, ...surface }: RecoveryCenterProps): ReactNode {
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "narrow"}>
      <Stack gap="sm">
        <Alert tone="warning" title="Recovery mode" message={incident} />
        <Checkbox name="safeMode" checked={safeMode === "enabled"} changeAction={safeModeAction} accessibleLabel="Start in safe mode with third-party components disabled" />
        <VirtualizedCollection id={`${surface.id}-points`} label={`${points.length} recovery points`} items={points} selectedKey={selectedPointId} emptyMessage="No recovery points available" itemKey={(point) => point.id}>
          {points.slice(0, 30).map((point) => (
            <Stack key={point.id} gap="xs">
              <Row align="spaceBetween"><Text value={point.label} weight="bold" /><StatusBadge status={point.status} /></Row>
              <Text value={`${point.createdAt} · ${point.reason} · ${point.resourceCount} resources`} role="caption" />
              <Row align="end" gap="xs">
                <Button action={selectAction} label="Inspect" payload={toUiJson({ recoveryPointId: point.id })} accessibleLabel={`Inspect ${point.label}`} />
                {exportIntent === undefined ? null : <IntentButton intent={{ ...exportIntent, payload: { recoveryPointId: point.id } }} />}
                <IntentButton intent={{ ...recoverIntent, payload: { recoveryPointId: point.id } }} />
              </Row>
            </Stack>
          ))}
        </VirtualizedCollection>
      </Stack>
    </SurfaceFrame>
  );
}

export interface NotificationItem {
  id: string;
  title: string;
  message: string;
  tone: "neutral" | "info" | "positive" | "warning" | "critical";
  createdAt: string;
  read: boolean;
  source?: string;
  action?: SemanticIntent<{ notificationId: string }>;
}

export interface NotificationCenterProps extends SurfaceOptions {
  notifications: readonly NotificationItem[];
  selectedNotificationId?: string;
  selectAction: string;
  markReadAction: string;
  dismissAction: string;
  markAllReadIntent?: SemanticIntent;
}

export function NotificationCenter({ notifications, selectedNotificationId, selectAction, markReadAction, dismissAction, markAllReadIntent, ...surface }: NotificationCenterProps): ReactNode {
  const unread = notifications.filter((notification) => !notification.read).length;
  return (
    <SurfaceFrame {...surface} actions={markAllReadIntent === undefined ? [] : [markAllReadIntent]} description={surface.description ?? `${unread} unread`}>
      <VirtualizedCollection id={`${surface.id}-notifications`} label={`${notifications.length} notifications, ${unread} unread`} items={notifications} selectedKey={selectedNotificationId} emptyMessage="No notifications" itemKey={(notification) => notification.id}>
        {notifications.slice(0, 50).map((notification) => (
          <Toast key={notification.id} tone={notification.tone} title={notification.title} message={notification.message} dismissAction={dismissAction} accessibleLabel={`${notification.read ? "Read" : "Unread"} notification: ${notification.title}. ${notification.message}`}>
            <Stack gap="xs">
              <Text value={`${notification.createdAt}${notification.source === undefined ? "" : ` · ${notification.source}`}`} role="caption" tone="muted" />
              <Row align="end" gap="xs">
                <Button action={selectAction} label="Open" payload={toUiJson({ notificationId: notification.id })} accessibleLabel={`Open ${notification.title}`} />
                {notification.read ? null : <Button action={markReadAction} label="Mark read" payload={toUiJson({ notificationId: notification.id })} accessibleLabel={`Mark ${notification.title} as read`} />}
                {notification.action === undefined ? null : <IntentButton intent={{ ...notification.action, payload: { notificationId: notification.id } }} />}
              </Row>
            </Stack>
          </Toast>
        ))}
      </VirtualizedCollection>
    </SurfaceFrame>
  );
}
