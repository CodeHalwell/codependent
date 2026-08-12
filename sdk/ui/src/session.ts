import type { UiCapabilities, UiJsonValue, UiViewport } from "./protocol.js";

export interface SessionSummary {
  id: string;
  title?: string;
  state: string;
  activeRunId?: string;
  updatedAt?: string;
}

export interface RunView {
  id: string;
  sessionId: string;
  state: string;
  agentMode?: string;
  progress?: number;
  cost?: number;
  startedAt?: string;
  completedAt?: string;
  data?: UiJsonValue;
}

export interface ArtifactView<T extends UiJsonValue = UiJsonValue> {
  id: string;
  mediaType: string;
  schema?: string;
  title?: string;
  revision: number;
  value: T;
}

export interface ArtifactProjectionOptions {
  includeContent?: boolean;
  maxBytes?: number;
  offset?: number;
  length?: number;
  page?: number;
  pageSize?: number;
}

export interface IdeContextView {
  activeFile?: string;
  selection?: UiJsonValue;
  openFiles: readonly string[];
  dirtyBuffers: readonly UiJsonValue[];
  diagnosticsRevision: number;
}

export interface WorkflowNodeView {
  workflowRunId: string;
  nodeId: string;
  state: string;
  attempt: number;
  cost?: UiJsonValue;
  error?: string;
  warnings: readonly string[];
}

export interface WorkflowView {
  workflowRunId: string;
  phase: string;
  nodes: readonly WorkflowNodeView[];
}

/**
 * One artifact posted to a workflow run's blackboard. Unknown fields survive
 * in `extra` rather than being dropped, so a board column added daemon-side
 * reaches a component without an SDK release.
 */
export interface BlackboardItemView {
  id: string;
  workflowRunId: string;
  kind: string;
  payload: UiJsonValue;
  author: UiJsonValue;
  confidence?: number;
  evidence: readonly UiJsonValue[];
  revision: number;
  supersededBy?: string;
  extra: Readonly<Record<string, UiJsonValue>>;
}

export interface BlackboardView {
  workflowRunId: string;
  items: readonly BlackboardItemView[];
}

export interface BlackboardProjectionOptions {
  /** Restrict to one artifact kind (`finding`, `decision`, …). */
  kind?: string;
  /** Include superseded revisions; `false` returns only the live board. */
  includeSuperseded?: boolean;
}

export interface CommandDescriptor<TInput extends UiJsonValue = UiJsonValue, TOutput extends UiJsonValue = UiJsonValue> {
  id: string;
  title: string;
  enabled: boolean;
  disabledReason?: string;
  execute(input: TInput): Promise<TOutput>;
}

export interface ThemeTokens {
  id: string;
  mode: "light" | "dark" | "highContrast" | "monochrome";
  colors: Readonly<Record<string, string>>;
  spacing: Readonly<Record<string, number>>;
}

export interface ExternalProjection<T> {
  getSnapshot(): T;
  subscribe(listener: () => void): () => void;
}

/**
 * The only state boundary components receive. Implementations live in the host;
 * no filesystem, database, process, socket, or secret access is exposed.
 */
export interface UiProjectionStore {
  session(id: string): ExternalProjection<SessionSummary | undefined>;
  run(id: string): ExternalProjection<RunView | undefined>;
  context(sessionId: string): ExternalProjection<IdeContextView | undefined>;
  workflow(id: string): ExternalProjection<WorkflowView | undefined>;
  /** A workflow run's blackboard, read-only. `id` is a workflow run id. */
  blackboard(id: string, options?: BlackboardProjectionOptions): ExternalProjection<BlackboardView | undefined>;
  artifact<T extends UiJsonValue = UiJsonValue>(id: string, options?: ArtifactProjectionOptions): ExternalProjection<ArtifactView<T> | undefined>;
  command<TInput extends UiJsonValue = UiJsonValue, TOutput extends UiJsonValue = UiJsonValue>(id: string): ExternalProjection<CommandDescriptor<TInput, TOutput> | undefined>;
  theme(): ExternalProjection<ThemeTokens>;
  viewport(): ExternalProjection<UiViewport>;
  capabilities(): ExternalProjection<UiCapabilities>;
}

export interface UiCommandActions {
  invoke<TInput extends UiJsonValue = UiJsonValue, TOutput extends UiJsonValue = UiJsonValue>(
    commandId: string,
    input: TInput,
    options?: { signal?: AbortSignal },
  ): Promise<TOutput>;
  cancel(invocationId: string): Promise<void>;
}

export interface UiProviderMeta {
  clientId: string;
  sessionId?: string;
  pluginId?: string;
  hotReloadGeneration: number;
}
