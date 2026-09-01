/**
 * The banner across the top of the main pane while the daemon is not
 * connected.
 *
 * "Not connected" is not a detail for a corner: it changes what every view
 * below can be trusted to mean. This used to be a sentence and the raw socket
 * error — `No such file or directory (os error 2)` — with nothing to click and
 * no instruction, on top of a reconnect loop that could never succeed because
 * nobody had started a daemon. Now it says what is missing, offers to start it
 * (`launcher.rs`, the CLI's own `ensure_daemon` flow), offers an immediate
 * retry, and names the terminal command for the case where the shell cannot.
 */
import React, { useEffect, useState } from "react";

import type { ConnectionStatus } from "../types.js";
import type { DaemonLaunchStatus } from "../transport.js";

export interface ConnectionBannerProps {
  status: ConnectionStatus;
  /** The daemon's (or shell's) own reason, verbatim. */
  detail: string;
  /** Whether there is a transport at all — outside the shell there is none. */
  hasTransport: boolean;
  /** Whether the shell offers `startDaemon`. */
  canStart: boolean;
  onStart: () => Promise<{ started: boolean; detail: string }>;
  onRetry: () => void;
  /** Reads what the shell would launch; absent on an older shell. */
  launchStatus?: () => Promise<DaemonLaunchStatus>;
}

const BANNER: React.CSSProperties = {
  padding: "10px 24px",
  fontSize: 12,
  lineHeight: 1.5,
  display: "flex",
  flexDirection: "column",
  gap: 6,
};
const DISCONNECTED: React.CSSProperties = {
  ...BANNER,
  background: "var(--cody-danger-bg, #2d1214)",
  borderBottom: "1px solid var(--cody-danger, #da3633)",
  color: "var(--cody-danger-text, #ffa198)",
};
const CONNECTING: React.CSSProperties = {
  ...BANNER,
  background: "var(--cody-warning-bg, #2b2109)",
  borderBottom: "1px solid var(--cody-warning-border, #9e6a03)",
  color: "var(--cody-warning-text, #e3b341)",
};
const GUIDANCE: React.CSSProperties = { color: "var(--cody-text, #e6edf3)" };
const RAW: React.CSSProperties = {
  fontFamily: "var(--cody-mono, ui-monospace, SFMono-Regular, Menlo, monospace)",
  fontSize: 11,
  opacity: 0.9,
  overflowWrap: "anywhere",
};
const ACTIONS: React.CSSProperties = { display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" };
const PRIMARY: React.CSSProperties = {
  background: "var(--cody-success-strong, #238636)",
  border: "none",
  color: "#fff",
  padding: "5px 12px",
  borderRadius: 6,
  fontSize: 12,
  cursor: "pointer",
  fontWeight: 600,
};
const SECONDARY: React.CSSProperties = {
  background: "var(--cody-inset, #21262d)",
  border: "1px solid var(--cody-border-strong, #30363d)",
  color: "var(--cody-text, #e6edf3)",
  padding: "5px 12px",
  borderRadius: 6,
  fontSize: 12,
  cursor: "pointer",
  fontWeight: 600,
};
const CODE: React.CSSProperties = {
  fontFamily: "var(--cody-mono, ui-monospace, SFMono-Regular, Menlo, monospace)",
  background: "var(--cody-canvas, #0d1117)",
  border: "1px solid var(--cody-border-strong, #30363d)",
  borderRadius: 4,
  padding: "0 4px",
};

export const ConnectionBanner: React.FC<ConnectionBannerProps> = ({
  status,
  detail,
  hasTransport,
  canStart,
  onStart,
  onRetry,
  launchStatus,
}) => {
  const [launch, setLaunch] = useState<DaemonLaunchStatus | null>(null);
  const [starting, setStarting] = useState(false);
  const [outcome, setOutcome] = useState<{ started: boolean; detail: string } | null>(null);

  // What the shell would launch, read whenever we land in the disconnected
  // state. A failed read leaves the banner with the manual command only.
  useEffect(() => {
    if (status !== "disconnected" || !launchStatus) {
      return;
    }
    let cancelled = false;
    void launchStatus()
      .then((state) => {
        if (!cancelled) {
          setLaunch(state);
        }
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [status, launchStatus]);

  // A fresh disconnect retires the previous attempt's report.
  useEffect(() => {
    if (status === "connected") {
      setOutcome(null);
    }
  }, [status]);

  if (status === "connected") {
    return null;
  }

  if (status === "connecting") {
    return (
      <div data-testid="connection-banner" role="status" style={CONNECTING}>
        <strong>Connecting to codypendentd…</strong> {detail}
      </div>
    );
  }

  const manual = launch?.manualCommand ?? "codypendent daemon start";
  const found = launch?.invocation?.program ?? null;
  const start = async () => {
    setStarting(true);
    setOutcome(null);
    try {
      setOutcome(await onStart());
    } finally {
      setStarting(false);
    }
  };

  return (
    <div data-testid="connection-banner" role="alert" style={DISCONNECTED}>
      <div>
        <strong>
          {hasTransport ? "Not connected to codypendentd. Reconnecting…" : "Not connected to codypendentd."}
        </strong>{" "}
        <span style={RAW}>{detail}</span>
      </div>
      {hasTransport && (
        <div style={GUIDANCE}>
          {launch && !launch.listening ? (
            found ? (
              <>
                No daemon is listening on <span style={CODE}>{launch.socketPath}</span>. Start it
                here — the shell runs <span style={CODE}>{found}</span> for you — or run{" "}
                <span style={CODE}>{manual}</span> in a terminal.
              </>
            ) : (
              <>
                No daemon is listening, and the <span style={CODE}>codypendent</span> program was
                not found on this machine (looked on PATH and in the usual install directories).
                Install Codypendent, then run <span style={CODE}>{manual}</span> in a terminal, or
                set <span style={CODE}>CODYPENDENT_BINARY</span> to the binary and start it here.
              </>
            )
          ) : (
            <>
              The daemon keeps its own process: a run in flight is safe, and the app reconnects the
              moment it answers. If nothing is running, start it here or run{" "}
              <span style={CODE}>{manual}</span> in a terminal.
            </>
          )}
        </div>
      )}
      {hasTransport && (
        <div style={ACTIONS}>
          {canStart && (
            <button
              type="button"
              style={PRIMARY}
              disabled={starting}
              onClick={() => void start()}
              data-testid="start-daemon"
            >
              {starting ? "Starting daemon…" : "Start daemon"}
            </button>
          )}
          <button type="button" style={SECONDARY} onClick={onRetry} data-testid="retry-connect">
            Retry now
          </button>
          {outcome && (
            <span
              role="status"
              data-testid="start-daemon-outcome"
              style={{ color: outcome.started ? "var(--cody-text, #e6edf3)" : "inherit" }}
            >
              {outcome.detail}
            </span>
          )}
        </div>
      )}
    </div>
  );
};
