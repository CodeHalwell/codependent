import React, { useState, KeyboardEvent } from "react";

interface ComposerProps {
  onSend: (text: string) => void;
  /**
   * REQUEST a cancellation — it opens the confirmation, it does not send
   * `CancelRun`. The command is only sent once the operator confirms, in
   * `ConfirmCancel.tsx`; this button used to fire it on the click, and a
   * misclick cost the whole run.
   */
  onRequestCancel?: () => void;
  isRunning: boolean;
  disabled?: boolean;
  /**
   * Whether a real `CancelRun` can be sent — i.e. the client is connected and
   * knows the run's id. When it cannot, the button is not offered at all.
   */
  canCancel?: boolean;
  /**
   * Whether a real `QueueSteering` can be sent — connected, with a live run
   * id. When it cannot, the Steer button is not offered at all, because there
   * is nothing it could truthfully do.
   */
  canSteer?: boolean;
  /** Whether the steering panel is currently open (rendered by the caller). */
  steeringOpen?: boolean;
  /** Open or close the steering panel. */
  onToggleSteering?: () => void;
}

export const Composer: React.FC<ComposerProps> = ({
  onSend,
  onRequestCancel,
  isRunning,
  disabled,
  canCancel,
  canSteer,
  steeringOpen,
  onToggleSteering,
}) => {
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
          placeholder={disabled ? "Not connected to codypendentd — runs cannot be submitted." : isRunning ? "Run in progress..." : "Ask Codypendent or describe a task (Enter to send, Shift+Enter for newline)..."}
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
          <span style={{ fontSize: 12, color: "#8b949e" }}>{disabled ? "Not connected" : "Build Mode"}</span>
          <div style={{ display: "flex", gap: 8 }}>
            {/*
              Steering lives here rather than behind a nav entry: while a run
              is live the composer's own textarea is disabled, so this is
              exactly where an operator reaches for a way to change the
              agent's course.
            */}
            {isRunning && canSteer && onToggleSteering && (
              <button
                onClick={onToggleSteering}
                aria-expanded={Boolean(steeringOpen)}
                data-testid="composer-steer"
                style={{
                  background: steeringOpen ? "#1f6feb" : "#21262d",
                  border: `1px solid ${steeringOpen ? "#1f6feb" : "#30363d"}`,
                  color: steeringOpen ? "#fff" : "#e6edf3",
                  padding: "6px 14px",
                  borderRadius: 6,
                  fontSize: 12,
                  cursor: "pointer",
                  fontWeight: 600,
                }}
              >
                {steeringOpen ? "Hide steering" : "Steer Run"}
              </button>
            )}
            {isRunning && canCancel && onRequestCancel && (
              <button
                onClick={onRequestCancel}
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
                background: input.trim() && !isRunning && !disabled ? "#238636" : "#21262d",
                color: input.trim() && !isRunning && !disabled ? "#fff" : "#484f58",
                border: "none",
                padding: "6px 14px",
                borderRadius: 6,
                fontSize: 12,
                cursor: input.trim() && !isRunning && !disabled ? "pointer" : "default",
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
