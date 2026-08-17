import React from "react";
import { AlertCircle, RefreshCw } from "lucide-react";

export interface ErrorMessageProps {
  title?: string;
  message: string;
  onRetry?: () => void;
}

export function ErrorMessage({
  title = "An error occurred",
  message,
  onRetry,
}: ErrorMessageProps): React.JSX.Element {
  return (
    <div
      role="alert"
      style={{
        backgroundColor: "var(--danger-surface)",
        border: "1px solid var(--danger)",
        borderRadius: "var(--radius-md)",
        padding: "16px",
        display: "flex",
        alignItems: "flex-start",
        gap: "12px",
        marginBottom: "16px",
      }}
    >
      <AlertCircle size={20} color="var(--danger)" style={{ flexShrink: 0, marginTop: "2px" }} />
      <div style={{ flex: 1 }}>
        <h4 style={{ fontSize: "14px", fontWeight: 600, color: "var(--danger)" }}>{title}</h4>
        <p style={{ fontSize: "13px", color: "var(--text-primary)", marginTop: "4px" }}>{message}</p>
        {onRetry && (
          <button
            className="btn btn-secondary btn-sm"
            onClick={onRetry}
            style={{ marginTop: "10px" }}
          >
            <RefreshCw size={14} /> Retry
          </button>
        )}
      </div>
    </div>
  );
}
