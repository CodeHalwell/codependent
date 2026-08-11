/** @jsxImportSource react */
import { createElement, type ReactNode } from "react";
import type { UiJsonValue, UiNode } from "../protocol.js";
import {
  Alert,
  Badge,
  Box,
  Button,
  EmptyState,
  Progress,
  Row,
  Spinner,
  Stack,
  Text,
  VirtualList,
} from "../react/primitives.js";
import {
  intentPayload,
  statusTone,
  toUiJson,
  type SemanticIntent,
  type SurfaceDensity,
  type SurfaceProps,
  type SurfaceState,
  type SurfaceWidth,
} from "./types.js";

const WIDTHS: Record<SurfaceWidth, string> = {
  narrow: "48ch",
  standard: "80ch",
  wide: "120ch",
  full: "100%",
};

const GAPS: Record<SurfaceDensity, "xs" | "sm" | "md"> = {
  compact: "xs",
  comfortable: "sm",
  spacious: "md",
};

export function textFallback(title: string, detail: string): UiNode {
  return {
    kind: "element",
    type: "Stack",
    props: { gap: "xs", accessibleLabel: title },
    children: [
      { kind: "element", type: "Text", props: { value: title, role: "heading", weight: "bold" }, children: [] },
      { kind: "element", type: "Text", props: { value: detail, role: "text" }, children: [] },
    ],
  };
}

export interface IntentButtonProps {
  id?: string;
  intent: SemanticIntent;
  confirmationState?: "not-required" | "confirmed" | "unconfirmed";
}

export function IntentButton({ id, intent, confirmationState = "not-required" }: IntentButtonProps): ReactNode {
  const disabledReason = confirmationState === "unconfirmed"
    ? "Confirm the reviewed scope before continuing"
    : intent.disabledReason;
  const payload = intentPayload(intent);
  return (
    <Button
      action={intent.action}
      label={intent.label}
      accessibleLabel={intent.label}
      disabled={disabledReason !== undefined}
      {...(id === undefined ? {} : { id })}
      {...(disabledReason === undefined ? {} : { description: disabledReason })}
      {...(payload === undefined ? {} : { payload })}
      {...(intent.shortcut === undefined ? {} : { shortcut: intent.shortcut })}
      {...(intent.tone === undefined ? {} : { tone: intent.tone })}
    />
  );
}

export interface StatusBadgeProps {
  status: string;
  label?: string;
}

export function StatusBadge({ status, label = status }: StatusBadgeProps): ReactNode {
  return <Badge title={label} message={status} tone={statusTone(status)} accessibleLabel={`${label}: ${status}`} />;
}

function StateContent({ state }: { state: Exclude<SurfaceState, { phase: "ready" }> }): ReactNode {
  switch (state.phase) {
    case "loading":
      return <Spinner indeterminate label={state.label ?? "Loading"} accessibleLabel={state.label ?? "Loading"} />;
    case "empty":
      return (
        <Stack gap="sm">
          <EmptyState title={state.title} accessibleLabel={state.title} {...(state.message === undefined ? {} : { message: state.message })} />
          {state.recoveryAction === undefined ? null : <IntentButton intent={state.recoveryAction} />}
        </Stack>
      );
    case "error":
      return (
        <Stack gap="sm">
          <Alert tone="critical" title={state.title} message={state.message} accessibleLabel={`${state.title}: ${state.message}`} />
          {state.recoveryAction === undefined ? null : <IntentButton intent={state.recoveryAction} />}
        </Stack>
      );
    case "streaming": {
      const determinate = state.total !== undefined && state.completed !== undefined;
      return (
        <Progress
          indeterminate={!determinate}
          label={state.label}
          accessibleLabel={state.label}
          {...(determinate ? { value: state.completed, maximum: state.total } : {})}
        />
      );
    }
  }
}

