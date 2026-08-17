import type { UUID } from "./common.js";
import type { DataClassification, PublicationClass } from "./organization.js";

export interface RepositoryPolicy {
  maxPublicationClass: PublicationClass;
  maxClassification: DataClassification;
  policyVersion: number;
}

export interface Repository {
  id: UUID;
  organizationId: UUID;
  federatedId: string;
  displayName: string;
  maxPublicationClass: PublicationClass;
  maxClassification: DataClassification;
  policyVersion: number;
  createdAt: string;
}

export interface RegisterRepositoryRequest {
  federatedId: string;
  displayName: string;
  maxPublicationClass?: PublicationClass | undefined;
  maxClassification?: DataClassification | undefined;
}

export interface UpdateRepositoryPolicyRequest {
  displayName?: string | undefined;
  maxPublicationClass?: PublicationClass | undefined;
  maxClassification?: DataClassification | undefined;
}
