/**
 * The code-graph inspector — the TUI's `Overlay::Edges` and
 * `Overlay::EdgeSearch(String)`.
 *
 * # This surface is paged on purpose
 *
 * A real repository graph runs to hundreds of thousands of nodes and over a
 * million edges. The TUI never loads it whole: `scroll_transcript` calls
 * `request_edge_page` with `EDGE_PAGE_SIZE` (`crates/tui/src/state.rs`, 100).
 * The daemon draws the same line from the other side — `MAX_GRAPH_PAGE` is 500
 * in `crates/codypendentd/src/codegraph_ops.rs`, and a `limit` of 0 asks for
 * that ceiling rather than for "everything", because the 16 MiB frame is a
 * wall, not a policy. So this panel ALWAYS sends an explicit limit and always
 * renders the reply's own `total_nodes` / `total_edges` (which the daemon
 * computes BEFORE the limit) as the `M` in "showing N of M".
 *
 * # A cut page must not read as the whole set
 *
 * `CodeGraphPage` carries no cursor and `CodeGraphQuery` has no offset — the
 * only fields that exist are the narrowing filters and `limit`
 * (`crates/protocol/src/codegraph.rs`). There is therefore no "next page" this
 * client could honestly offer: when the daemon's totals exceed what it sent,
 * the page is CUT and the only way to reach the rest is a narrower filter. That
 * is said in as many words on the banner rather than hidden behind a disabled
 * button, and the daemon's applied `limit` (after its own clamp) is shown
 * beside the requested one so a silently clamped request is visible.
 *
 * # Nothing here is synthesised
 *
 * An empty graph is a real answer and is rendered as one, with the command that
 * folds it (`codypendent graph build`) named — the status read says whether the
 * graph exists at all, so "read, and there is nothing" and "could not read" are
 * never merged. Counts are printed only from a reply that actually arrived; an
 * unread graph shows no numbers rather than zeros.
 */
import React, { useCallback, useEffect, useRef, useState } from "react";
import type {
  CodeGraphEdgeView,
  CodeGraphNodeView,
  CodeGraphPage,
  CodeGraphQuery,
  CodeGraphStatusView,
} from "@codypendent/protocol";
import type { DesktopTransport } from "../transport.js";
import { GraphPlot } from "./GraphPlot.js";
import { Field, surfaceButton, surfaceStyles } from "./surfaceChrome.js";

export interface EdgesViewProps {
  transport: DesktopTransport | null;
  /** Why there is no transport, shown instead of an empty graph. */
  unavailable?: string | null;
}

/**
 * The page size this panel asks for, matching the TUI's `EDGE_PAGE_SIZE`.
 * The daemon clamps anything above its own ceiling; the reply says what it
 * actually applied.
 */
const PAGE_SIZE = 100;

type Read<T> =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "loaded"; value: T }
  /** The read FAILED. Not the same fact as a graph with nothing in it. */
  | { status: "failed"; detail: string };

interface Filters {
  /** Case-insensitive substring of the qualified name — the edge search box. */
  name: string;
  /** Repo-relative path prefix, matched against `code_nodes.source_path`. */
  path: string;
  /** Exact stored language scalar (`rust`, `python`, …). */
  language: string;
  /** Exact stored node-kind scalar (`function`, `type`, …). */
  kind: string;
}

const EMPTY_FILTERS: Filters = { name: "", path: "", language: "", kind: "" };

