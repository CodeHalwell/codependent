/**
 * Shared chrome for the knowledge surfaces.
 *
 * The important piece here is {@link SurfaceBody}: it is the single place that
 * decides between "could not read" and "read, and there is nothing". Every
 * surface routes through it so the distinction cannot be lost by accident in
 * one view's JSX (`InboxView.tsx` established the idiom; this generalises it).
 */
import React from "react";
import type { LoadStatus } from "./knowledgeTransport.js";

export const surfaceStyles = {
  page: {
    flex: 1,
    display: "flex",
    flexDirection: "column",
    height: "100%",
    overflow: "hidden",
    background: "#0d1117",
  } as React.CSSProperties,
  header: {
    padding: "16px 24px",
    borderBottom: "1px solid #282e39",
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 12,
  } as React.CSSProperties,
  title: { fontSize: 16, fontWeight: 600, color: "#e6edf3" } as React.CSSProperties,
  subtitle: { fontSize: 12, color: "#8b949e", marginTop: 2 } as React.CSSProperties,
  scroll: { flex: 1, overflowY: "auto", padding: "16px 24px" } as React.CSSProperties,
  card: {
    border: "1px solid #282e39",
    borderRadius: 8,
    background: "#161b22",
    padding: 12,
    marginBottom: 10,
  } as React.CSSProperties,
  meta: { fontSize: 11, color: "#8b949e", marginTop: 4 } as React.CSSProperties,
  mono: {
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
    fontSize: 12,
  } as React.CSSProperties,
};

export const surfaceButton = (tone: "neutral" | "danger" | "primary" = "neutral"): React.CSSProperties => ({
  background: tone === "danger" ? "#4a1d1d" : tone === "primary" ? "#238636" : "#21262d",
  color: tone === "danger" ? "#ffa198" : "#e6edf3",
  border: `1px solid ${tone === "danger" ? "#da3633" : "#30363d"}`,
  borderRadius: 6,
  padding: "4px 10px",
  fontSize: 12,
  cursor: "pointer",
});

export interface SurfaceBodyProps {
  status: LoadStatus;
  /** Why the read failed. Shown verbatim when `status` is `"unavailable"`. */
  detail: string | null;
  /** How many rows the read returned. */
  count: number;
  /** The sentence shown when the read succeeded and returned nothing. */
  emptyMessage: string;
  children: React.ReactNode;
}

/**
 * Renders `children` only once a read actually succeeded and returned rows.
 *
 * `"unavailable"` shows the reason and never the empty state; `"unloaded"` and
 * `"loading"` say so rather than implying an answer.
 */
export const SurfaceBody: React.FC<SurfaceBodyProps> = ({
  status,
  detail,
  count,
  emptyMessage,
  children,
}) => {
  if (status === "unavailable") {
    return (
      <div
        role="status"
        style={{
          border: "1px solid #9e6a03",
          background: "#2b2109",
          color: "#e3b341",
          borderRadius: 8,
          padding: 14,
          fontSize: 13,
          lineHeight: 1.5,
        }}
      >
        {detail ?? "Unavailable, with no reason reported."}
      </div>
    );
  }
  if (status === "unloaded") {
    return <div style={{ color: "#6e7681", fontSize: 13 }}>Not read yet.</div>;
  }
  if (status === "loading") {
    return (
      <div role="status" style={{ color: "#8b949e", fontSize: 13 }}>
        Reading…
      </div>
    );
  }
  if (count === 0) {
    return <div style={{ color: "#6e7681", fontSize: 13 }}>{emptyMessage}</div>;
  }
  return <>{children}</>;
};

/** A dim label / value pair used across the surface cards. */
export const Field: React.FC<{ label: string; value: React.ReactNode }> = ({ label, value }) => (
  <span style={{ fontSize: 11, color: "#8b949e", marginRight: 12 }}>
    {label} <span style={{ color: "#c9d1d9" }}>{value}</span>
  </span>
);

/**
 * A host-owned confirmation.
 *
 * Trust transitions (enabling, approving, revoking a plugin; deleting a
 * learning; publishing a document) never fire straight off a click. The
 * evidence the operator is consenting to travels ON this prompt, so the
 * decisive moment never loses it.
 */
export const ConfirmPanel: React.FC<{
  title: string;
  /** The exact daemon-supplied evidence. Rendered verbatim, never summarised. */
  evidence?: React.ReactNode;
  confirmLabel: string;
  tone?: "neutral" | "danger" | "primary";
  onConfirm: () => void;
  onCancel: () => void;
}> = ({ title, evidence, confirmLabel, tone = "danger", onConfirm, onCancel }) => (
  <div
    role="dialog"
    aria-label={title}
    style={{
      border: "1px solid #da3633",
      background: "#1b1113",
      borderRadius: 8,
      padding: 14,
      marginBottom: 12,
    }}
  >
    <div style={{ color: "#ffa198", fontSize: 13, fontWeight: 600, marginBottom: 8 }}>{title}</div>
    {evidence !== undefined && (
      <pre
        style={{
          ...surfaceStyles.mono,
          whiteSpace: "pre-wrap",
          wordBreak: "break-word",
          color: "#c9d1d9",
          background: "#0d1117",
          border: "1px solid #30363d",
          borderRadius: 6,
          padding: 10,
          margin: "0 0 10px",
        }}
      >
        {evidence}
      </pre>
    )}
    <div style={{ display: "flex", gap: 8 }}>
      <button style={surfaceButton(tone)} onClick={onConfirm}>
        {confirmLabel}
      </button>
      <button style={surfaceButton()} onClick={onCancel}>
        Cancel
      </button>
    </div>
  </div>
);
