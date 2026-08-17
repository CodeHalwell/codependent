import type { UUID } from "./common.js";

export interface Team {
  id: UUID;
  organizationId: UUID;
  slug: string;
  displayName: string;
  createdAt: string;
  memberCount?: number | undefined;
}

export interface TeamMember {
  teamId: UUID;
  userId: UUID;
  userDisplayName?: string | undefined;
  userEmail?: string | null | undefined;
  joinedAt?: string | undefined;
}

export interface CreateTeamRequest {
  slug: string;
  displayName: string;
}

export interface AddTeamMemberRequest {
  userId: UUID;
}
