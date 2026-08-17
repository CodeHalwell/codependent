import React from "react";
import type { ConnectionStatus, SessionSummary, SessionId } from "../types.js";

export type DesktopView = "sessions" | "inbox" | "analytics";

interface NavigationProps {
  sessions: SessionSummary[];
  activeSessionId: SessionId | null;
  onSelectSession: (id: SessionId) => void;
  connectionStatus: ConnectionStatus;
  /** Why the client is in this connection state — shown verbatim. */
  statusDetail?: string;
  currentView?: DesktopView;
  onSelectView?: (view: DesktopView) => void;
  unreadInboxCount?: number;
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
            style={{
              width: 10,
              height: 10,
              borderRadius: "50%",
              background: connected ? "#2ea043" : "#da3633",
            }}
          />
          <span style={{ fontWeight: 600, fontSize: 14, color: "#e6edf3" }}>Codypendent</span>
        </div>
      </div>

      {/* Main Views Nav */}
      <div style={{ padding: "12px 8px 8px", borderBottom: "1px solid #282e39", display: "flex", flexDirection: "column", gap: 4 }}>
        <button
          onClick={() => onSelectView?.("sessions")}
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            padding: "8px 10px",
            borderRadius: 6,
            background: currentView === "sessions" ? "#1f242c" : "transparent",
            border: currentView === "sessions" ? "1px solid #388bfd" : "1px solid transparent",
            color: "#e6edf3",
            fontSize: 13,
            fontWeight: currentView === "sessions" ? 600 : 400,
            cursor: "pointer",
            textAlign: "left",
            width: "100%",
          }}
          aria-label="Sessions View"
        >
          <span>Sessions</span>
        </button>

        <button
          onClick={() => onSelectView?.("inbox")}
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            padding: "8px 10px",
            borderRadius: 6,
            background: currentView === "inbox" ? "#1f242c" : "transparent",
            border: currentView === "inbox" ? "1px solid #388bfd" : "1px solid transparent",
            color: "#e6edf3",
            fontSize: 13,
            fontWeight: currentView === "inbox" ? 600 : 400,
            cursor: "pointer",
            textAlign: "left",
            width: "100%",
          }}
          aria-label="Inbox View"
        >
          <span>Inbox</span>
          {unreadInboxCount > 0 && (
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

        <button
          onClick={() => onSelectView?.("analytics")}
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            padding: "8px 10px",
            borderRadius: 6,
            background: currentView === "analytics" ? "#1f242c" : "transparent",
            border: currentView === "analytics" ? "1px solid #388bfd" : "1px solid transparent",
            color: "#e6edf3",
            fontSize: 13,
            fontWeight: currentView === "analytics" ? 600 : 400,
            cursor: "pointer",
            textAlign: "left",
            width: "100%",
          }}
          aria-label="Analytics View"
        >
          <span>Analytics</span>
        </button>
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

      {/* Footer Status */}
      <div style={{ padding: 12, borderTop: "1px solid #282e39", fontSize: 12, color: "#8b949e" }}>
        <div>codypendentd: {connectionStatus}</div>
        {statusDetail && (
          <div style={{ marginTop: 4, fontSize: 11, color: "#6e7681", wordBreak: "break-word" }}>
            {statusDetail}
          </div>
        )}
      </div>
    </aside>
  );
};
