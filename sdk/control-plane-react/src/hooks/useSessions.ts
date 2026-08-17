import { useCallback, useEffect, useState } from "react";
import { useControlPlaneContext } from "./useControlPlaneContext.js";
import type {
  SessionListQuery,
  SharedSession,
  SharedSessionDetail,
} from "@codypendent/control-plane";

export interface UseSessionsOptions {
  repositoryId?: string | undefined;
  state?: string | undefined;
  search?: string | undefined;
  limit?: number | undefined;
  autoRefreshIntervalMs?: number | undefined;
  subscribeLive?: boolean | undefined;
}

export interface UseSessionsResult {
  sessions: SharedSession[];
  selectedSession: SharedSessionDetail | null;
  selectedSessionId: string | null;
  setSelectedSessionId: (id: string | null) => void;
  isLoading: boolean;
  isDetailLoading: boolean;
  error: Error | null;
  cursor: string | null;
  hasMore: boolean;
  nextPage: () => Promise<void>;
  prevPage: () => Promise<void>;
  refresh: () => Promise<void>;
  filter: SessionListQuery;
  setFilter: (filter: SessionListQuery | ((prev: SessionListQuery) => SessionListQuery)) => void;
}

export function useSessions(options: UseSessionsOptions = {}): UseSessionsResult {
  const { client, streamClient, activeOrganizationId, activeRepositoryId } = useControlPlaneContext();

  const [sessions, setSessions] = useState<SharedSession[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [selectedSession, setSelectedSession] = useState<SharedSessionDetail | null>(null);
  const [isLoading, setIsLoading] = useState<boolean>(true);
  const [isDetailLoading, setIsDetailLoading] = useState<boolean>(false);
  const [error, setError] = useState<Error | null>(null);
  const [cursor, setCursor] = useState<string | null>(null);
  const [cursorHistory, setCursorHistory] = useState<(string | null)[]>([]);
  const [hasMore, setHasMore] = useState<boolean>(false);

  const [filter, setFilter] = useState<SessionListQuery>({
    repositoryId: options.repositoryId ?? (activeRepositoryId ?? undefined),
    state: options.state,
    search: options.search,
    limit: options.limit ?? 20,
  });

  const fetchSessions = useCallback(
    async (targetCursor?: string | null | undefined) => {
      if (!activeOrganizationId) {
        setSessions([]);
        setIsLoading(false);
        return;
      }
      setIsLoading(true);
      setError(null);
      try {
        const queryParams: SessionListQuery = {
          ...filter,
          cursor: targetCursor ?? undefined,
        };
        const page = await client.listSharedSessions(activeOrganizationId, queryParams);
        setSessions(page.items);
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
    setCursorHistory([]);
    await fetchSessions(null);
  }, [fetchSessions]);

  const nextPage = useCallback(async () => {
    if (!cursor || !hasMore) return;
    setCursorHistory((prev) => [...prev, cursor]);
    await fetchSessions(cursor);
  }, [cursor, hasMore, fetchSessions]);

  const prevPage = useCallback(async () => {
    if (cursorHistory.length === 0) return;
    const newHistory = [...cursorHistory];
    newHistory.pop(); // remove current
    const prevCursor = newHistory[newHistory.length - 1] ?? null;
    setCursorHistory(newHistory);
    await fetchSessions(prevCursor);
  }, [cursorHistory, fetchSessions]);

  // Load session details when selected
  useEffect(() => {
    let mounted = true;
    if (!activeOrganizationId || !selectedSessionId) {
      setSelectedSession(null);
      return;
    }
    const loadDetail = async () => {
      setIsDetailLoading(true);
      try {
        const detail = await client.getSharedSession(activeOrganizationId, selectedSessionId);
        if (mounted) setSelectedSession(detail);
      } catch {
        if (mounted) setSelectedSession(null);
      } finally {
        if (mounted) setIsDetailLoading(false);
      }
    };
    loadDetail();
    return () => {
      mounted = false;
    };
  }, [client, activeOrganizationId, selectedSessionId]);

  // Initial & filter change fetch
  useEffect(() => {
    fetchSessions(null);
  }, [fetchSessions]);

  // Live stream subscription for session status updates
  useEffect(() => {
    if (!activeOrganizationId || options.subscribeLive === false) return;

    const unsubscribe = streamClient.subscribe({
      organizationId: activeOrganizationId,
      stream: "sessions",
      repositoryId: filter.repositoryId,
      onEvent: (event) => {
        const updated = event.payload as Partial<SharedSession>;
        if (updated && updated.id) {
          setSessions((prev) =>
            prev.map((s) => (s.id === updated.id ? { ...s, ...updated } : s))
          );
          if (selectedSessionId === updated.id) {
            setSelectedSession((prev) => (prev ? { ...prev, ...updated } : null));
          }
        }
      },
    });

    return () => {
      unsubscribe();
    };
  }, [streamClient, activeOrganizationId, filter.repositoryId, selectedSessionId, options.subscribeLive]);

  return {
    sessions,
    selectedSession,
    selectedSessionId,
    setSelectedSessionId,
    isLoading,
    isDetailLoading,
    error,
    cursor,
    hasMore,
    nextPage,
    prevPage,
    refresh,
    filter,
    setFilter,
  };
}
