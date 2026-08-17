import { useCallback, useEffect, useMemo, useState } from "react";
import { useControlPlaneContext } from "./useControlPlaneContext.js";
import type {
  ControlPlaneInboxEntry,
  InboxEntryState,
  InboxListQuery,
} from "@codypendent/control-plane";

export interface UseInboxOptions {
  query?: InboxListQuery | undefined;
  subscribeLive?: boolean | undefined;
}

export interface UseInboxResult {
  items: ControlPlaneInboxEntry[];
  unreadCount: number;
  isLoading: boolean;
  error: Error | null;
  markAsRead: (id: string) => Promise<void>;
  markAsDismissed: (id: string) => Promise<void>;
  refresh: () => Promise<void>;
}

export function useInbox(options: UseInboxOptions = {}): UseInboxResult {
  const { client, streamClient, activeOrganizationId } = useControlPlaneContext();

  const [items, setItems] = useState<ControlPlaneInboxEntry[]>([]);
  const [isLoading, setIsLoading] = useState<boolean>(true);
  const [error, setError] = useState<Error | null>(null);

  const fetchInbox = useCallback(async () => {
    if (!activeOrganizationId) {
      setItems([]);
      setIsLoading(false);
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      const page = await client.listInbox(activeOrganizationId, options.query ?? {});
      setItems(page.items);
    } catch (err) {
      setError(err as Error);
    } finally {
      setIsLoading(false);
    }
  }, [client, activeOrganizationId, options.query]);

  const mutateItem = useCallback(
    async (id: string, newState: InboxEntryState) => {
      if (!activeOrganizationId) return;
      // Optimistic update
      setItems((prev) =>
        prev.map((item) =>
          item.id === id
            ? {
                ...item,
                state: newState,
                readAt: newState === "read" ? new Date().toISOString() : item.readAt,
                actedAt: newState === "acted" || newState === "dismissed" ? new Date().toISOString() : item.actedAt,
              }
            : item
        )
      );

      try {
        await client.mutateInbox(activeOrganizationId, id, { state: newState });
      } catch (err) {
        await fetchInbox();
        throw err;
      }
    },
    [client, activeOrganizationId, fetchInbox]
  );

  const markAsRead = useCallback((id: string) => mutateItem(id, "read"), [mutateItem]);
  const markAsDismissed = useCallback((id: string) => mutateItem(id, "dismissed"), [mutateItem]);

  useEffect(() => {
    fetchInbox();
  }, [fetchInbox]);

  // Live stream updates for notifications/inbox
  useEffect(() => {
    if (!activeOrganizationId || options.subscribeLive === false) return;

    const unsubscribe = streamClient.subscribe({
      organizationId: activeOrganizationId,
      stream: "notifications",
      onEvent: (event) => {
        const newEntry = event.payload as Partial<ControlPlaneInboxEntry>;
        if (newEntry && newEntry.id) {
          setItems((prev) => [newEntry as ControlPlaneInboxEntry, ...prev.filter((i) => i.id !== newEntry.id)]);
        }
      },
    });

    return () => {
      unsubscribe();
    };
  }, [streamClient, activeOrganizationId, options.subscribeLive]);

  const unreadCount = useMemo(
    () => items.filter((i) => i.state === "unread").length,
    [items]
  );

  return {
    items,
    unreadCount,
    isLoading,
    error,
    markAsRead,
    markAsDismissed,
    refresh: fetchInbox,
  };
}
