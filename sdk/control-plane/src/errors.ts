import type { ApiErrorResponse } from "./types/common.js";

export class ControlPlaneError extends Error {
  readonly status: number;
  readonly type: string;
  readonly resource: string | undefined;
  readonly code: string | undefined;
  readonly detail: Record<string, unknown> | undefined;

  constructor(
    message: string,
    status: number,
    type: string = "control_plane_error",
    options?: {
      resource?: string | undefined;
      code?: string | undefined;
      detail?: Record<string, unknown> | undefined;
      cause?: unknown;
    }
  ) {
    super(message, { cause: options?.cause });
    this.name = "ControlPlaneError";
    this.status = status;
    this.type = type;
    this.resource = options?.resource;
    this.code = options?.code;
    this.detail = options?.detail;
  }
}

/**
 * Unified 404 Not Found error.
 * Used for both absent resources and unauthorized resources to prevent existence disclosure (design §5.3).
 */
export class NotFoundError extends ControlPlaneError {
  constructor(resource: string = "resource", message?: string | undefined) {
    super(message ?? `no such ${resource}`, 404, "not_found", { resource });
    this.name = "NotFoundError";
  }
}

/**
 * 401 Unauthorized error.
 * Reserved for "no valid credential presented" (carries no resource information).
 */
export class UnauthorizedError extends ControlPlaneError {
  constructor(message: string = "no valid credential presented") {
    super(message, 401, "unauthorized");
    this.name = "UnauthorizedError";
  }
}

/**
 * 403 Forbidden error.
 */
export class ForbiddenError extends ControlPlaneError {
  constructor(message: string = "forbidden", resource?: string | undefined) {
    super(message, 403, "forbidden", { resource });
    this.name = "ForbiddenError";
  }
}

/**
 * 409 Conflict error.
 */
export class ConflictError extends ControlPlaneError {
  constructor(message: string = "resource conflict", resource?: string | undefined) {
    super(message, 409, "conflict", { resource });
    this.name = "ConflictError";
  }
}

/**
 * 400 Bad Request / Validation error.
 */
export class ValidationError extends ControlPlaneError {
  constructor(message: string = "invalid request parameters", detail?: Record<string, unknown> | undefined) {
    super(message, 400, "validation_error", { detail });
    this.name = "ValidationError";
  }
}

/**
 * 422 Policy Violation error.
 */
export class PolicyViolationError extends ControlPlaneError {
  constructor(message: string = "policy ceiling exceeded", detail?: Record<string, unknown> | undefined) {
    super(message, 422, "policy_violation", { detail });
    this.name = "PolicyViolationError";
  }
}

/**
 * Network / Connectivity error.
 */
export class NetworkError extends ControlPlaneError {
  constructor(message: string = "failed to connect to control plane", cause?: unknown) {
    super(message, 0, "network_error", { cause });
    this.name = "NetworkError";
  }
}

/**
 * The installed control-plane server does not expose this capability yet.
 *
 * This is deliberately distinct from a server-side 404. A 404 can be part of
 * the control plane's non-disclosure policy, while this error means the SDK
 * knows that no route exists in the current Axum router and therefore did not
 * make a network request at all.
 */
export class UnsupportedControlPlaneCapabilityError extends ControlPlaneError {
  readonly capability: string;

  constructor(capability: string) {
    super(
      `control-plane capability is not implemented by the server: ${capability}`,
      501,
      "not_implemented",
      { resource: capability },
    );
    this.name = "UnsupportedControlPlaneCapabilityError";
    this.capability = capability;
  }
}

export function parseApiError(status: number, data: unknown): ControlPlaneError {
  let type = "error";
  let message = `Request failed with status ${status}`;
  let resource: string | undefined;
  let code: string | undefined;
  let detail: Record<string, unknown> | undefined;

  if (typeof data === "object" && data !== null) {
    const errorObj = data as Partial<ApiErrorResponse>;
    if (typeof errorObj.type === "string") type = errorObj.type;
    if (typeof errorObj.message === "string") message = errorObj.message;
    if (typeof errorObj.resource === "string") resource = errorObj.resource;
    if (typeof errorObj.code === "string") code = errorObj.code;
    if (typeof errorObj.detail === "object" && errorObj.detail !== null) detail = errorObj.detail as Record<string, unknown>;
  }

  switch (status) {
    case 404:
      return new NotFoundError(resource, message);
    case 401:
      return new UnauthorizedError(message);
    case 403:
      return new ForbiddenError(message, resource);
    case 409:
      return new ConflictError(message, resource);
    case 400:
      return new ValidationError(message, detail);
    case 422:
      return new PolicyViolationError(message, detail);
    default:
      return new ControlPlaneError(message, status, type, { resource, code, detail });
  }
}
