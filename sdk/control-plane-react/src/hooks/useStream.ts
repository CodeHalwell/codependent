import { useEffect, useRef, useState } from "react";
import { useControlPlaneContext } from "./useControlPlaneContext.js";
import type {
  StreamEvent,
  StreamName,
  StreamResumeCursor,
} from "@codypendent/control-plane";

export interface UseStreamOptions<T = Record<string, unknown>> {
  stream?: StreamName | undefined;
  repositoryId?: string | undefined;
  initialCursor?: StreamResumeCursor | undefined;
  onEvent?: ((event: StreamEvent<T>) => void) | undefined;
  enabled?: boolean | undefined;
}

export interface UseStreamResult<T = Record<string, unknown>> {
  lastEvent: StreamEvent<T> | null;
  events: StreamEvent<T>[];
  isConnected: boolean;
  error: Error | null;
  clearEvents: () => void;
}

export function useStream<T = Record<string, unknown>>(
  options: UseStreamOptions<T> = {}
): UseStreamResult<T> {
  const { streamClient, activeOrganizationId } = useControlPlaneContext();

  const [lastEvent, setLastEvent] = useState<StreamEvent<T> | null>(null);
  const [events, setEvents] = useState<StreamEvent<T>[]>([]);
  const [isConnected, setIsConnected] = useState<boolean>(false);
  const [error, setError] = useState<Error | null>(null);

  const onEventRef = useRef(options.onEvent);
  onEventRef.current = options.onEvent;

  useEffect(() => {
    if (!activeOrganizationId || options.enabled === false) {
      setIsConnected(false);
      return;
    }

    const unsubscribe = streamClient.subscribe({
      organizationId: activeOrganizationId,
      stream: options.stream,
      repositoryId: options.repositoryId,
      cursor: options.initialCursor,
      onEvent: (evt) => {
        const typedEvt = evt as StreamEvent<T>;
        setLastEvent(typedEvt);
        setEvents((prev) => [...prev.slice(-99), typedEvt]);
        onEventRef.current?.(typedEvt);
      },
      onConnect: () => {
        setIsConnected(true);
        setError(null);
      },
      onDisconnect: () => {
        setIsConnected(false);
      },
      onError: (err) => {
        setError(err);
      },
    });

    return () => {
      unsubscribe();
    };
  }, [streamClient, activeOrganizationId, options.stream, options.repositoryId, options.initialCursor, options.enabled]);

  return {
    lastEvent,
    events,
    isConnected,
    error,
    clearEvents: () => setEvents([]),
  };
}