function describe(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

/** The protocol query, with every empty field left OFF the wire. */
export function buildQuery(filters: Filters, limit: number): CodeGraphQuery {
  const query: CodeGraphQuery = {
    include_nodes: true,
    include_edges: true,
    limit,
  };
  const name = filters.name.trim();
  const path = filters.path.trim();
  const language = filters.language.trim();
  const kind = filters.kind.trim();
  if (name) query.name = name;
  if (path) query.path = path;
  if (language) query.language = language;
  if (kind) query.kind = kind;
  return query;
}

/** True when the daemon's own total exceeds the rows it was able to send. */
export function isCut(shown: number, total: number): boolean {
  return total > shown;
}

const label: React.CSSProperties = {
  fontSize: 11,
  color: "#8b949e",
  display: "block",
  marginBottom: 3,
};

const input: React.CSSProperties = {
  background: "#0d1117",
  border: "1px solid #30363d",
  borderRadius: 6,
  color: "#e6edf3",
  fontSize: 12,
  padding: "5px 8px",
  width: "100%",
  boxSizing: "border-box",
};

const banner = (tone: "warn" | "info"): React.CSSProperties => ({
  border: `1px solid ${tone === "warn" ? "#9e6a03" : "#30363d"}`,
  background: tone === "warn" ? "#2b2109" : "#161b22",
  color: tone === "warn" ? "#e3b341" : "#8b949e",
  borderRadius: 8,
  padding: 12,
  fontSize: 12,
  lineHeight: 1.5,
  marginBottom: 12,
});

export const EdgesView: React.FC<EdgesViewProps> = ({ transport, unavailable }) => {
  const [filters, setFilters] = useState<Filters>(EMPTY_FILTERS);
  const [tab, setTab] = useState<"edges" | "nodes">("edges");
  /**
   * The plot is a view of the STATUS already fetched, not a second query — the
   * per-language and per-kind tallies come back with it and were only ever
   * rendered as numbers.
   */
  const [plotting, setPlotting] = useState(false);
  const [page, setPage] = useState<Read<CodeGraphPage>>({ status: "idle" });
  const [status, setStatus] = useState<Read<CodeGraphStatusView>>({ status: "idle" });
  /** The filters the loaded page actually answers, so the heading cannot drift. */
  const [answered, setAnswered] = useState<Filters>(EMPTY_FILTERS);

  const canRead = Boolean(transport?.readCodeGraph);

  /** The filter set whose page may still be drawn. An answer to an older set
   * is dropped, so a slow read cannot paint under newer filters. */
  const liveFilters = useRef<Filters>(EMPTY_FILTERS);

  const load = useCallback(
    async (next: Filters) => {
      if (!transport?.readCodeGraph) {
        return;
      }
      liveFilters.current = next;
      setPage({ status: "loading" });
      try {
        const answer = await transport.readCodeGraph(buildQuery(next, PAGE_SIZE));
        if (liveFilters.current !== next) {
          // A newer search is in flight; its own answer sets the state.
          return;
        }
        setPage({ status: "loaded", value: answer });
        // Claimed only after a successful, still-current read: a failed read
        // must not leave the heading asserting these filters were answered.
        setAnswered(next);
      } catch (error) {
        if (liveFilters.current !== next) {
          return;
        }
        setPage({ status: "failed", detail: describe(error) });
      }
    },
    [transport],
  );

  const loadStatus = useCallback(async () => {
    if (!transport?.codeGraphStatus) {
      return;
    }
    setStatus({ status: "loading" });
    try {
      setStatus({ status: "loaded", value: await transport.codeGraphStatus() });
    } catch (error) {
      setStatus({ status: "failed", detail: describe(error) });
    }
  }, [transport]);

  const search = useCallback(() => {
    void loadStatus();
    void load(filters);
  }, [filters, load, loadStatus]);

  // One bounded page on open, exactly as `Action::OpenEdges` requests
  // `request_edge_page(state, state.edge_page)` in the TUI. Bounded is the
  // whole point: this is a `limit`-carrying read, not a graph download.
  useEffect(() => {
    if (transport?.readCodeGraph) {
      void loadStatus();
      void load(EMPTY_FILTERS);
    }
    // Deliberately once per transport: a re-read is the Search button, so an
    // unavailable graph is not retried in a loop.
  }, [transport, load, loadStatus]);

  return (
    <div style={surfaceStyles.page}>
      <div style={surfaceStyles.header}>
        <div>
          <div style={surfaceStyles.title}>Code graph</div>
          <div style={surfaceStyles.subtitle}>
            Nodes and edges from the stored graph, filtered and paged — never fetched whole.
          </div>
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <button
            style={surfaceButton(canRead ? "primary" : "neutral")}
            disabled={!canRead}
            onClick={search}
          >
            Search graph
          </button>
        </div>
      </div>

      <div style={surfaceStyles.scroll}>
        {unavailable && (
          <div role="status" style={banner("warn")}>
            {unavailable}
          </div>
        )}

        {!canRead && !unavailable && (
          <div role="status" style={banner("warn")}>
            The shell does not expose <code>read_code_graph</code>, so the code graph cannot be
            read here. Run the desktop app rather than a browser tab.
          </div>
        )}

        {/* Filters. Every one of these NARROWS; none widens, and the repository
            scope is the daemon's — there is no field here that could name
            another checkout. */}
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "repeat(auto-fit, minmax(160px, 1fr))",
            gap: 10,
            marginBottom: 14,
          }}
        >
          <div>
            <label style={label} htmlFor="graph-name">
              name contains
            </label>
            <input
              id="graph-name"
              style={input}
              value={filters.name}
              placeholder="parse"
              onChange={(event) => setFilters({ ...filters, name: event.target.value })}
              onKeyDown={(event) => {
                if (event.key === "Enter") search();
              }}
            />
          </div>
          <div>
            <label style={label} htmlFor="graph-path">
              path prefix
            </label>
            <input
              id="graph-path"
              style={input}
              value={filters.path}
              placeholder="crates/cli/"
              onChange={(event) => setFilters({ ...filters, path: event.target.value })}
              onKeyDown={(event) => {
                if (event.key === "Enter") search();
              }}
            />
          </div>
          <div>
            <label style={label} htmlFor="graph-language">
              language
            </label>
            <input
              id="graph-language"
              style={input}
              value={filters.language}
              placeholder="rust"
              onChange={(event) => setFilters({ ...filters, language: event.target.value })}
              onKeyDown={(event) => {
                if (event.key === "Enter") search();
              }}
            />
          </div>
          <div>
            <label style={label} htmlFor="graph-kind">
              node kind
            </label>
            <input
              id="graph-kind"
              style={input}
              value={filters.kind}
              placeholder="function"
              onChange={(event) => setFilters({ ...filters, kind: event.target.value })}
              onKeyDown={(event) => {
                if (event.key === "Enter") search();
              }}
            />
          </div>
        </div>

        <GraphStatus status={status} />

        {page.status === "idle" && (
          <div style={{ color: "#6e7681", fontSize: 13 }}>
            Not read yet — the graph is only fetched when you ask, one bounded page at a time.
          </div>
        )}
        {page.status === "loading" && (
          <div role="status" style={{ color: "#8b949e", fontSize: 13 }}>
            Reading one page…
          </div>
        )}
        {page.status === "failed" && (
          <div role="alert" style={banner("warn")}>
            The graph read FAILED, so nothing below reflects the repository: {page.detail}
          </div>
        )}

        {status.status === "loaded" && (
          <div style={{ marginBottom: 12 }}>
            <button
              type="button"
              onClick={() => setPlotting((shown) => !shown)}
              style={surfaceButton(plotting ? "primary" : "neutral")}
            >
              {plotting ? "Hide plot" : "Plot the graph"}
            </button>
            {plotting && (
              <div style={{ marginTop: 12 }}>
                <GraphPlot status={status.value} />
              </div>
            )}
          </div>
        )}

        {page.status === "loaded" && (
          <GraphPage page={page.value} filters={answered} tab={tab} onTab={setTab} />
        )}
      </div>
    </div>
  );
};

