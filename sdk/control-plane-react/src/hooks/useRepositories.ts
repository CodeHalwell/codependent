import { useCallback, useState } from "react";
import { useControlPlaneContext } from "./useControlPlaneContext.js";
import type {
  RegisterRepositoryRequest,
  Repository,
  UpdateRepositoryPolicyRequest,
} from "@codypendent/control-plane";

export interface UseRepositoriesResult {
  repositories: Repository[];
  activeRepository: Repository | null;
  activeRepositoryId: string | null;
  setActiveRepositoryId: (id: string | null) => void;
  registerRepository: (data: RegisterRepositoryRequest) => Promise<Repository>;
  updateRepositoryPolicy: (
    repositoryId: string,
    data: UpdateRepositoryPolicyRequest
  ) => Promise<Repository>;
  deleteRepository: (repositoryId: string) => Promise<void>;
  refreshRepositories: () => Promise<void>;
  isLoading: boolean;
  error: Error | null;
}

export function useRepositories(): UseRepositoriesResult {
  const {
    client,
    repositories,
    activeRepository,
    activeRepositoryId,
    activeOrganizationId,
    setActiveRepositoryId,
    refreshRepositories,
    isLoading: contextLoading,
    error: contextError,
  } = useControlPlaneContext();

  const [isMutating, setIsMutating] = useState<boolean>(false);
  const [mutationError, setMutationError] = useState<Error | null>(null);

  const registerRepository = useCallback(
    async (data: RegisterRepositoryRequest) => {
      if (!activeOrganizationId) {
        throw new Error("No active organization selected");
      }
      setIsMutating(true);
      setMutationError(null);
      try {
        const repo = await client.registerRepository(activeOrganizationId, data);
        await refreshRepositories();
        setActiveRepositoryId(repo.id);
        return repo;
      } catch (err) {
        setMutationError(err as Error);
        throw err;
      } finally {
        setIsMutating(false);
      }
    },
    [client, activeOrganizationId, refreshRepositories, setActiveRepositoryId]
  );

  const updateRepositoryPolicy = useCallback(
    async (repositoryId: string, data: UpdateRepositoryPolicyRequest) => {
      if (!activeOrganizationId) {
        throw new Error("No active organization selected");
      }
      setIsMutating(true);
      setMutationError(null);
      try {
        const repo = await client.updateRepositoryPolicy(activeOrganizationId, repositoryId, data);
        await refreshRepositories();
        return repo;
      } catch (err) {
        setMutationError(err as Error);
        throw err;
      } finally {
        setIsMutating(false);
      }
    },
    [client, activeOrganizationId, refreshRepositories]
  );

  const deleteRepository = useCallback(
    async (repositoryId: string) => {
      if (!activeOrganizationId) {
        throw new Error("No active organization selected");
      }
      setIsMutating(true);
      setMutationError(null);
      try {
        await client.deleteRepository(activeOrganizationId, repositoryId);
        await refreshRepositories();
        if (activeRepositoryId === repositoryId) {
          setActiveRepositoryId(null);
        }
      } catch (err) {
        setMutationError(err as Error);
        throw err;
      } finally {
        setIsMutating(false);
      }
    },
    [client, activeOrganizationId, activeRepositoryId, refreshRepositories, setActiveRepositoryId]
  );

  return {
    repositories,
    activeRepository,
    activeRepositoryId,
    setActiveRepositoryId,
    registerRepository,
    updateRepositoryPolicy,
    deleteRepository,
    refreshRepositories,
    isLoading: contextLoading || isMutating,
    error: mutationError || contextError,
  };
}
