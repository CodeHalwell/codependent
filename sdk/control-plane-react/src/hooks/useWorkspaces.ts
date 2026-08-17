import { useCallback, useState } from "react";
import { useControlPlaneContext } from "./useControlPlaneContext.js";
import type { CreateTeamRequest, Team, TeamMember } from "@codypendent/control-plane";

export interface UseWorkspacesResult {
  teams: Team[];
  activeTeam: Team | null;
  activeTeamId: string | null;
  setActiveTeamId: (id: string | null) => void;
  createTeam: (data: CreateTeamRequest) => Promise<Team>;
  listTeamMembers: (teamId: string) => Promise<TeamMember[]>;
  addTeamMember: (teamId: string, userId: string) => Promise<TeamMember>;
  removeTeamMember: (teamId: string, userId: string) => Promise<void>;
  refreshTeams: () => Promise<void>;
  isLoading: boolean;
  error: Error | null;
}

export function useWorkspaces(): UseWorkspacesResult {
  const {
    client,
    teams,
    activeTeam,
    activeTeamId,
    activeOrganizationId,
    setActiveTeamId,
    refreshTeams,
    isLoading: contextLoading,
    error: contextError,
  } = useControlPlaneContext();

  const [isMutating, setIsMutating] = useState<boolean>(false);
  const [mutationError, setMutationError] = useState<Error | null>(null);

  const createTeam = useCallback(
    async (data: CreateTeamRequest) => {
      if (!activeOrganizationId) {
        throw new Error("No active organization selected");
      }
      setIsMutating(true);
      setMutationError(null);
      try {
        const team = await client.createTeam(activeOrganizationId, data);
        await refreshTeams();
        setActiveTeamId(team.id);
        return team;
      } catch (err) {
        setMutationError(err as Error);
        throw err;
      } finally {
        setIsMutating(false);
      }
    },
    [client, activeOrganizationId, refreshTeams, setActiveTeamId]
  );

  const listTeamMembers = useCallback(
    async (teamId: string) => {
      if (!activeOrganizationId) return [];
      return client.listTeamMembers(activeOrganizationId, teamId);
    },
    [client, activeOrganizationId]
  );

  const addTeamMember = useCallback(
    async (teamId: string, userId: string) => {
      if (!activeOrganizationId) {
        throw new Error("No active organization selected");
      }
      const member = await client.addTeamMember(activeOrganizationId, teamId, { userId });
      await refreshTeams();
      return member;
    },
    [client, activeOrganizationId, refreshTeams]
  );

  const removeTeamMember = useCallback(
    async (teamId: string, userId: string) => {
      if (!activeOrganizationId) {
        throw new Error("No active organization selected");
      }
      await client.removeTeamMember(activeOrganizationId, teamId, userId);
      await refreshTeams();
    },
    [client, activeOrganizationId, refreshTeams]
  );

  return {
    teams,
    activeTeam,
    activeTeamId,
    setActiveTeamId,
    createTeam,
    listTeamMembers,
    addTeamMember,
    removeTeamMember,
    refreshTeams,
    isLoading: contextLoading || isMutating,
    error: mutationError || contextError,
  };
}

export const useTeams = useWorkspaces;
