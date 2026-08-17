import type { UUID } from "./common.js";

export type StreamName =
  | "notifications"
  | "approvals"
  | "schedules"
  | "runner-events"
  | "policy"
  | "sessions";

export interface StreamEvent<T = Record<string, unknown>> {
  id: number;
  organizationId: UUID;
  repositoryId: UUID | null;
  stream: StreamName;
  payload: T;
  createdAt: string;
}

export type StreamResumeCursor = string | number;

export interface StreamSubscriptionOptions {
  organizationId: UUID;
  stream?: StreamName | undefined;
  repositoryId?: UUID | undefined;
  cursor?: StreamResumeCursor | undefined;
  onEvent: (event: StreamEvent) => void;
  onError?: ((error: Error) => void) | undefined;
  onConnect?: (() => void) | undefined;
  onDisconnect?: (() => void) | undefined;
}
