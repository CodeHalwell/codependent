/**
 * The command palette — the desktop's port of `Overlay::Palette`.
 *
 * The ranking is `crates/tui/src/palette.rs::palette_match_score`, ported
 * exactly, so `mode` beats `model` here the same way it does in the TUI. The
 * table itself is NOT hard-coded in this module: the app passes the rows for
 * the views it actually mounts, so the palette can never advertise a surface
 * that does not exist (`palette.rs`: "a front door to existing commands, never
 * a second code path").
 */
import React, { useEffect, useMemo, useRef, useState } from "react";

export interface PaletteEntry {
  /** Stable id the app dispatches on. */
  id: string;
  /** What the user reads and matches on. */
  title: string;
  description: string;
  /** The keyboard equivalent, or `"—"` when there is none. */
  key: string;
  /** `Setup` / `Run` / `Models` / `Workspace` / `Session`. */
  group: string;
}

/**
 * Lower is a stronger match. A verbatim port of `palette_match_score`
 * (`crates/tui/src/palette.rs`) — exact title 0, exact word 1, key 2, title
 * prefix 3, title substring 4, description prefix 5, description substring 6.
 */
export function paletteMatchScore(entry: PaletteEntry, needle: string): number | null {
  const title = entry.title.toLowerCase();
  const description = entry.description.toLowerCase();
  const key = entry.key.toLowerCase();

  if (title === needle) return 0;
  if (title.split(/[^\p{L}\p{N}]/u).some((word) => word === needle)) return 1;
  if (key === needle) return 2;
  if (title.startsWith(needle)) return 3;
  if (title.includes(needle)) return 4;
  if (description.startsWith(needle)) return 5;
  if (description.includes(needle)) return 6;
  return null;
}

/** The entries matching `query`, ranked by intent; table order breaks ties. */
export function filterPalette(
  entries: readonly PaletteEntry[],
  query: string,
): PaletteEntry[] {
  const needle = query.trim().toLowerCase();
  if (needle.length === 0) {
    return [...entries];
  }
  return entries
    .map((entry, tableIndex) => ({ entry, tableIndex, score: paletteMatchScore(entry, needle) }))
    .filter((row): row is { entry: PaletteEntry; tableIndex: number; score: number } =>
      row.score !== null,
    )
    .sort((left, right) => left.score - right.score || left.tableIndex - right.tableIndex)
    .map((row) => row.entry);
}

export interface CommandPaletteProps {
  open: boolean;
  entries: readonly PaletteEntry[];
  onRun: (id: string) => void;
  onClose: () => void;
}

export const CommandPalette: React.FC<CommandPaletteProps> = ({
  open,
  entries,
  onRun,
  onClose,
}) => {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);

  const matches = useMemo(() => filterPalette(entries, query), [entries, query]);

  useEffect(() => {
    if (open) {
      setQuery("");
      setSelected(0);
      inputRef.current?.focus();
    }
  }, [open]);

  if (!open) {
    return null;
  }

  const handleKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setSelected((current) => (matches.length === 0 ? 0 : (current + 1) % matches.length));
      return;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      setSelected((current) =>
        matches.length === 0 ? 0 : (current - 1 + matches.length) % matches.length,
      );
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      // A zero-match filter runs NOTHING. It must not fall back to row 0 —
      // the TUI pickers carry the same guard, and it was a real bug there.
      const entry = matches[selected];
      if (entry) {
        onRun(entry.id);
      }
    }
  };

  // Group labels are rendered only while the query is empty: filtering can
  // straddle groups, so the labels would mislead (`palette.rs`).
  const showGroups = query.trim().length === 0;
  let lastGroup: string | null = null;

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(1, 4, 9, 0.6)",
        display: "flex",
        alignItems: "flex-start",
        justifyContent: "center",
        paddingTop: "12vh",
        zIndex: 1000,
      }}
      onClick={onClose}
    >
      <div
        role="dialog"
        aria-label="Command palette"
        onClick={(event) => event.stopPropagation()}
        style={{
          width: "min(680px, 90vw)",
          maxHeight: "70vh",
          display: "flex",
          flexDirection: "column",
          background: "#161b22",
          border: "1px solid #30363d",
          borderRadius: 10,
          overflow: "hidden",
          boxShadow: "0 16px 48px rgba(1, 4, 9, 0.7)",
        }}
      >
        <input
          ref={inputRef}
          value={query}
          aria-label="Command palette filter"
          placeholder="Run a command…"
          onChange={(event) => {
            setQuery(event.target.value);
            setSelected(0);
          }}
          onKeyDown={handleKeyDown}
          style={{
            background: "#0d1117",
            border: "none",
            borderBottom: "1px solid #30363d",
            outline: "none",
            color: "#e6edf3",
            fontSize: 14,
            padding: "12px 16px",
          }}
        />
        <div style={{ overflowY: "auto" }}>
          {matches.length === 0 ? (
            <div style={{ padding: 16, color: "#6e7681", fontSize: 13 }}>
              No command matches “{query.trim()}”.
            </div>
          ) : (
            matches.map((entry, index) => {
              const groupLabel = showGroups && entry.group !== lastGroup ? entry.group : null;
              lastGroup = entry.group;
              const active = index === selected;
              return (
                <React.Fragment key={entry.id}>
                  {groupLabel && (
                    <div
                      style={{
                        padding: "10px 16px 4px",
                        fontSize: 10,
                        letterSpacing: 0.6,
                        textTransform: "uppercase",
                        color: "#6e7681",
                      }}
                    >
                      {groupLabel}
                    </div>
                  )}
                  <button
                    onMouseEnter={() => setSelected(index)}
                    onClick={() => onRun(entry.id)}
                    aria-label={entry.title}
                    style={{
                      display: "flex",
                      width: "100%",
                      alignItems: "baseline",
                      justifyContent: "space-between",
                      gap: 12,
                      textAlign: "left",
                      padding: "8px 16px",
                      border: "none",
                      borderLeft: `2px solid ${active ? "#388bfd" : "transparent"}`,
                      background: active ? "#1f242c" : "transparent",
                      color: "#e6edf3",
                      cursor: "pointer",
                    }}
                  >
                    <span style={{ minWidth: 0 }}>
                      <span style={{ fontSize: 13, fontWeight: active ? 600 : 400 }}>
                        {entry.title}
                      </span>
                      <span
                        style={{
                          display: "block",
                          fontSize: 11,
                          color: "#8b949e",
                          marginTop: 2,
                        }}
                      >
                        {entry.description}
                      </span>
                    </span>
                    <span style={{ fontSize: 11, color: "#6e7681", whiteSpace: "nowrap" }}>
                      {entry.key}
                    </span>
                  </button>
                </React.Fragment>
              );
            })
          )}
        </div>
      </div>
    </div>
  );
};
