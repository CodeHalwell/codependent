/**
 * Connects the store in `daemonState.ts` to the transport in `transport.ts`.
 *
 * The connection is attempted once on mount. It succeeds only when the shell's
 * handshake with `codypendentd` succeeded; every other outcome — no shell, no
 * socket, no daemon, a dropped socket — lands in a disconnected state carrying
 * the reason. Submit and cancel go straight to the daemon and do nothing
 * locally on failure beyond reporting it.
 */
import { useCallback, useEffect, useReducer, useRef } from "react";

import type {
  AnalyticsExportRequest,
  AnalyticsExportResult,
  AnalyticsPage,
  AnalyticsQuery,
  ArtifactRef,
  InboxListQuery,
} from "@codypendent/protocol";
import { createTransport, type ApprovalChoice, type DesktopTransport, type SessionRow } from "./transport.js";
import { initialState, reduce, type DaemonState } from "./daemonState.js";
import { publishFrame } from "./frameBus.js";
import {
  BlockingWorkNotifier,
  defaultNotificationSink,
  type NotificationSink,
} from "./osNotifications.js";

/** Shown when the UI is running outside the Tauri shell (a browser tab). */
export const NO_SHELL_DETAIL =
  "Not running in the Codypendent desktop shell, so there is no daemon transport. Start it with `npm run tauri:dev`.";

export interface DaemonController {
  state: DaemonState;
  submit: (objective: string) => Promise<void>;
  cancel: () => Promise<void>;
  selectSession: (sessionId: string) => Promise<void>;
  resolveApproval: (approvalId: string, decision: ApprovalChoice) => Promise<void>;
  loadInbox: (query?: InboxListQuery) => Promise<void>;
  acknowledgeInbox: (entryId: string) => Promise<void>;
  dismissInbox: (entryId: string) => Promise<void>;
  queryAnalytics: (query?: AnalyticsQuery) => Promise<AnalyticsPage | null>;
  exportAnalytics: (request: AnalyticsExportRequest) => Promise<AnalyticsExportResult | null>;
  readArtifact: (artifact: ArtifactRef) => Promise<Uint8Array | null>;
  /**
   * The live bridge, or `null` outside the shell.
   *
   * Exposed so the panels that model their own daemon-side resource — the
   * Session Library, workflow runs, the task board, a run's blackboard — can
   * call the bridge directly instead of routing every one of those reads
   * through a store that models a single session's transcript. It is the same
   * connected instance this hook handshook with; nothing else opens one.
   */
  transport: DesktopTransport | null;
}

