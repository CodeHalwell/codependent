import React from "react";
import { Loader2 } from "lucide-react";

export interface LoadingSpinnerProps {
  label?: string;
  size?: number;
}

export function LoadingSpinner({
  label = "Loading...",
  size = 24,
}: LoadingSpinnerProps): React.JSX.Element {
  return (
    <div
      role="status"
      aria-live="polite"
      style={{
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        padding: "32px",
        gap: "10px",
        color: "var(--text-secondary)",
      }}
    >
      <Loader2 size={size} className="spin" style={{ animation: "spin 1s linear infinite" }} />
      <span style={{ fontSize: "13px" }}>{label}</span>
      <style>{`
        @keyframes spin {
          from { transform: rotate(0deg); }
          to { transform: rotate(360deg); }
        }
      `}</style>
    </div>
  );
}
