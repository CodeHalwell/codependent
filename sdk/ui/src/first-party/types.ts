import type { ReactNode } from "react";
import type { Tone } from "../primitives.js";
import type { UiJsonValue } from "../protocol.js";

export type SurfaceDensity = "compact" | "comfortable" | "spacious";
export type SurfaceWidth = "narrow" | "standard" | "wide" | "full";

export type SurfaceState =
  | { phase: "ready" }
  | { phase: "loading"; label?: string }
  | { phase: "empty"; title: string; message?: string; recoveryAction?: SemanticIntent }
  | { phase: "error"; title: string; message: string; recoveryAction?: SemanticIntent }
  | { phase: "streaming"; label: string; completed?: number; total?: number };

export interface SemanticIntent<TPayload = unknown> {
  /** A namespaced command identifier resolved and authorized by the host. */
  action: string;
  label: string;
  payload?: TPayload;
  shortcut?: string;
  tone?: Tone;
  disabledReason?: string;
}

export interface SurfaceOptions {
  id: string;
  title: string;
  description?: string | undefined;
  density?: SurfaceDensity | undefined;
  width?: SurfaceWidth | undefined;
  state?: SurfaceState | undefined;
  actions?: readonly SemanticIntent[] | undefined;
}

export interface SurfaceProps extends SurfaceOptions {
  children?: ReactNode;
}

export interface SelectionModel {
  selectedId?: string | undefined;
  selectAction: string;
}

export interface PageWindow {
  offset: number;
  limit: number;
  total: number;
}

/** Convert plain application data into protocol JSON, rejecting unsafe values. */
export function toUiJson(input: unknown): UiJsonValue {
  const seen = new WeakSet<object>();
  const visit = (value: unknown, path: string): UiJsonValue => {
    if (value === null || typeof value === "string" || typeof value === "boolean") return value;
    if (typeof value === "number") {
      if (!Number.isFinite(value)) throw new TypeError(`${path} contains a non-finite number`);
      return value;
    }
    if (Array.isArray(value)) return value.map((entry, index) => visit(entry, `${path}[${index}]`));
    if (typeof value !== "object") throw new TypeError(`${path} is not JSON serializable`);
    if (seen.has(value)) throw new TypeError(`${path} contains a cycle`);
    seen.add(value);
    const output: Record<string, UiJsonValue> = {};
    for (const [key, entry] of Object.entries(value)) {
      if (entry !== undefined) output[key] = visit(entry, `${path}.${key}`);
    }
    seen.delete(value);
    return output;
  };
  return visit(input, "value");
}

export function intentPayload(intent: SemanticIntent): UiJsonValue | undefined {
  return intent.payload === undefined ? undefined : toUiJson(intent.payload);
}

export function statusTone(status: string): Tone {
  switch (status) {
    case "complete":
    case "completed":
    case "connected":
    case "healthy":
    case "passed":
    case "ready":
    case "succeeded":
      return "positive";
    case "blocked":
    case "critical":
    case "denied":
    case "error":
    case "failed":
    case "unhealthy":
      return "critical";
    case "cancelled":
    case "disconnected":
    case "idle":
    case "skipped":
      return "muted";
    case "pending":
    case "queued":
    case "warning":
    case "waiting":
      return "warning";
    case "running":
    case "streaming":
      return "info";
    default:
      return "neutral";
  }
}
