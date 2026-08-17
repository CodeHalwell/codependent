import { useCallback, useEffect, useState } from "react";
import { useControlPlaneContext } from "./useControlPlaneContext.js";
import type {
  AuditPage,
  AuditQuery,
  AuditRecord,
  AuditVerificationResult,
} from "@codypendent/control-plane";

export interface UseAuditLogsOptions {
  initialQuery?: AuditQuery | undefined;
}

export interface UseAuditLogsResult {
  records: AuditRecord[];
  isLoading: boolean;
  error: Error | null;
  cursor: string | null;
  hasMore: boolean;
  nextPage: () => Promise<void>;
  refresh: () => Promise<void>;
  filter: AuditQuery;
  setFilter: (filter: AuditQuery | ((prev: AuditQuery) => AuditQuery)) => void;
  verifyChain: () => Promise<AuditVerificationResult>;
  verificationResult: AuditVerificationResult | null;
  isVerifying: boolean;
}

export function useAuditLogs(options: UseAuditLogsOptions = {}): UseAuditLogsResult {
  const { client, activeOrganizationId } = useControlPlaneContext();

  const [records, setRecords] = useState<AuditRecord[]>([]);
  const [isLoading, setIsLoading] = useState<boolean>(true);
  const [error, setError] = useState<Error | null>(null);
  const [cursor, setCursor] = useState<string | null>(null);
  const [hasMore, setHasMore] = useState<boolean>(false);
  const [filter, setFilter] = useState<AuditQuery>(options.initialQuery ?? { limit: 50 });

  const [verificationResult, setVerificationResult] = useState<AuditVerificationResult | null>(null);
  const [isVerifying, setIsVerifying] = useState<boolean>(false);

  const fetchRecords = useCallback(
    async (targetCursor?: string | null | undefined) => {
      if (!activeOrganizationId) {
        setRecords([]);
        setIsLoading(false);
        return;
      }
      setIsLoading(true);
      setError(null);
      try {
        const query: AuditQuery = {
          ...filter,
          cursor: targetCursor ?? undefined,
        };
        const page: AuditPage = await client.listAuditRecords(activeOrganizationId, query);
        setRecords(page.items);
        setCursor(page.cursor);
        setHasMore(page.hasMore);
      } catch (err) {
        setError(err as Error);
      } finally {
        setIsLoading(false);
      }
    },
    [client, activeOrganizationId, filter]
  );

  const refresh = useCallback(async () => {
    await fetchRecords(null);
  }, [fetchRecords]);

  const nextPage = useCallback(async () => {
    if (!cursor || !hasMore) return;
    await fetchRecords(cursor);
  }, [cursor, hasMore, fetchRecords]);

  const verifyChain = useCallback(async () => {
    if (!activeOrganizationId) {
      const res: AuditVerificationResult = {
        valid: false,
        totalRecordsChecked: 0,
        message: "No organization selected",
      };
      setVerificationResult(res);
      return res;
    }
    setIsVerifying(true);
    try {
      const result = await client.verifyAuditChain(activeOrganizationId, 100);
      setVerificationResult(result);
      return result;
    } catch (err) {
      const res: AuditVerificationResult = {
        valid: false,
        totalRecordsChecked: 0,
        message: (err as Error).message,
      };
      setVerificationResult(res);
      return res;
    } finally {
      setIsVerifying(false);
    }
  }, [client, activeOrganizationId]);

  useEffect(() => {
    fetchRecords(null);
  }, [fetchRecords]);

  return {
    records,
    isLoading,
    error,
    cursor,
    hasMore,
    nextPage,
    refresh,
    filter,
    setFilter,
    verifyChain,
    verificationResult,
    isVerifying,
  };
}
