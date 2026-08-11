/** @jsxImportSource react */
import type { ReactNode } from "react";
import {
  Chart,
  KeyValue,
  LogViewer,
  Row,
  Select,
  Sparkline,
  Stack,
  Text,
  TextInput,
  TraceView,
  Tree,
} from "../react/primitives.js";
import { StatusBadge, SurfaceFrame, VirtualizedCollection } from "./foundation.js";
import { toUiJson, type SurfaceOptions } from "./types.js";

export interface TraceSpan {
  id: string;
  parentId?: string;
  name: string;
  service: string;
  status: "running" | "succeeded" | "failed" | "cancelled";
  startedAt: string;
  durationMs: number;
  attributes?: Readonly<Record<string, string | number | boolean>>;
}

export interface TraceExplorerProps extends SurfaceOptions {
  traceId: string;
  spans: readonly TraceSpan[];
  selectedSpanId?: string;
  selectAction: string;
}

export function TraceExplorer({ traceId, spans, selectedSpanId, selectAction, ...surface }: TraceExplorerProps): ReactNode {
  const selected = spans.find((span) => span.id === selectedSpanId);
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "wide"}>
      <TraceView resourceId={traceId} status={spans.some((span) => span.status === "failed") ? "failed" : "healthy"} data={toUiJson({ traceId, spans })} accessibleLabel={`Trace ${traceId} with ${spans.length} spans`}>
        <Stack gap="sm">
          <Tree
            items={spans.map((span) => toUiJson(span))}
            virtualized
            emptyMessage="No trace spans"
            accessibleLabel="Trace span hierarchy"
            description={`Selection intent: ${selectAction}`}
            {...(selectedSpanId === undefined ? {} : { selectedKey: selectedSpanId })}
          />
          {selected === undefined ? null : (
            <Stack gap="xs" accessibleLabel={`Selected span ${selected.name}`}>
              <Row align="spaceBetween"><Text value={selected.name} weight="bold" /><StatusBadge status={selected.status} /></Row>
              <KeyValue entries={{ service: selected.service, startedAt: selected.startedAt, durationMs: selected.durationMs, attributes: toUiJson(selected.attributes ?? {}) }} />
            </Stack>
          )}
        </Stack>
      </TraceView>
    </SurfaceFrame>
  );
}

export interface LogEntry {
  sequence: number;
  timestamp: string;
  level: "trace" | "debug" | "info" | "warning" | "error";
  source: string;
  message: string;
  traceId?: string;
  fields?: Readonly<Record<string, unknown>>;
}

export interface LogsExplorerProps extends SurfaceOptions {
  entries: readonly LogEntry[];
  query: string;
  queryAction: string;
  level: "all" | LogEntry["level"];
  levelAction: string;
}

export function LogsExplorer({ entries, query, queryAction, level, levelAction, ...surface }: LogsExplorerProps): ReactNode {
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "full"}>
      <Stack gap="sm">
        <Row gap="sm">
          <TextInput name="logQuery" value={query} changeAction={queryAction} placeholder="Filter logs" accessibleLabel="Filter logs" />
          <Select name="logLevel" value={level} changeAction={levelAction} options={["all", "trace", "debug", "info", "warning", "error"].map((value) => ({ value, label: value }))} accessibleLabel="Minimum log level" />
        </Row>
        <LogViewer
          lines={entries.map((entry) => toUiJson(entry))}
          level={level}
          filter={query}
          accessibleLabel={`${entries.length} log entries, ${level} level filter`}
          description="Logs are virtualized by the host. Use Page Up and Page Down to navigate."
        />
      </Stack>
    </SurfaceFrame>
  );
}

export interface MetricSeries {
  id: string;
  label: string;
  unit: string;
  status: "healthy" | "warning" | "critical";
  points: readonly { timestamp: string; value: number }[];
  current: number;
  threshold?: number;
}

export interface MetricsDashboardProps extends SurfaceOptions {
  metrics: readonly MetricSeries[];
  period: string;
  periodAction: string;
}

export function MetricsDashboard({ metrics, period, periodAction, ...surface }: MetricsDashboardProps): ReactNode {
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "wide"}>
      <Stack gap="sm">
        <Select name="metricsPeriod" value={period} changeAction={periodAction} options={["15m", "1h", "6h", "24h", "7d"].map((value) => ({ value, label: value }))} accessibleLabel="Metrics time period" />
        <VirtualizedCollection id={`${surface.id}-metrics`} label={`${metrics.length} metrics`} items={metrics} emptyMessage="No metrics available" itemKey={(metric) => metric.id}>
          {metrics.slice(0, 30).map((metric) => (
            <Stack key={metric.id} gap="xs" accessibleLabel={`${metric.label}: ${metric.current} ${metric.unit}, ${metric.status}`}>
              <Row align="spaceBetween"><Text value={metric.label} weight="medium" /><Row gap="xs"><Text value={`${metric.current} ${metric.unit}`} role="status" /><StatusBadge status={metric.status} /></Row></Row>
              <Chart data={metric.points.map((point) => toUiJson(point))} chartType="line" xKey="timestamp" yKey="value" accessibleLabel={`${metric.label} over ${period}`} fallback={{ kind: "element", type: "Sparkline", props: { values: metric.points.map((point) => point.value), accessibleLabel: `${metric.label} trend` }, children: [] }} />
              <Sparkline values={metric.points.map((point) => point.value)} tone={metric.status === "critical" ? "critical" : metric.status === "warning" ? "warning" : "positive"} hidden accessibleLabel={`${metric.label} compact trend`} />
            </Stack>
          ))}
        </VirtualizedCollection>
      </Stack>
    </SurfaceFrame>
  );
}
