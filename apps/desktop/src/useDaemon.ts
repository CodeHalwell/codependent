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

import { createTransport, type DesktopTransport } from "./transport.js";
import { initialState, reduce, type DaemonState } from "./daemonState.js";

/** Shown when the UI is running outside the Tauri shell (a browser tab). */
export const NO_SHELL_DETAIL =
  "Not running in the Codypendent desktop shell, so there is no daemon transport. Start it with `npm run tauri:dev`.";

export interface DaemonController {
  state: DaemonState;
  submit: (objective: string) => Promise<void>;
  cancel: () => Promise<void>;
  selectSession: (sessionId: string) => Promise<void>;
}

export function useDaemon(
  makeTransport: () => DesktopTransport | null = createTransport,
): DaemonController {
  const [state, dispatch] = useReducer(reduce, initialState);
  const factory = useRef(makeTransport);
  const transport = useRef<DesktopTransport | null>(null);

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
            dispatch({ type: "sessions", sessions });
          }
        } catch (error) {
          // Listing is not the connection: a daemon that answered the
          // handshake but refused this command is still connected.
          if (live) {
            dispatch({ type: "command-failed", message: describe(error) });
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

  return { state, submit, cancel, selectSession };
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
