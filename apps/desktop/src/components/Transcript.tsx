import React, { useRef, useEffect } from "react";
import type { ConnectionStatus, TranscriptItem } from "../types.js";

interface TranscriptProps {
  items: TranscriptItem[];
  connectionStatus?: ConnectionStatus;
  onApprove?: (approvalId: string) => void;
  onReject?: (approvalId: string) => void;
}

export const Transcript: React.FC<TranscriptProps> = ({ items, connectionStatus = "connected", onApprove, onReject }) => {
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [items]);

  return (
    <div style={{ flex: 1, overflowY: "auto", padding: "24px 32px", display: "flex", flexDirection: "column", gap: 16 }}>
      {items.length === 0 ? (
        <div style={{ margin: "auto", textAlign: "center", color: "#6e7681" }}>
          <h3 style={{ margin: "0 0 8px 0", color: "#c9d1d9" }}>
            {connectionStatus === "disconnected" ? "Daemon disconnected" : "Ready"}
          </h3>
          <p style={{ margin: 0, fontSize: 14 }}>
            {connectionStatus === "disconnected"
              ? "Start codypendentd to make it available. This desktop build does not include daemon discovery yet."
              : "Start a run with an objective below."}
          </p>
        </div>
      ) : (
        items.map((item) => {
          switch (item.type) {
            case "user":
              return (
                <div
                  key={item.id}
                  style={{
                    alignSelf: "flex-end",
                    maxWidth: "75%",
                    background: "#1f242c",
                    border: "1px solid #388bfd",
                    padding: "12px 16px",
                    borderRadius: "12px 12px 2px 12px",
                    fontSize: 14,
                    lineHeight: 1.5,
                  }}
                >
                  {item.text}
                </div>
              );

            case "assistant":
              return (
                <div
                  key={item.id}
                  style={{
                    alignSelf: "flex-start",
                    maxWidth: "85%",
                    background: "#16191f",
                    border: "1px solid #282e39",
                    padding: "16px 20px",
                    borderRadius: "12px 12px 12px 2px",
                    fontSize: 14,
                    lineHeight: 1.6,
                    color: "#e6edf3",
                    whiteSpace: "pre-wrap",
                  }}
                >
                  {item.text}
                </div>
              );

            case "thought":
              return (
                <details
                  key={item.id}
                  style={{
                    alignSelf: "flex-start",
                    maxWidth: "85%",
                    background: "#121417",
                    border: "1px solid #21262d",
                    padding: "8px 12px",
                    borderRadius: 6,
                    fontSize: 12,
                    color: "#8b949e",
                  }}
                >
                  <summary style={{ cursor: "pointer", fontWeight: 500 }}>Thought Process</summary>
                  <div style={{ marginTop: 8, whiteSpace: "pre-wrap" }}>{item.text}</div>
                </details>
              );

            case "tool_call":
              return (
                <div
                  key={item.id}
                  style={{
                    alignSelf: "flex-start",
                    width: "85%",
                    background: "#0d1117",
                    border: "1px solid #30363d",
                    borderRadius: 8,
                    overflow: "hidden",
                  }}
                >
                  <div
                    style={{
                      background: "#161b22",
                      padding: "8px 12px",
                      fontSize: 12,
                      fontWeight: 600,
                      color: "#58a6ff",
                      display: "flex",
                      justifyContent: "space-between",
                      borderBottom: "1px solid #30363d",
                    }}
                  >
                    <span>Tool: {item.toolName}</span>
                    <span style={{ color: item.status === "running" ? "#d29922" : "#3fb950" }}>
                      {item.status ?? "running"}
                    </span>
                  </div>
                  {item.toolArgs && (
                    <pre style={{ margin: 0, padding: 12, fontSize: 12, overflowX: "auto", color: "#8b949e" }}>
                      {JSON.stringify(item.toolArgs, null, 2)}
                    </pre>
                  )}
                </div>
              );

            case "approval":
              return (
                <div
                  key={item.id}
                  style={{
                    alignSelf: "flex-start",
                    width: "85%",
                    background: "#251a00",
                    border: "1px solid #9e6a03",
                    borderRadius: 8,
                    padding: 16,
                  }}
                >
                  <div style={{ fontWeight: 600, color: "#d29922", marginBottom: 8, fontSize: 14 }}>
                    Approval Required
                  </div>
                  <div style={{ fontSize: 13, color: "#e6edf3", marginBottom: 12 }}>{item.text}</div>
                  {item.approvalId && (
                    <div style={{ display: "flex", gap: 8 }}>
                      <button
                        onClick={() => onApprove?.(item.approvalId!)}
                        style={{
                          background: "#238636",
                          border: "none",
                          color: "#fff",
                          padding: "6px 12px",
                          borderRadius: 6,
                          fontSize: 12,
                          cursor: "pointer",
                          fontWeight: 600,
                        }}
                      >
                        Approve
                      </button>
                      <button
                        onClick={() => onReject?.(item.approvalId!)}
                        style={{
                          background: "#21262d",
                          border: "1px solid #30363d",
                          color: "#c9d1d9",
                          padding: "6px 12px",
                          borderRadius: 6,
                          fontSize: 12,
                          cursor: "pointer",
                        }}
                      >
                        Reject
                      </button>
                    </div>
                  )}
                </div>
              );

            default:
              return (
                <div key={item.id} style={{ fontSize: 12, color: "#8b949e", textAlign: "center" }}>
                  {item.text}
                </div>
              );
          }
        })
      )}
      <div ref={bottomRef} />
    </div>
  );
};
