import { useCallback, useEffect, useState } from "react";
import { useControlPlaneContext } from "./useControlPlaneContext.js";
import type {
  ApprovalDecision,
  PendingApproval,
} from "@codypendent/control-plane";

export interface UseApprovalsOptions {
  subscribeLive?: boolean | undefined;
}

export interface UseApprovalsResult {
  pendingApprovals: PendingApproval[];
  isLoading: boolean;
  isSubmitting: boolean;
  error: Error | null;
  decide: (
    approvalId: string,
    decision: ApprovalDecision,
    actionDigest: string,
    reason?: string | undefined
  ) => Promise<void>;
  refresh: () => Promise<void>;
}

export function useApprovals(options: UseApprovalsOptions = {}): UseApprovalsResult {
  const { client, streamClient, activeOrganizationId } = useControlPlaneContext();

  const [pendingApprovals, setPendingApprovals] = useState<PendingApproval[]>([]);
  const [isLoading, setIsLoading] = useState<boolean>(true);
  const [isSubmitting, setIsSubmitting] = useState<boolean>(false);
  const [error, setError] = useState<Error | null>(null);

  const fetchApprovals = useCallback(async () => {
    if (!activeOrganizationId) {
      setPendingApprovals([]);
      setIsLoading(false);
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      const list = await client.listPendingApprovals(activeOrganizationId);
      setPendingApprovals(list.filter((a) => a.status === "pending"));
    } catch (err) {
      setError(err as Error);
    } finally {
      setIsLoading(false);
    }
  }, [client, activeOrganizationId]);

  const decide = useCallback(
    async (
      approvalId: string,
      decision: ApprovalDecision,
      actionDigest: string,
      reason?: string | undefined
    ) => {
      if (!activeOrganizationId) return;

      setIsSubmitting(true);
      setError(null);

      // Optimistic removal
      setPendingApprovals((prev) => prev.filter((a) => a.id !== approvalId));

      try {
        await client.decideApproval(activeOrganizationId, approvalId, {
          decision,
          actionDigest,
          reason,
        });
      } catch (err) {
        await fetchApprovals();
        setError(err as Error);
        throw err;
      } finally {
        setIsSubmitting(false);
      }
    },
    [client, activeOrganizationId, fetchApprovals]
  );

  useEffect(() => {
    fetchApprovals();
  }, [fetchApprovals]);

  // Live stream updates for approval requests
  useEffect(() => {
    if (!activeOrganizationId || options.subscribeLive === false) return;

    const unsubscribe = streamClient.subscribe({
      organizationId: activeOrganizationId,
      stream: "approvals",
      onEvent: (event) => {
        const payload = event.payload as Partial<PendingApproval>;
        if (payload && payload.id) {
          if (payload.status === "pending") {
            setPendingApprovals((prev) => [
              payload as PendingApproval,
              ...prev.filter((a) => a.id !== payload.id),
            ]);
          } else {
            setPendingApprovals((prev) => prev.filter((a) => a.id !== payload.id));
          }
        }
      },
    });

    return () => {
      unsubscribe();
    };
  }, [streamClient, activeOrganizationId, options.subscribeLive]);

  return {
    pendingApprovals,
    isLoading,
    isSubmitting,
    error,
    decide,
    refresh: fetchApprovals,
  };
}
