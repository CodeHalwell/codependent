/** @jsxImportSource react */
import type { ReactNode } from "react";
import {
  Badge,
  Button,
  Chart,
  CostView,
  KeyValue,
  Progress,
  Row,
  Select,
  Stack,
  Text,
  TextInput,
} from "../react/primitives.js";
import { IntentButton, StatusBadge, SurfaceFrame, VirtualizedCollection } from "./foundation.js";
import { toUiJson, type SemanticIntent, type SurfaceOptions } from "./types.js";

export interface KnowledgeResult {
  id: string;
  title: string;
  excerpt: string;
  source: string;
  kind: "memory" | "document" | "code" | "conversation" | "external";
  score: number;
  updatedAt?: string;
  tags?: readonly string[];
}

export interface MemoryKnowledgeSearchProps extends SurfaceOptions {
  query: string;
  queryAction: string;
  scope: "workspace" | "session" | "global";
  scopeAction: string;
  results: readonly KnowledgeResult[];
  selectedResultId?: string;
  openAction: string;
  forgetIntent?: SemanticIntent<{ resultId: string }>;
}

export function MemoryKnowledgeSearch({ query, queryAction, scope, scopeAction, results, selectedResultId, openAction, forgetIntent, ...surface }: MemoryKnowledgeSearchProps): ReactNode {
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "wide"}>
      <Stack gap="sm">
        <Row gap="sm">
          <TextInput name="knowledgeQuery" value={query} changeAction={queryAction} placeholder="Search memory and knowledge" accessibleLabel="Search memory and knowledge" />
          <Select name="knowledgeScope" value={scope} changeAction={scopeAction} options={["workspace", "session", "global"].map((value) => ({ value, label: value }))} accessibleLabel="Knowledge search scope" />
        </Row>
        <VirtualizedCollection id={`${surface.id}-results`} label={`${results.length} knowledge results`} items={results} selectedKey={selectedResultId} emptyMessage="No knowledge matches this query" itemKey={(result) => result.id}>
          {results.slice(0, 50).map((result) => (
            <Stack key={result.id} gap="xs">
              <Row align="spaceBetween"><Text value={result.title} weight="bold" /><Row gap="xs"><Badge title={result.kind} message={result.kind} /><Text value={`${Math.round(result.score * 100)}%`} role="status" /></Row></Row>
              <Text value={result.excerpt} />
              <Text value={result.source} role="caption" tone="muted" />
              <Row align="end" gap="xs">
                <Button action={openAction} label="Open" payload={toUiJson({ resultId: result.id })} accessibleLabel={`Open ${result.title}`} />
                {forgetIntent === undefined ? null : <IntentButton intent={{ ...forgetIntent, payload: { resultId: result.id } }} />}
              </Row>
            </Stack>
          ))}
        </VirtualizedCollection>
      </Stack>
    </SurfaceFrame>
  );
}

export interface ModelRoute {
  id: string;
  model: string;
  provider: string;
  workload: string;
  priority: number;
  status: "ready" | "degraded" | "unavailable";
  latencyMs?: number;
  pricePerMillionInput?: number;
  pricePerMillionOutput?: number;
  reason?: string;
}

export interface ModelRoutingViewProps extends SurfaceOptions {
  routes: readonly ModelRoute[];
  selectedRouteId?: string;
  selectAction: string;
  policy: string;
  changePolicyAction: string;
  testIntent?: SemanticIntent;
}

export function ModelRoutingView({ routes, selectedRouteId, selectAction, policy, changePolicyAction, testIntent, ...surface }: ModelRoutingViewProps): ReactNode {
  return (
    <SurfaceFrame {...surface} actions={testIntent === undefined ? [] : [testIntent]}>
      <Stack gap="sm">
        <Select name="routingPolicy" value={policy} changeAction={changePolicyAction} options={["quality", "balanced", "cost", "latency", "manual"].map((value) => ({ value, label: value }))} accessibleLabel="Model routing policy" />
        <VirtualizedCollection id={`${surface.id}-routes`} label={`${routes.length} model routes`} items={routes} selectedKey={selectedRouteId} emptyMessage="No model routes configured" itemKey={(route) => route.id}>
          {routes.slice(0, 50).map((route) => (
            <Stack key={route.id} gap="xs">
              <Row align="spaceBetween"><Text value={`${route.model} · ${route.provider}`} weight="bold" /><StatusBadge status={route.status} /></Row>
              <KeyValue entries={{ workload: route.workload, priority: route.priority, ...(route.latencyMs === undefined ? {} : { latencyMs: route.latencyMs }), ...(route.reason === undefined ? {} : { reason: route.reason }) }} />
              <Button action={selectAction} label="Select route" payload={toUiJson({ routeId: route.id })} accessibleLabel={`Select ${route.model} route for ${route.workload}`} />
            </Stack>
          ))}
        </VirtualizedCollection>
      </Stack>
    </SurfaceFrame>
  );
}

export interface UsageBucket { timestamp: string; inputTokens: number; outputTokens: number; cost: number }
export interface QuotaLimit { id: string; label: string; used: number; limit: number; unit: string; resetsAt?: string }

export interface CostQuotaViewProps extends SurfaceOptions {
  period: string;
  totalCost: number;
  currency: string;
  usage: readonly UsageBucket[];
  quotas: readonly QuotaLimit[];
  exportIntent?: SemanticIntent;
}

export function CostQuotaView({ period, totalCost, currency, usage, quotas, exportIntent, ...surface }: CostQuotaViewProps): ReactNode {
  return (
    <SurfaceFrame {...surface} actions={exportIntent === undefined ? [] : [exportIntent]} width={surface.width ?? "wide"}>
      <CostView status="current" data={toUiJson({ period, totalCost, currency, usage, quotas })} accessibleLabel={`Cost ${totalCost} ${currency} for ${period}`}>
        <Stack gap="sm">
          <Text value={`${totalCost.toFixed(2)} ${currency}`} role="heading" weight="bold" />
          <Chart data={usage.map((bucket) => toUiJson(bucket))} chartType="area" xKey="timestamp" yKey="cost" accessibleLabel={`Cost trend for ${period}`} fallback={{ kind: "element", type: "Sparkline", props: { values: usage.map((bucket) => bucket.cost), accessibleLabel: "Cost trend" }, children: [] }} />
          <VirtualizedCollection id={`${surface.id}-quotas`} label={`${quotas.length} quota limits`} items={quotas} emptyMessage="No quotas configured" itemKey={(quota) => quota.id}>
            {quotas.map((quota) => (
              <Stack key={quota.id} gap="xs">
                <Row align="spaceBetween"><Text value={quota.label} /><Text value={`${quota.used} / ${quota.limit} ${quota.unit}`} role="status" /></Row>
                <Progress value={quota.used} maximum={quota.limit} label={quota.label} accessibleLabel={`${quota.label}: ${quota.used} of ${quota.limit} ${quota.unit}`} />
              </Stack>
            ))}
          </VirtualizedCollection>
        </Stack>
      </CostView>
    </SurfaceFrame>
  );
}
