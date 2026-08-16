import React, { useState, KeyboardEvent } from "react";

interface ComposerProps {
  onSend: (text: string) => void;
  onCancel?: () => void;
  isRunning: boolean;
  disabled?: boolean;
}

export const Composer: React.FC<ComposerProps> = ({ onSend, onCancel, isRunning, disabled }) => {
  const [input, setInput] = useState("");

  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (input.trim() && !disabled && !isRunning) {
        onSend(input.trim());
        setInput("");
      }
    }
  };

  const handleSend = () => {
    if (input.trim() && !disabled && !isRunning) {
      onSend(input.trim());
      setInput("");
    }
  };

  return (
    <div
      style={{
        padding: "16px 24px 20px",
        background: "#16191f",
        borderTop: "1px solid #282e39",
      }}
    >
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          background: "#0d1117",
          border: "1px solid #30363d",
          borderRadius: 8,
          overflow: "hidden",
        }}
      >
        <textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={isRunning ? "Run in progress..." : "Ask Codypendent or describe a task (Enter to send, Shift+Enter for newline)..."}
          disabled={disabled || isRunning}
          rows={3}
          style={{
            width: "100%",
            boxSizing: "border-box",
            background: "transparent",
            border: "none",
            outline: "none",
            color: "#e6edf3",
            padding: "12px 16px",
            fontSize: 14,
            resize: "none",
            fontFamily: "inherit",
          }}
        />
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            padding: "8px 12px",
            background: "#161b22",
            borderTop: "1px solid #21262d",
          }}
        >
          <span style={{ fontSize: 12, color: "#8b949e" }}>Build Mode · Active Scope</span>
          <div style={{ display: "flex", gap: 8 }}>
            {isRunning && onCancel && (
              <button
                onClick={onCancel}
                style={{
                  background: "#da3633",
                  border: "none",
                  color: "#fff",
                  padding: "6px 14px",
                  borderRadius: 6,
                  fontSize: 12,
                  cursor: "pointer",
                  fontWeight: 600,
                }}
              >
                Cancel Run
              </button>
            )}
            <button
              onClick={handleSend}
              disabled={!input.trim() || isRunning || disabled}
              style={{
                background: input.trim() && !isRunning ? "#238636" : "#21262d",
                color: input.trim() && !isRunning ? "#fff" : "#484f58",
                border: "none",
                padding: "6px 14px",
                borderRadius: 6,
                fontSize: 12,
                cursor: input.trim() && !isRunning ? "pointer" : "default",
                fontWeight: 600,
              }}
            >
              Send
            </button>
          </div>
        </div>
      </div>
    </div>
  );
};
