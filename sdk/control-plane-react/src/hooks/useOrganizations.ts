import { useCallback, useState } from "react";
import { useControlPlaneContext } from "./useControlPlaneContext.js";
import type {
  CreateOrganizationRequest,
  Organization,
  UpdateOrganizationPolicyRequest,
} from "@codypendent/control-plane";

export interface UseOrganizationsResult {
  organizations: Organization[];
  activeOrganization: Organization | null;
  activeOrganizationId: string | null;
  setActiveOrganizationId: (id: string | null) => void;
  createOrganization: (data: CreateOrganizationRequest) => Promise<Organization>;
  updateOrganizationPolicy: (
    id: string,
    data: UpdateOrganizationPolicyRequest
  ) => Promise<Organization>;
  refreshOrganizations: () => Promise<void>;
  isLoading: boolean;
  error: Error | null;
}

export function useOrganizations(): UseOrganizationsResult {
  const {
    client,
    organizations,
    activeOrganization,
    activeOrganizationId,
    setActiveOrganizationId,
    refreshOrganizations,
    isLoading: contextLoading,
    error: contextError,
  } = useControlPlaneContext();

  const [isMutating, setIsMutating] = useState<boolean>(false);
  const [mutationError, setMutationError] = useState<Error | null>(null);

  const createOrganization = useCallback(
    async (data: CreateOrganizationRequest) => {
      setIsMutating(true);
      setMutationError(null);
      try {
        const org = await client.createOrganization(data);
        await refreshOrganizations();
        setActiveOrganizationId(org.id);
        return org;
      } catch (err) {
        setMutationError(err as Error);
        throw err;
      } finally {
        setIsMutating(false);
      }
    },
    [client, refreshOrganizations, setActiveOrganizationId]
  );

  const updateOrganizationPolicy = useCallback(
    async (id: string, data: UpdateOrganizationPolicyRequest) => {
      setIsMutating(true);
      setMutationError(null);
      try {
        const org = await client.updateOrganizationPolicy(id, data);
        await refreshOrganizations();
        return org;
      } catch (err) {
        setMutationError(err as Error);
        throw err;
      } finally {
        setIsMutating(false);
      }
    },
    [client, refreshOrganizations]
  );

  return {
    organizations,
    activeOrganization,
    activeOrganizationId,
    setActiveOrganizationId,
    createOrganization,
    updateOrganizationPolicy,
    refreshOrganizations,
    isLoading: contextLoading || isMutating,
    error: mutationError || contextError,
  };
}
