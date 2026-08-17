import type { UUID } from "./common.js";

export type ControlPlaneRole =
  | "observer"
  | "contributor"
  | "approver"
  | "maintainer"
  | "organization-admin";

export interface ActionScope {
  actions: string[];
  repositories?: UUID[] | undefined;
  maxRiskLevel?: "low" | "medium" | "high" | "critical" | undefined;
}

export interface RoleGrant {
  id: UUID;
  organizationId: UUID;
  userId: UUID | null;
  teamId: UUID | null;
  repositoryId: UUID | null;
  role: ControlPlaneRole;
  actionScope: ActionScope | null;
  grantedBy: UUID;
  grantedAt: string;
  expiresAt: string | null;
  revokedAt: string | null;
  userDisplayName?: string | undefined;
  teamDisplayName?: string | undefined;
}

export interface GrantRoleRequest {
  userId?: UUID | undefined;
  teamId?: UUID | undefined;
  repositoryId?: UUID | undefined;
  role: ControlPlaneRole;
  actionScope?: ActionScope | undefined;
  expiresInDays?: number | undefined;
}