export function useDaemon(
  makeTransport: () => DesktopTransport | null = createTransport,
  /**
   * Where blocking-work notifications go. Defaults to the OS, through Tauri's
   * notification plugin, whenever the app is running inside the shell; a test
   * injects its own sink to observe exactly what a user would be shown.
   */
  notify?: NotificationSink,
): DaemonController {
  const [state, dispatch] = useReducer(reduce, initialState);
  const factory = useRef(makeTransport);
  const transport = useRef<DesktopTransport | null>(null);
  /** Session titles, for naming the parked session in a notification. */
  const sessionTitles = useRef(new Map<string, string>());
  /**
   * Approvals and questions are the only two things that stop a run until a
   * human acts, so they are the only two that raise an OS notification. Every
   * other event stays in the transcript and the inbox badge
   * (`osNotifications.ts` explains why over-notifying is the bug).
   */
  const notifier = useRef<BlockingWorkNotifier | null>(null);
  if (notifier.current === null) {
    notifier.current = new BlockingWorkNotifier(
      notify ?? defaultNotificationSink((sessionId) =>
        sessionId === null ? undefined : sessionTitles.current.get(sessionId),
      ),
    );
  }

  const loadInbox = useCallback(async (query?: InboxListQuery) => {
    const client = transport.current;
    if (!client) {
      dispatch({ type: "inbox-unavailable", detail: NO_SHELL_DETAIL });
      return;
    }
    try {
      const page = await client.listInbox(query);
      dispatch({ type: "inbox-loaded", entries: page.items });
    } catch (error) {
      // The inbox view must say it could not read, rather than draw the empty
      // state and imply there is no pending human work.
      dispatch({ type: "inbox-unavailable", detail: describe(error) });
      dispatch({ type: "command-failed", message: describe(error) });
    }
  }, []);

  useEffect(() => {
    let live = true;
    const client = factory.current();
    transport.current = client;

    if (!client) {
      dispatch({ type: "shell-missing", detail: NO_SHELL_DETAIL });
      return;
    }

    dispatch({ type: "connecting", detail: "Connecting to codypendentd…" });
    client
      .connect((frame) => {
        if (live) {
          dispatch({ type: "frame", frame });
          // Workflow and blackboard frames are not session-scoped, so the
          // session reducer has nowhere to put them; the panels showing those
          // runs and boards subscribe to the bus instead.
          publishFrame(frame);
          // Same authoritative frames the store folds; read here for the two
          // kinds that block a human.
          notifier.current?.observeFrame(frame);
        }
      })
      .then(async (info) => {
        if (!live) {
          return;
        }
        dispatch({ type: "connected", info });
        try {
          const sessions = await client.listSessions();
          if (live) {
            rememberTitles(sessionTitles.current, sessions);
            dispatch({ type: "sessions", sessions });
          }
        } catch (error) {
          // Listing is not the connection: a daemon that answered the
          // handshake but refused this command is still connected.
          if (live) {
            dispatch({ type: "command-failed", message: describe(error) });
          }
        }
        try {
          const page = await client.listInbox();
          if (live) {
            dispatch({ type: "inbox-loaded", entries: page.items });
          }
        } catch (error) {
          // A connected daemon that cannot serve `ListInbox` leaves the inbox
          // unread, not empty. Record why so the view can say so.
          if (live) {
            dispatch({ type: "inbox-unavailable", detail: describe(error) });
          }
        }
      })
      .catch(async (error) => {
        if (!live) {
          return;
        }
        let socket = "";
        try {
          socket = await client.socketPath();
        } catch {
          socket = "";
        }
        dispatch({
          type: "connect-failed",
          detail: socket
            ? `No daemon on ${socket}: ${describe(error)}`
            : `No daemon: ${describe(error)}`,
        });
      });

    return () => {
      live = false;
      void client.disconnect().catch(() => undefined);
    };
  }, []);

  const submit = useCallback(async (objective: string) => {
    const client = transport.current;
    if (!client) {
      dispatch({ type: "command-failed", message: NO_SHELL_DETAIL });
      return;
    }
    try {
      const handle = await client.startObjective(objective);
      dispatch({ type: "run-submitted", handle });
      try {
        const sessions = await client.listSessions();
        rememberTitles(sessionTitles.current, sessions);
        dispatch({ type: "sessions", sessions });
      } catch {
        // Ignore session refresh failure
      }
    } catch (error) {
      dispatch({ type: "command-failed", message: describe(error) });
    }
  }, []);

  const activeRunId = state.activeRunId;
  const cancel = useCallback(async () => {
    const client = transport.current;
    if (!client || !activeRunId) {
      return;
    }
    try {
      await client.cancelRun(activeRunId);
    } catch (error) {
      dispatch({ type: "command-failed", message: describe(error) });
    }
  }, [activeRunId]);

  const selectSession = useCallback(async (sessionId: string) => {
    const client = transport.current;
    if (!client) {
      return;
    }
    dispatch({ type: "session-selected", sessionId });
    try {
      await client.attachSession(sessionId);
    } catch (error) {
      dispatch({ type: "command-failed", message: describe(error) });
    }
  }, []);

  const resolveApproval = useCallback(async (approvalId: string, decision: ApprovalChoice) => {
    const client = transport.current;
    if (!client) {
      dispatch({ type: "command-failed", message: NO_SHELL_DETAIL });
      return;
    }
    try {
      await client.resolveApproval(approvalId, decision);
    } catch (error) {
      dispatch({ type: "command-failed", message: describe(error) });
    }
  }, []);

  const acknowledgeInbox = useCallback(async (entryId: string) => {
    const client = transport.current;
    if (!client) {
      dispatch({ type: "command-failed", message: NO_SHELL_DETAIL });
      return;
    }
    try {
      const entry = await client.mutateInbox({ type: "Acknowledge", entry_id: entryId });
      dispatch({ type: "inbox-entry-updated", entry });
    } catch (error) {
      dispatch({ type: "command-failed", message: describe(error) });
    }
  }, []);

  const dismissInbox = useCallback(async (entryId: string) => {
    const client = transport.current;
    if (!client) {
      dispatch({ type: "command-failed", message: NO_SHELL_DETAIL });
      return;
    }
    try {
      const entry = await client.mutateInbox({ type: "Dismiss", entry_id: entryId });
      dispatch({ type: "inbox-entry-updated", entry });
    } catch (error) {
      dispatch({ type: "command-failed", message: describe(error) });
    }
  }, []);

  const queryAnalytics = useCallback(async (query?: AnalyticsQuery) => {
    const client = transport.current;
    if (!client) {
      dispatch({ type: "command-failed", message: NO_SHELL_DETAIL });
      return null;
    }
    try {
      return await client.queryAnalytics(query);
    } catch (error) {
      dispatch({ type: "command-failed", message: describe(error) });
      return null;
    }
  }, []);

  const exportAnalytics = useCallback(async (request: AnalyticsExportRequest) => {
    const client = transport.current;
    if (!client) {
      dispatch({ type: "command-failed", message: NO_SHELL_DETAIL });
      return null;
    }
    try {
      return await client.exportAnalytics(request);
    } catch (error) {
      dispatch({ type: "command-failed", message: describe(error) });
      return null;
    }
  }, []);

  const readArtifact = useCallback(async (artifact: ArtifactRef) => {
    const client = transport.current;
    if (!client || !client.readArtifact) {
      dispatch({ type: "command-failed", message: NO_SHELL_DETAIL });
      return null;
    }
    try {
      return await client.readArtifact(artifact);
    } catch (error) {
      dispatch({ type: "command-failed", message: describe(error) });
      return null;
    }
  }, []);

  return {
    state,
    submit,
    cancel,
    selectSession,
    resolveApproval,
    loadInbox,
    acknowledgeInbox,
    dismissInbox,
    queryAnalytics,
    exportAnalytics,
    readArtifact,
    transport: transport.current,
  };
}

/** Titles come from the daemon's own session list; nothing is invented. */
function rememberTitles(titles: Map<string, string>, sessions: readonly SessionRow[]): void {
  for (const session of sessions) {
    titles.set(session.session_id, session.title);
  }
}

function describe(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (error instanceof Error) {
    return error.message;
  }
  return String(error);
}
