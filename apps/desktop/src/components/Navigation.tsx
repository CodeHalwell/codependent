import React from "react";
import type { SessionSummary, SessionId } from "../types.js";

interface NavigationProps {
  sessions: SessionSummary[];
  activeSessionId: SessionId | null;
  onSelectSession: (id: SessionId) => void;
  onCreateSession: () => void;
  connected: boolean;
}

export const Navigation: React.FC<NavigationProps> = ({
  sessions,
  activeSessionId,
  onSelectSession,
  onCreateSession,
  connected,
}) => {
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
          <span style={{ fontWeight: 600, fontSize: 14 }}>Codypendent</span>
        </div>
        <button
          onClick={onCreateSession}
          style={{
            background: "#21262d",
            border: "1px solid #30363d",
            color: "#c9d1d9",
            borderRadius: 6,
            padding: "4px 8px",
            fontSize: 12,
            cursor: "pointer",
          }}
        >
          + New
        </button>
      </div>

      <div style={{ flex: 1, overflowY: "auto", padding: "8px" }}>
        <div style={{ fontSize: 11, color: "#8b949e", padding: "6px 8px", textTransform: "uppercase" }}>
          Recent Sessions
        </div>
        {sessions.length === 0 ? (
          <div style={{ padding: "12px 8px", color: "#6e7681", fontSize: 13 }}>No sessions yet</div>
        ) : (
          sessions.map((session) => {
            const active = session.id === activeSessionId;
            return (
              <div
                key={session.id}
                onClick={() => onSelectSession(session.id)}
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
                  {session.run_count} run{session.run_count === 1 ? "" : "s"}
                </div>
              </div>
            );
          })
        )}
      </div>

      <div style={{ padding: 12, borderTop: "1px solid #282e39", fontSize: 12, color: "#8b949e" }}>
        codypendentd: {connected ? "connected" : "disconnected"}
      </div>
    </aside>
  );
};