/** The stored graph's own description of itself, or nothing at all. */
const GraphStatus: React.FC<{ status: Read<CodeGraphStatusView> }> = ({ status }) => {
  if (status.status === "failed") {
    return (
      <div role="alert" style={banner("warn")}>
        Could not read the graph's status, so its size and staleness are unknown: {status.detail}
      </div>
    );
  }
  if (status.status !== "loaded") {
    // No placeholder counts. An unmeasured graph shows no numbers.
    return null;
  }
  const view = status.value;
  return (
    <div style={banner("info")}>
      <div style={{ ...surfaceStyles.mono, color: "#c9d1d9", marginBottom: 6 }}>
        {view.repository_root}
      </div>
      <Field label="nodes" value={view.nodes.toLocaleString()} />
      <Field label="edges" value={view.edges.toLocaleString()} />
      <Field label="files" value={view.files.toLocaleString()} />
      <Field label="head" value={view.head_revision} />
      {view.working_tree_dirty && <Field label="" value="working tree dirty" />}
      {view.stale && (
        <div style={{ color: "#e3b341", marginTop: 6 }}>
          This graph does NOT describe the current working tree
          {view.stale_reason ? `: ${view.stale_reason}` : "."}
        </div>
      )}
      {view.nodes === 0 && view.edges === 0 && (
        <div style={{ color: "#e3b341", marginTop: 6 }}>
          The stored graph is empty for this checkout — run <code>codypendent graph build</code> to
          fold it; the build reports which files were walked and which produced nothing.
        </div>
      )}
      {view.by_language.length > 0 && (
        <div style={{ marginTop: 6 }}>
          {view.by_language.slice(0, 6).map((entry) => (
            <Field
              key={entry.language}
              label={entry.language}
              value={`${entry.nodes.toLocaleString()} nodes · ${entry.edges.toLocaleString()} edges`}
            />
          ))}
        </div>
      )}
    </div>
  );
};

