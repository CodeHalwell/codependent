import { useCallback, useEffect, useState } from "react";
import { useControlPlaneContext } from "./useControlPlaneContext.js";
import type {
  CreatePairingChallengeRequest,
  Daemon,
  PairingChallenge,
} from "@codypendent/control-plane";

export interface UseDaemonsResult {
  daemons: Daemon[];
  isLoading: boolean;
  isMutating: boolean;
  error: Error | null;
  createPairingChallenge: (
    options?: Omit<CreatePairingChallengeRequest, "organizationId"> | undefined
  ) => Promise<PairingChallenge>;
  revokeDaemon: (daemonId: string, reason?: string | undefined) => Promise<void>;
  refresh: () => Promise<void>;
}

export function useDaemons(): UseDaemonsResult {
  const { client, activeOrganizationId } = useControlPlaneContext();

  const [daemons, setDaemons] = useState<Daemon[]>([]);
  const [isLoading, setIsLoading] = useState<boolean>(true);
  const [isMutating, setIsMutating] = useState<boolean>(false);
  const [error, setError] = useState<Error | null>(null);

  const fetchDaemons = useCallback(async () => {
    if (!activeOrganizationId) {
      setDaemons([]);
      setIsLoading(false);
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      const list = await client.listDaemons(activeOrganizationId);
      setDaemons(list);
    } catch (err) {
      setError(err as Error);
    } finally {
      setIsLoading(false);
    }
  }, [client, activeOrganizationId]);

  const createPairingChallenge = useCallback(
    async (options?: Omit<CreatePairingChallengeRequest, "organizationId"> | undefined) => {
      if (!activeOrganizationId) {
        throw new Error("No active organization selected");
      }
      setIsMutating(true);
      setError(null);
      try {
        const challenge = await client.createPairingChallenge({
          ...options,
          organizationId: activeOrganizationId,
        });
        return challenge;
      } catch (err) {
        setError(err as Error);
        throw err;
      } finally {
        setIsMutating(false);
      }
    },
    [client, activeOrganizationId]
  );

  const revokeDaemon = useCallback(
    async (daemonId: string, reason?: string | undefined) => {
      if (!activeOrganizationId) {
        throw new Error("No active organization selected");
      }
      setIsMutating(true);
      setError(null);
      try {
        await client.revokeDaemon(activeOrganizationId, daemonId, reason);
        await fetchDaemons();
      } catch (err) {
        setError(err as Error);
        throw err;
      } finally {
        setIsMutating(false);
      }
    },
    [client, activeOrganizationId, fetchDaemons]
  );

  useEffect(() => {
    fetchDaemons();
  }, [fetchDaemons]);

  return {
    daemons,
    isLoading,
    isMutating,
    error,
    createPairingChallenge,
    revokeDaemon,
    refresh: fetchDaemons,
  };
}
