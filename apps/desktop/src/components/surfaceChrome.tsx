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
    background: "var(--cody-canvas)",
  } as React.CSSProperties,
  header: {
    padding: "16px 24px",
    borderBottom: "1px solid var(--cody-border)",
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 12,
  } as React.CSSProperties,
  title: { fontSize: 16, fontWeight: 600, color: "var(--cody-text)" } as React.CSSProperties,
  subtitle: { fontSize: 12, color: "var(--cody-text-muted)", marginTop: 2 } as React.CSSProperties,
  scroll: { flex: 1, overflowY: "auto", padding: "16px 24px" } as React.CSSProperties,
  card: {
    border: "1px solid var(--cody-border)",
    borderRadius: 8,
    background: "var(--cody-panel-raised)",
    padding: 12,
    marginBottom: 10,
  } as React.CSSProperties,
  meta: { fontSize: 11, color: "var(--cody-text-muted)", marginTop: 4 } as React.CSSProperties,
  mono: {
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
    fontSize: 12,
  } as React.CSSProperties,
};

export const surfaceButton = (tone: "neutral" | "danger" | "primary" = "neutral"): React.CSSProperties => ({
  background: tone === "danger" ? "var(--cody-danger-bg)" : tone === "primary" ? "var(--cody-success-strong)" : "var(--cody-inset)",
  color: tone === "danger" ? "var(--cody-danger-text)" : "var(--cody-text)",
  border: `1px solid ${tone === "danger" ? "var(--cody-danger)" : "var(--cody-border-strong)"}`,
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
          border: "1px solid var(--cody-warning-border)",
          background: "var(--cody-warning-bg)",
          color: "var(--cody-warning-text)",
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
    return <div style={{ color: "var(--cody-text-faint)", fontSize: 13 }}>Not read yet.</div>;
  }
  if (status === "loading") {
    return (
      <div role="status" style={{ color: "var(--cody-text-muted)", fontSize: 13 }}>
        Reading…
      </div>
    );
  }
  if (count === 0) {
    return <div style={{ color: "var(--cody-text-faint)", fontSize: 13 }}>{emptyMessage}</div>;
  }
  return <>{children}</>;
};

/** A dim label / value pair used across the surface cards. */
export const Field: React.FC<{ label: string; value: React.ReactNode }> = ({ label, value }) => (
  <span style={{ fontSize: 11, color: "var(--cody-text-muted)", marginRight: 12 }}>
    {label} <span style={{ color: "var(--cody-text-secondary)" }}>{value}</span>
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
      border: "1px solid var(--cody-danger)",
      background: "var(--cody-danger-bg)",
      borderRadius: 8,
      padding: 14,
      marginBottom: 12,
    }}
  >
    <div style={{ color: "var(--cody-danger-text)", fontSize: 13, fontWeight: 600, marginBottom: 8 }}>{title}</div>
    {evidence !== undefined && (
      <pre
        style={{
          ...surfaceStyles.mono,
          whiteSpace: "pre-wrap",
          wordBreak: "break-word",
          color: "var(--cody-text-secondary)",
          background: "var(--cody-canvas)",
          border: "1px solid var(--cody-border-strong)",
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
