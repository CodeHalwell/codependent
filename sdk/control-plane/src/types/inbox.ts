import type { PaginatedResult, PaginationParams, UUID } from "./common.js";

export type InboxEntryKind = "notification" | "approval_request" | "alert" | "system";

export type InboxEntryState = "unread" | "read" | "acted" | "dismissed";

export interface ControlPlaneInboxEntry {
  id: UUID;
  organizationId: UUID;
  userId: UUID | null;
  kind: InboxEntryKind;
  state: InboxEntryState;
  title: string;
  body: string;
  sourceKind: "daemon" | "runner" | "system";
  sourceId: string;
  actionUrl?: string | undefined;
  createdAt: string;
  readAt: string | null;
  actedAt: string | null;
  detail?: Record<string, unknown> | undefined;
}

export interface InboxMutationRequest {
  state: InboxEntryState;
}

export interface InboxListQuery extends PaginationParams {
  state?: InboxEntryState | "active" | undefined;
  kind?: InboxEntryKind | undefined;
}

export type InboxPage = PaginatedResult<ControlPlaneInboxEntry>;