export function SurfaceFrame({
  id,
  title,
  description,
  density = "comfortable",
  width = "standard",
  state = { phase: "ready" },
  actions = [],
  children,
}: SurfaceProps): ReactNode {
  const gap = GAPS[density];
  return (
    <Box
      id={id}
      border="rounded"
      padding={gap}
      width={WIDTHS[width]}
      accessibleLabel={title}
      {...(description === undefined ? {} : { description })}
    >
      <Stack gap={gap}>
        <Row align="spaceBetween" gap={gap}>
          <Stack gap="xs">
            <Text value={title} role="heading" weight="bold" accessibleLabel={title} />
            {description === undefined ? null : <Text value={description} role="caption" tone="muted" />}
          </Stack>
          {actions.length === 0 ? null : (
            <Row gap="xs" accessibleLabel={`${title} actions`}>
              {actions.map((intent, index) => <IntentButton key={`${intent.action}-${index}`} intent={intent} />)}
            </Row>
          )}
        </Row>
        {state.phase === "ready" ? children : (
          <Stack gap={gap}>
            <StateContent state={state} />
            {state.phase === "streaming" ? children : null}
          </Stack>
        )}
      </Stack>
    </Box>
  );
}

export const Surface = {
  Frame: SurfaceFrame,
  Ready(props: Omit<SurfaceProps, "state">): ReactNode { return <SurfaceFrame {...props} state={{ phase: "ready" }} />; },
  Loading(props: Omit<SurfaceProps, "state"> & { label?: string }): ReactNode {
    const { label, ...frame } = props;
    return <SurfaceFrame {...frame} state={{ phase: "loading", ...(label === undefined ? {} : { label }) }} />;
  },
  Empty(props: Omit<SurfaceProps, "state"> & { emptyTitle: string; emptyMessage?: string }): ReactNode {
    const { emptyTitle, emptyMessage, ...frame } = props;
    return <SurfaceFrame {...frame} state={{ phase: "empty", title: emptyTitle, ...(emptyMessage === undefined ? {} : { message: emptyMessage }) }} />;
  },
  Error(props: Omit<SurfaceProps, "state"> & { errorTitle: string; errorMessage: string }): ReactNode {
    const { errorTitle, errorMessage, ...frame } = props;
    return <SurfaceFrame {...frame} state={{ phase: "error", title: errorTitle, message: errorMessage }} />;
  },
  Streaming(props: Omit<SurfaceProps, "state"> & { streamLabel: string; completed?: number; total?: number }): ReactNode {
    const { streamLabel, completed, total, ...frame } = props;
    return <SurfaceFrame {...frame} state={{ phase: "streaming", label: streamLabel, ...(completed === undefined ? {} : { completed }), ...(total === undefined ? {} : { total }) }} />;
  },
} as const;

export interface VirtualizedCollectionProps<TItem> {
  id: string;
  label: string;
  items: readonly TItem[];
  selectedKey?: string | undefined;
  emptyMessage: string;
  itemKey(item: TItem): string;
  children?: ReactNode;
}

export function VirtualizedCollection<TItem>({
  id,
  label,
  items,
  selectedKey,
  emptyMessage,
  itemKey,
  children,
}: VirtualizedCollectionProps<TItem>): ReactNode {
  const semanticItems: UiJsonValue[] = items.map((item) => {
    const value = toUiJson(item);
    return value !== null && typeof value === "object" && !Array.isArray(value)
      ? { ...value, key: itemKey(item) }
      : { key: itemKey(item), value };
  });
  return (
    <VirtualList
      id={id}
      items={semanticItems}
      emptyMessage={emptyMessage}
      virtualized
      accessibleLabel={label}
      {...(selectedKey === undefined ? {} : { selectedKey })}
    >
      {children}
    </VirtualList>
  );
}

/** Adds governance metadata without granting the component authority. */
export function coreOnlyData(kind: "approval" | "permission" | "secret" | "policy", value: unknown): UiJsonValue {
  return toUiJson({
    governance: "core-only",
    authority: "intent-only",
    kind,
    value,
  });
}

export function keyedChildren<T>(items: readonly T[], render: (item: T, index: number) => ReactNode): ReactNode {
  return createElement(Stack, { gap: "xs" }, items.map(render));
}
