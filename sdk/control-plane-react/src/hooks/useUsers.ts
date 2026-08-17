import { useCallback, useEffect, useState } from "react";
import { useControlPlaneContext } from "./useControlPlaneContext.js";
import type {
  GrantRoleRequest,
  RoleGrant,
  User,
} from "@codypendent/control-plane";

export interface UseUsersResult {
  members: User[];
  roleGrants: RoleGrant[];
  isLoading: boolean;
  isMutating: boolean;
  error: Error | null;
  inviteUser: (email: string, role: string) => Promise<User>;
  grantRole: (data: GrantRoleRequest) => Promise<RoleGrant>;
  revokeRoleGrant: (grantId: string) => Promise<void>;
  removeUser: (userId: string) => Promise<void>;
  refresh: () => Promise<void>;
}

export function useUsers(): UseUsersResult {
  const { client, activeOrganizationId } = useControlPlaneContext();

  const [members, setMembers] = useState<User[]>([]);
  const [roleGrants, setRoleGrants] = useState<RoleGrant[]>([]);
  const [isLoading, setIsLoading] = useState<boolean>(true);
  const [isMutating, setIsMutating] = useState<boolean>(false);
  const [error, setError] = useState<Error | null>(null);

  const fetchUsersAndGrants = useCallback(async () => {
    if (!activeOrganizationId) {
      setMembers([]);
      setRoleGrants([]);
      setIsLoading(false);
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      const [userList, grantList] = await Promise.all([
        client.listUsers(activeOrganizationId),
        client.listRoleGrants(activeOrganizationId),
      ]);
      setMembers(userList);
      setRoleGrants(grantList);
    } catch (err) {
      setError(err as Error);
    } finally {
      setIsLoading(false);
    }
  }, [client, activeOrganizationId]);

  const inviteUser = useCallback(
    async (email: string, role: string) => {
      if (!activeOrganizationId) {
        throw new Error("No active organization selected");
      }
      setIsMutating(true);
      setError(null);
      try {
        const user = await client.inviteUser(activeOrganizationId, email, role);
        await fetchUsersAndGrants();
        return user;
      } catch (err) {
        setError(err as Error);
        throw err;
      } finally {
        setIsMutating(false);
      }
    },
    [client, activeOrganizationId, fetchUsersAndGrants]
  );

  const grantRole = useCallback(
    async (data: GrantRoleRequest) => {
      if (!activeOrganizationId) {
        throw new Error("No active organization selected");
      }
      setIsMutating(true);
      setError(null);
      try {
        const grant = await client.grantRole(activeOrganizationId, data);
        await fetchUsersAndGrants();
        return grant;
      } catch (err) {
        setError(err as Error);
        throw err;
      } finally {
        setIsMutating(false);
      }
    },
    [client, activeOrganizationId, fetchUsersAndGrants]
  );

  const revokeRoleGrant = useCallback(
    async (grantId: string) => {
      if (!activeOrganizationId) {
        throw new Error("No active organization selected");
      }
      setIsMutating(true);
      setError(null);
      try {
        await client.revokeRoleGrant(activeOrganizationId, grantId);
        await fetchUsersAndGrants();
      } catch (err) {
        setError(err as Error);
        throw err;
      } finally {
        setIsMutating(false);
      }
    },
    [client, activeOrganizationId, fetchUsersAndGrants]
  );

  const removeUser = useCallback(
    async (userId: string) => {
      if (!activeOrganizationId) {
        throw new Error("No active organization selected");
      }
      setIsMutating(true);
      setError(null);
      try {
        await client.removeUser(activeOrganizationId, userId);
        await fetchUsersAndGrants();
      } catch (err) {
        setError(err as Error);
        throw err;
      } finally {
        setIsMutating(false);
      }
    },
    [client, activeOrganizationId, fetchUsersAndGrants]
  );

  useEffect(() => {
    fetchUsersAndGrants();
  }, [fetchUsersAndGrants]);

  return {
    members,
    roleGrants,
    isLoading,
    isMutating,
    error,
    inviteUser,
    grantRole,
    revokeRoleGrant,
    removeUser,
    refresh: fetchUsersAndGrants,
  };
}

export const useMembers = useUsers;
