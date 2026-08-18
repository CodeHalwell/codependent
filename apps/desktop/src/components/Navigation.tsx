import React from "react";
import type { ConnectionStatus, SessionSummary, SessionId } from "../types.js";

export type DesktopView =
  | "sessions"
  | "inbox"
  | "analytics"
  | "skills"
  | "memory"
  | "docs"
  | "plugins"
  | "context"
  | "library"
  | "workflow"
  | "board"
  | "blackboard"
  | "repository"
  | "council"
  | "models"
  | "providers"
  | "keys"
  | "mode"
  | "councilResults";

/** The views the sidebar offers, grouped the way the palette groups them. */
const NAV_GROUPS: ReadonlyArray<{ group: string; views: ReadonlyArray<[DesktopView, string]> }> = [
  {
    group: "Run",
    views: [
      ["sessions", "Sessions"],
      ["library", "Session Library"],
      ["inbox", "Inbox"],
      ["analytics", "Analytics"],
      ["context", "Context"],
    ],
  },
  {
    group: "Workflow",
    views: [
      ["workflow", "Workflow Runs"],
      ["board", "Task Board"],
      ["blackboard", "Blackboard"],
    ],
  },
  {
    group: "Workspace",
    views: [
      ["repository", "Repository"],
      ["docs", "Docs"],
      ["skills", "Skills"],
      ["memory", "Memory"],
      ["plugins", "Plugins"],
    ],
  },
  {
    // Model, provider, key and mode selection are LOCAL CONFIG (models.toml,
    // auth.json) rather than daemon protocol, so they group apart from the
    // daemon-backed views above — same reasoning as Councils below.
    group: "Configuration",
    views: [
      ["models", "Models"],
      ["providers", "Providers"],
      ["keys", "API Keys"],
      ["mode", "Mode"],
    ],
  },
  {
    // Councils are local configuration, not protocol — grouped apart from the
    // daemon-backed views so the distinction is visible in the chrome itself.
    group: "Councils",
    views: [
      ["council", "Councils"],
      ["councilResults", "Council Results"],
    ],
  },
];

interface NavigationProps {
  sessions: SessionSummary[];
  activeSessionId: SessionId | null;
  onSelectSession: (id: SessionId) => void;
  connectionStatus: ConnectionStatus;
  /**
   * Why the client is in this connection state.
   *
   * It is NOT rendered as text here any more. The dot below carries the state
   * for a glance and this string as its accessible name and tooltip; a
   * connection that is not healthy is reported by the banner across the top of
   * the main pane (`App.tsx`), where it cannot be missed. A failure hidden in
   * a 12px footer while the app looks otherwise normal was the bad outcome.
   */
  statusDetail?: string;
  currentView?: DesktopView;
  onSelectView?: (view: DesktopView) => void;
  unreadInboxCount?: number;
  /** Opens the command palette; the palette is the full command surface. */
  onOpenPalette?: () => void;
}

export const Navigation: React.FC<NavigationProps> = ({
  sessions,
  activeSessionId,
  onSelectSession,
  connectionStatus,
  statusDetail,
  currentView = "sessions",
  onSelectView,
  unreadInboxCount = 0,
  onOpenPalette,
}) => {
  const connected = connectionStatus === "connected";

  const handleSelectSession = (id: SessionId) => {
    onSelectView?.("sessions");
    onSelectSession(id);
  };

  return (
    <aside
      style={{
        width: 260,
        background: "#16191f",
        borderRight: "1px solid #282e39",
        display: "flex",
        flexDirection: "column",
        height: "100vh",
        userSelect: "none",
      }}
    >
      {/* Brand Header */}
      <div
        style={{
          padding: "16px",
          borderBottom: "1px solid #282e39",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <div
            role="img"
            aria-label={`codypendentd ${connectionStatus}`}
            title={statusDetail ?? `codypendentd ${connectionStatus}`}
            style={{
              width: 10,
              height: 10,
              borderRadius: "50%",
              background: connected ? "#2ea043" : connectionStatus === "connecting" ? "#d29922" : "#da3633",
            }}
          />
          <span style={{ fontWeight: 600, fontSize: 14, color: "#e6edf3" }}>Codypendent</span>
        </div>
        {onOpenPalette && (
          <button
            onClick={onOpenPalette}
            aria-label="Open command palette"
            title="Command palette (⌘K / Ctrl-K)"
            style={{
              background: "#21262d",
              border: "1px solid #30363d",
              borderRadius: 6,
              color: "#8b949e",
              fontSize: 11,
              padding: "3px 8px",
              cursor: "pointer",
            }}
          >
            ⌘K
          </button>
        )}
      </div>

      {/* Main Views Nav */}
      <div
        style={{
          padding: "8px 8px 8px",
          borderBottom: "1px solid #282e39",
          display: "flex",
          flexDirection: "column",
          gap: 2,
        }}
      >
        {NAV_GROUPS.map(({ group, views }) => (
          <React.Fragment key={group}>
            <div
              style={{
                fontSize: 10,
                letterSpacing: 0.6,
                textTransform: "uppercase",
                color: "#6e7681",
                padding: "8px 10px 2px",
              }}
            >
              {group}
            </div>
            {views.map(([view, label]) => {
              const active = currentView === view;
              return (
                <button
                  key={view}
                  onClick={() => onSelectView?.(view)}
                  aria-label={`${label} View`}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "space-between",
                    padding: "7px 10px",
                    borderRadius: 6,
                    background: active ? "#1f242c" : "transparent",
                    border: active ? "1px solid #388bfd" : "1px solid transparent",
                    color: "#e6edf3",
                    fontSize: 13,
                    fontWeight: active ? 600 : 400,
                    cursor: "pointer",
                    textAlign: "left",
                    width: "100%",
                  }}
                >
                  <span>{label}</span>
                  {view === "inbox" && unreadInboxCount > 0 && (
                    <span
                      data-testid="inbox-badge"
                      style={{
                        padding: "1px 6px",
                        borderRadius: 10,
                        fontSize: 11,
                        fontWeight: 600,
                        background: "#1f6feb",
                        color: "#ffffff",
                      }}
                    >
                      {unreadInboxCount}
                    </span>
                  )}
                </button>
              );
            })}
          </React.Fragment>
        ))}
      </div>

      {/* Sessions List */}
      <div style={{ flex: 1, overflowY: "auto", padding: "8px" }}>
        <div style={{ fontSize: 11, color: "#8b949e", padding: "6px 8px", textTransform: "uppercase" }}>
          Recent Sessions
        </div>
        {sessions.length === 0 ? (
          <div style={{ padding: "12px 8px", color: "#6e7681", fontSize: 13 }}>
            {connected ? "No sessions yet" : "No sessions (not connected)"}
          </div>
        ) : (
          sessions.map((session) => {
            const active = session.id === activeSessionId && currentView === "sessions";
            return (
              <div
                key={session.id}
                onClick={() => handleSelectSession(session.id)}
                style={{
                  padding: "8px 10px",
                  borderRadius: 6,
                  marginBottom: 4,
                  cursor: "pointer",
                  background: active ? "#1f242c" : "transparent",
                  border: active ? "1px solid #388bfd" : "1px solid transparent",
                }}
              >
                <div style={{ fontSize: 13, fontWeight: active ? 600 : 400, color: "#e6edf3", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
                  {session.title || "Untitled Session"}
                </div>
                <div style={{ fontSize: 11, color: "#8b949e", marginTop: 2 }}>
                  {session.state} · {session.updated_at}
                </div>
              </div>
            );
          })
        )}
      </div>
    </aside>
  );
};
