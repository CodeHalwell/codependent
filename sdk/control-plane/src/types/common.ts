export type UUID = string;

export interface PageCursor {
  cursor: string;
  hasMore: boolean;
}

export interface PaginationParams {
  limit?: number | undefined;
  cursor?: string | undefined;
  direction?: "forward" | "backward" | undefined;
}

export interface PaginatedResult<T> {
  items: T[];
  cursor: string | null;
  hasMore: boolean;
  total?: number | undefined;
}

export interface RequestOptions {
  headers?: Record<string, string> | undefined;
  signal?: AbortSignal | undefined;
}

export interface IdempotentRequestOptions extends RequestOptions {
  idempotencyKey?: string | undefined;
}

export interface ApiErrorResponse {
  type?: string | undefined;
  resource?: string | undefined;
  message: string;
  code?: string | undefined;
  detail?: Record<string, unknown> | undefined;
}
