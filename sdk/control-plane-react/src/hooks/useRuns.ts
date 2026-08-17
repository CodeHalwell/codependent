import { useSessions, type UseSessionsOptions } from "./useSessions.js";
import type { SharedRunStep, SharedSession, SharedSessionDetail } from "@codypendent/control-plane";

export interface UseRunsResult {
  runs: SharedSession[];
  activeRun: SharedSessionDetail | null;
  activeRunId: string | null;
  setActiveRunId: (id: string | null) => void;
  runSteps: SharedRunStep[];
  isLoading: boolean;
  isDetailLoading: boolean;
  error: Error | null;
  refresh: () => Promise<void>;
}

export function useRuns(options: UseSessionsOptions = {}): UseRunsResult {
  const {
    sessions,
    selectedSession,
    selectedSessionId,
    setSelectedSessionId,
    isLoading,
    isDetailLoading,
    error,
    refresh,
  } = useSessions(options);

  return {
    runs: sessions,
    activeRun: selectedSession,
    activeRunId: selectedSessionId,
    setActiveRunId: setSelectedSessionId,
    runSteps: selectedSession?.steps ?? [],
    isLoading,
    isDetailLoading,
    error,
    refresh,
  };
}
