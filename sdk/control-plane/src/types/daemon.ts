import type { UUID } from "./common.js";
import type { PublicationClass } from "./organization.js";

export type DaemonState = "pending" | "active" | "revoked" | "expired";

export interface Daemon {
  id: UUID;
  organizationId: UUID;
  pairedBy: UUID;
  displayName: string;
  consentManifestHash: string;
  maxPublicationClass: PublicationClass;
  acceptsRemoteApprovals: boolean;
  acceptsRunnerDispatch: boolean;
  state: DaemonState;
  pairedAt: string | null;
  revokedAt: string | null;
  lastSeenAt: string | null;
  createdAt: string;
}

export interface PairDaemonRequest {
  challengeCode: string;
  displayName: string;
  consentManifest: string;
  consentManifestHash: string;
}

export interface RevokeDaemonRequest {
  reason?: string | undefined;
}
