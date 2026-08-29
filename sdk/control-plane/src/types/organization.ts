import type { UUID } from "./common.js";
import type { ControlPlaneRole } from "./rbac.js";

export type PublicationClass =
  | "private-local"
  | "metadata-shared"
  | "content-shared"
  | "organization-knowledge"
  | "public-marketplace"
  | "unknown";

export type DataClassification = "public" | "internal" | "confidential" | "secret" | "unknown";

export interface OrganizationPolicy {
  maxPublicationClass: PublicationClass;
  maxClassification: DataClassification;
  dataResidency: string | null;
  retentionDays: number | null;
  policyVersion: number;
}

export interface Organization {
  id: UUID;
  slug: string;
  displayName: string;
  maxPublicationClass: PublicationClass;
  maxClassification: DataClassification;
  dataResidency: string | null;
  retentionDays: number | null;
  policyVersion: number;
  createdAt: string;
}

export interface CreateOrganizationRequest {
  slug: string;
  displayName: string;
  maxPublicationClass?: PublicationClass | undefined;
  maxClassification?: DataClassification | undefined;
  dataResidency?: string | undefined;
  retentionDays?: number | undefined;
}

export interface UpdateOrganizationPolicyRequest {
  displayName?: string | undefined;
  maxPublicationClass?: PublicationClass | undefined;
  maxClassification?: DataClassification | undefined;
  dataResidency?: string | null | undefined;
  retentionDays?: number | null | undefined;
}

/** The only membership mutation currently exposed by the Axum router. */
export interface AddOrganizationMemberRequest {
  userId: UUID;
  role: ControlPlaneRole;
}

export interface AddOrganizationMemberResponse {
  status: "added";
}