const GraphPage: React.FC<{
  page: CodeGraphPage;
  filters: Filters;
  tab: "edges" | "nodes";
  onTab: (tab: "edges" | "nodes") => void;
}> = ({ page, filters, tab, onTab }) => {
  const shown = tab === "edges" ? page.edges.length : page.nodes.length;
  const total = tab === "edges" ? page.total_edges : page.total_nodes;
  const cut = isCut(shown, total);
  const filtered = Object.values(filters).some((value) => value.trim().length > 0);

  return (
    <>
      <div style={{ display: "flex", gap: 8, marginBottom: 12 }}>
        <button
          style={surfaceButton(tab === "edges" ? "primary" : "neutral")}
          onClick={() => onTab("edges")}
        >
          Edges {page.edges.length}/{page.total_edges}
        </button>
        <button
          style={surfaceButton(tab === "nodes" ? "primary" : "neutral")}
          onClick={() => onTab("nodes")}
        >
          Nodes {page.nodes.length}/{page.total_nodes}
        </button>
      </div>

      <div style={banner(cut ? "warn" : "info")} role={cut ? "alert" : "status"}>
        Showing {shown.toLocaleString()} of {total.toLocaleString()}{" "}
        {tab === "edges" ? "edges" : "nodes"} matching this filter, at the daemon's applied limit
        of {page.limit}
        {page.limit !== PAGE_SIZE && ` (this client asked for ${PAGE_SIZE})`}.
        {cut && (
          <>
            {" "}
            <strong>This page is cut.</strong> `ReadCodeGraph` carries no cursor and its query has
            no offset, so there is no next page to ask for — narrow the filter (path prefix,
            language, kind, or a longer name substring) to reach the rest.
          </>
        )}
      </div>

      {shown === 0 &&
        (tab === "edges" ? (
          <div style={{ color: "#6e7681", fontSize: 13 }}>
            {filtered
              ? "No edges match this filter."
              : "The graph holds no edges for this checkout."}
          </div>
        ) : (
          <div style={{ color: "#6e7681", fontSize: 13 }}>
            {filtered
              ? "No nodes match this filter."
              : "The graph holds no nodes for this checkout."}
          </div>
        ))}

      {tab === "edges" && page.edges.map((edge) => <EdgeCard key={edgeKey(edge)} edge={edge} />)}
      {tab === "nodes" && page.nodes.map((node) => <NodeCard key={node.id} node={node} />)}
    </>
  );
};

function edgeKey(edge: CodeGraphEdgeView): string {
  return `${edge.from_id}→${edge.to_id}:${edge.relation}:${edge.revision}`;
}

/**
 * One edge, with both endpoints already named by the daemon — a client must not
 * have to issue a second query to render an edge.
 *
 * `asserted_by` is present exactly when the edge was written by
 * `graph.assert_edge`, and it is the audit trail that licenses a model to write
 * to the graph at all: who claimed it, in which run, and on what grounds. It is
 * shown verbatim, wrapped rather than assumed to fit a row.
 */
const EdgeCard: React.FC<{ edge: CodeGraphEdgeView }> = ({ edge }) => (
  <div style={surfaceStyles.card}>
    <div style={{ ...surfaceStyles.mono, color: "#e6edf3" }}>
      {edge.from_name} <span style={{ color: "#8b949e" }}>—{edge.relation}→</span> {edge.to_name}
    </div>
    <div style={{ marginTop: 8 }}>
      <Field label="confidence" value={edge.confidence.toFixed(2)} />
      <Field label="evidence" value={edge.evidence_kind} />
      <Field label="rev" value={edge.revision} />
    </div>
    {edge.asserted_by && (
      <div
        style={{
          marginTop: 8,
          borderLeft: "2px solid #8957e5",
          paddingLeft: 10,
          fontSize: 12,
          color: "#c9d1d9",
        }}
      >
        <div style={{ color: "#8b949e", fontSize: 11, marginBottom: 3 }}>
          agent-asserted · session {edge.asserted_by.session_id} · run {edge.asserted_by.run_id}
        </div>
        <div style={{ whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
          {edge.asserted_by.rationale}
        </div>
      </div>
    )}
  </div>
);

const NodeCard: React.FC<{ node: CodeGraphNodeView }> = ({ node }) => (
  <div style={surfaceStyles.card}>
    <div style={{ ...surfaceStyles.mono, color: "#e6edf3" }}>{node.qualified_name}</div>
    <div style={{ marginTop: 8 }}>
      <Field label="kind" value={node.kind} />
      <Field label="language" value={node.language} />
      {node.package !== undefined && <Field label="package" value={node.package} />}
      <Field label="rev" value={node.revision} />
    </div>
    {node.source_path !== undefined && (
      <div style={{ ...surfaceStyles.mono, ...surfaceStyles.meta }}>{node.source_path}</div>
    )}
  </div>
);
