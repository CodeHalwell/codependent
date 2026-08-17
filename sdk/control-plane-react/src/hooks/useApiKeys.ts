import { useCallback, useEffect, useState } from "react";
import { useControlPlaneContext } from "./useControlPlaneContext.js";
import type {
  ApiKey,
  ApiKeyCreatedResponse,
  CreateApiKeyRequest,
} from "@codypendent/control-plane";

export interface UseApiKeysResult {
  apiKeys: ApiKey[];
  isLoading: boolean;
  isMutating: boolean;
  error: Error | null;
  createApiKey: (data: Omit<CreateApiKeyRequest, "organizationId">) => Promise<ApiKeyCreatedResponse>;
  revokeApiKey: (apiKeyId: string) => Promise<void>;
  refresh: () => Promise<void>;
}

export function useApiKeys(): UseApiKeysResult {
  const { client, activeOrganizationId } = useControlPlaneContext();

  const [apiKeys, setApiKeys] = useState<ApiKey[]>([]);
  const [isLoading, setIsLoading] = useState<boolean>(true);
  const [isMutating, setIsMutating] = useState<boolean>(false);
  const [error, setError] = useState<Error | null>(null);

  const fetchApiKeys = useCallback(async () => {
    if (!activeOrganizationId) {
      setApiKeys([]);
      setIsLoading(false);
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      const keys = await client.listApiKeys(activeOrganizationId);
      setApiKeys(keys);
    } catch (err) {
      setError(err as Error);
    } finally {
      setIsLoading(false);
    }
  }, [client, activeOrganizationId]);

  const createApiKey = useCallback(
    async (data: Omit<CreateApiKeyRequest, "organizationId">) => {
      if (!activeOrganizationId) {
        throw new Error("No active organization selected");
      }
      setIsMutating(true);
      setError(null);
      try {
        const res = await client.createApiKey(activeOrganizationId, {
          ...data,
          organizationId: activeOrganizationId,
        });
        await fetchApiKeys();
        return res;
      } catch (err) {
        setError(err as Error);
        throw err;
      } finally {
        setIsMutating(false);
      }
    },
    [client, activeOrganizationId, fetchApiKeys]
  );

  const revokeApiKey = useCallback(
    async (apiKeyId: string) => {
      if (!activeOrganizationId) {
        throw new Error("No active organization selected");
      }
      setIsMutating(true);
      setError(null);
      try {
        await client.revokeApiKey(activeOrganizationId, apiKeyId);
        await fetchApiKeys();
      } catch (err) {
        setError(err as Error);
        throw err;
      } finally {
        setIsMutating(false);
      }
    },
    [client, activeOrganizationId, fetchApiKeys]
  );

  useEffect(() => {
    fetchApiKeys();
  }, [fetchApiKeys]);

  return {
    apiKeys,
    isLoading,
    isMutating,
    error,
    createApiKey,
    revokeApiKey,
    refresh: fetchApiKeys,
  };
}
