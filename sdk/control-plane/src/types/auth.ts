import type { UUID } from "./common.js";
import type { PublicationClass } from "./organization.js";

export type UserState = "active" | "suspended" | "deleted";

export interface User {
  id: UUID;
  displayName: string;
  primaryEmail: string | null;
  state: UserState;
  createdAt: string;
  updatedAt: string;
}

export type IdentityProvider = "github" | "oidc";

export interface UserIdentity {
  id: UUID;
  userId: UUID;
  provider: IdentityProvider;
  issuer: string;
  subject: string;
  emailAtLink: string | null;
  linkedAt: string;
  linkAuditId: UUID;
}

export interface AuthTokens {
  accessToken: string;
  refreshToken: string;
  tokenType: "Bearer";
  expiresIn: number;
}

export interface AuthSession {
  user: User;
  identities: UserIdentity[];
  tokens: AuthTokens;
}

export interface LoginResponse {
  authUrl: string;
  state: string;
}

export interface OAuthCallbackRequest {
  code: string;
  state: string;
}

export interface RefreshTokenRequest {
  refreshToken: string;
}

export interface LinkIdentityRequest {
  code: string;
  state: string;
}

export interface PairingChallenge {
  code: string;
  organizationId: UUID;
  requestedScope: {
    maxPublicationClass: PublicationClass;
    acceptsRemoteApprovals: boolean;
    acceptsRunnerDispatch: boolean;
  };
  expiresAt: string;
}

export interface CreatePairingChallengeRequest {
  organizationId: UUID;
  maxPublicationClass?: PublicationClass | undefined;
  acceptsRemoteApprovals?: boolean | undefined;
  acceptsRunnerDispatch?: boolean | undefined;
}

export interface ApiKey {
  id: UUID;
  organizationId: UUID;
  name: string;
  keyPrefix: string;
  role: string;
  createdAt: string;
  lastUsedAt: string | null;
  expiresAt: string | null;
  revokedAt: string | null;
}

export interface CreateApiKeyRequest {
  organizationId: UUID;
  name: string;
  role: string;
  repositoryId?: UUID | undefined;
  expiresInDays?: number | undefined;
}

export interface ApiKeyCreatedResponse {
  apiKey: ApiKey;
  token: string;
}

export type WorkloadPurpose = "sync" | "pairing" | "runner-job";

export interface WorkloadCredential {
  id: UUID;
  daemonId: UUID | null;
  audience: string;
  purpose: WorkloadPurpose;
  issuedAt: string;
  expiresAt: string;
  revokedAt: string | null;
}
