import React, { useRef } from "react";
import { useFocusTrap } from "../useFocusTrap.js";

/**
 * The desktop app's keyboard surface, in one card. The bindings live in
 * `App.tsx`'s window key handler and the individual dialogs; this card is the
 * only place they are written down for the operator — they used to exist
 * nowhere but a `title` tooltip on the ⌘K button.
 */
const SHORTCUTS: ReadonlyArray<{ keys: string; does: string }> = [
  { keys: "⌘K / Ctrl-K", does: "open the command palette from anywhere" },
  { keys: "/", does: "open the command palette (when not typing in a field)" },
  { keys: "Esc", does: "close the topmost overlay, else go back to the previous view" },
  { keys: "Enter", does: "send the composer message · confirm the open dialog" },
  { keys: "Shift-Enter", does: "insert a line break in the composer" },
  { keys: "↑ / ↓ + Enter", does: "move and run the palette selection" },
];

export const ShortcutsCard: React.FC<{ onClose: () => void }> = ({ onClose }) => {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  useFocusTrap(dialogRef, { active: true, onEscape: onClose });
  return (
  <div
    style={{
      position: "fixed",
      inset: 0,
      background: "rgba(1, 4, 9, 0.6)",
      display: "flex",
      alignItems: "flex-start",
      justifyContent: "center",
      paddingTop: "16vh",
      zIndex: 1000,
    }}
    onClick={onClose}
  >
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Keyboard shortcuts"
      tabIndex={-1}
      ref={dialogRef}
      onClick={(event) => event.stopPropagation()}
      style={{
        width: "min(480px, 90vw)",
        background: "#161b22",
        border: "1px solid #30363d",
        borderRadius: 10,
        boxShadow: "0 16px 48px rgba(1, 4, 9, 0.7)",
        padding: "16px 20px",
        color: "#e6edf3",
      }}
    >
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
          marginBottom: 10,
        }}
      >
        <h2 style={{ margin: 0, fontSize: 15 }}>Keyboard shortcuts</h2>
        <button
          aria-label="Close shortcuts"
          onClick={onClose}
          style={{
            background: "transparent",
            border: "none",
            color: "#8b949e",
            cursor: "pointer",
            fontSize: 14,
          }}
        >
          ×
        </button>
      </div>
      <dl style={{ margin: 0 }}>
        {SHORTCUTS.map((entry) => (
          <div
            key={entry.keys}
            style={{ display: "flex", gap: 12, padding: "5px 0", alignItems: "baseline" }}
          >
            <dt
              style={{
                minWidth: 130,
                fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
                fontSize: 12,
                color: "#58a6ff",
              }}
            >
              {entry.keys}
            </dt>
            <dd style={{ margin: 0, fontSize: 13, color: "#c9d1d9" }}>{entry.does}</dd>
          </div>
        ))}
      </dl>
    </div>
  </div>
  );
};
