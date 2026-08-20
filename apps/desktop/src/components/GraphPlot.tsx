import React from "react";
import type { CodeGraphStatusView } from "@codypendent/protocol";

/**
 * The stored code graph, drawn.
 *
 * `ReadCodeGraphStatus` already carries everything this needs — totals plus
 * per-language and per-kind tallies — so the shape of a repository is one read
 * away and was only ever shown as numbers. Nothing here re-queries; it is a
 * view of the status the panel has already fetched.
 *
 * Deliberately bars rather than a node-link diagram. A real repository graph
 * runs to hundreds of thousands of nodes and over a million edges — the reason
 * the inspector beside it is paged at all — so a force-directed layout of the
 * whole thing is not a picture anyone can read, and a layout of an arbitrary
 * few hundred nodes is a picture of the sample rather than of the repository.
 * Composition is the question this data can honestly answer.
 *
 * Plain SVG and divs: no charting dependency, and nothing here is fed by model
 * output.
 */

const CARD: React.CSSProperties = {
  background: "#0d1117",
  border: "1px solid #21262d",
  borderRadius: 8,
  padding: "14px 16px",
};

const LABEL: React.CSSProperties = {
  fontSize: 11,
  color: "#8b949e",
  textTransform: "uppercase",
  letterSpacing: 0.6,
};

const TOTAL: React.CSSProperties = { fontSize: 22, fontWeight: 700, color: "#e6edf3" };

/** Distinct enough to tell apart, dim enough not to shout. */
const SERIES = ["#58a6ff", "#3fb950", "#d29922", "#bc8cff", "#f778ba", "#39c5cf", "#8b949e"];

interface Tally {
  readonly label: string;
  readonly count: number;
}

function Bars({ title, rows }: { title: string; rows: Tally[] }): React.JSX.Element {
  const total = rows.reduce((sum, row) => sum + row.count, 0);
  if (total === 0) {
    return (
      <div style={CARD}>
        <div style={LABEL}>{title}</div>
        <div style={{ marginTop: 8, color: "#6e7681", fontSize: 12 }}>nothing recorded</div>
      </div>
    );
  }
  // Widths are a share of the LARGEST row, not of the total: with one dominant
  // language every other bar would otherwise round to invisible.
  const largest = Math.max(...rows.map((row) => row.count));
  return (
    <div style={CARD}>
      <div style={LABEL}>{title}</div>
      <div style={{ marginTop: 10, display: "flex", flexDirection: "column", gap: 8 }}>
        {rows.map((row, index) => (
          <div key={row.label}>
            <div
              style={{
                display: "flex",
                justifyContent: "space-between",
                fontSize: 12,
                color: "#c9d1d9",
                marginBottom: 3,
              }}
            >
              <span>{row.label}</span>
              <span style={{ color: "#8b949e" }}>
                {row.count.toLocaleString()} · {Math.round((row.count / total) * 100)}%
              </span>
            </div>
            <div style={{ height: 8, background: "#161b22", borderRadius: 4, overflow: "hidden" }}>
              <div
                data-testid={`bar-${row.label}`}
                style={{
                  height: "100%",
                  width: `${Math.max(2, (row.count / largest) * 100)}%`,
                  background: SERIES[index % SERIES.length],
                  borderRadius: 4,
                }}
              />
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

export function GraphPlot({ status }: { status: CodeGraphStatusView }): React.JSX.Element {
  const languages: Tally[] = status.by_language
    .map((row) => ({ label: row.language, count: Number(row.nodes) }))
    .sort((left, right) => right.count - left.count)
    .slice(0, 8);
  const kinds: Tally[] = status.by_kind
    .map((row) => ({ label: row.label, count: Number(row.count) }))
    .sort((left, right) => right.count - left.count)
    .slice(0, 8);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }} data-testid="graph-plot">
      <div style={{ display: "flex", gap: 12, flexWrap: "wrap" }}>
        {(
          [
            ["Nodes", status.nodes],
            ["Edges", status.edges],
            ["Files", status.files],
          ] as const
        ).map(([label, value]) => (
          <div key={label} style={{ ...CARD, flex: "1 1 120px" }}>
            <div style={LABEL}>{label}</div>
            <div style={TOTAL}>{Number(value).toLocaleString()}</div>
          </div>
        ))}
      </div>
      <div style={{ display: "flex", gap: 12, flexWrap: "wrap" }}>
        <div style={{ flex: "1 1 280px" }}>
          <Bars title="By language" rows={languages} />
        </div>
        <div style={{ flex: "1 1 280px" }}>
          <Bars title="By kind" rows={kinds} />
        </div>
      </div>
      {status.stale && (
        <div style={{ ...CARD, borderColor: "#9e6a03", color: "#d29922", fontSize: 12 }}>
          This graph does not describe the current working tree
          {status.stale_reason ? `: ${status.stale_reason}` : "."}
        </div>
      )}
    </div>
  );
}
