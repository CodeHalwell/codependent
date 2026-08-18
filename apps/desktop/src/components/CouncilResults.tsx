import React, { useCallback, useEffect, useState } from "react";
import type {
  CouncilProgressFrame,
  CouncilResultCard,
  CouncilResultsPage,
  CouncilRunReply,
} from "../localConfig.js";

/**
 * Convening a council, and reading what previous ones decided.
 *
 * Results are durable files under `<data_dir>/councils/<name>/`, written for
 * EVERY exit — completed, quorum-failed and chair-failed alike — so a partial
 * deliberation is never lost and never presented as a whole one. This view
 * keeps those apart: `status` is shown on every card, and a failure's reason is
 * shown next to the members that did complete.
 *
 * A run takes minutes (each member and the chair is a real daemon run), so the
 * progress channel is the only thing standing between the operator and a frozen
 * screen. Every line it emits is the council crate's own wording.
 *
 * Measured usage is passed through untouched: the crate omits tokens and cost
 * for a run that measured neither, and this view prints its `costLine` verbatim
 * rather than substituting zeros.
 */
export interface CouncilResultsProps {
  onLoad: () => Promise<CouncilResultsPage>;
  /**
   * Convene `name` against `objective`. `onProgress` receives each transition
   * while the promise is still pending.
   */
  onRun?: (
    name: string,
    objective: string,
    onProgress: (frame: CouncilProgressFrame) => void,
  ) => Promise<CouncilRunReply>;
  /** Pre-selected council, e.g. from the browser's Convene button. */
  initialCouncil?: string | null;
  /** Names available to convene, when the caller knows them. */
  councilNames?: string[];
  /**
   * The repository the run will be anchored to, for display. A council's members
   * run against a checkout, so a run with none selected is refused by the shell.
   */
  repository?: string | null;
}

type Status =
  | { kind: "loading" }
  | { kind: "loaded"; page: CouncilResultsPage }
  | { kind: "unavailable"; detail: string };

export const CouncilResults: React.FC<CouncilResultsProps> = ({
  onLoad,
  onRun,
  initialCouncil,
  councilNames,
  repository,
}) => {
  const [status, setStatus] = useState<Status>({ kind: "loading" });
  const [council, setCouncil] = useState(initialCouncil ?? "");
  const [objective, setObjective] = useState("");
  const [progress, setProgress] = useState<CouncilProgressFrame[]>([]);
  const [running, setRunning] = useState(false);
  const [runFailure, setRunFailure] = useState<string | null>(null);
  const [runResult, setRunResult] = useState<CouncilResultCard | null>(null);

  const load = useCallback(async () => {
    setStatus({ kind: "loading" });
    try {
      setStatus({ kind: "loaded", page: await onLoad() });
    } catch (error) {
      setStatus({ kind: "unavailable", detail: describe(error) });
    }
  }, [onLoad]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (initialCouncil) {
      setCouncil(initialCouncil);
    }
  }, [initialCouncil]);

  const run = async () => {
    if (!onRun || council.trim().length === 0 || objective.trim().length === 0) {
      return;
    }
    setRunning(true);
    setProgress([]);
    setRunFailure(null);
    setRunResult(null);
    try {
      const reply = await onRun(council.trim(), objective.trim(), (frame) =>
        setProgress((current) => [...current, frame]),
      );
      // A quorum or chair failure still persisted a report naming every member
      // that DID complete. Show both — the failure and the partial result.
      setRunResult(reply.result ?? null);
      setRunFailure(reply.failure ?? null);
      await load();
    } catch (error) {
      setRunFailure(describe(error));
    } finally {
      setRunning(false);
    }
  };

  const active = progress.length > 0 ? progress[progress.length - 1].activeSubagents : 0;

  return (
    <div style={{ padding: 24, overflowY: "auto", color: "#e6edf3" }}>
      <h2 style={{ margin: "0 0 4px", fontSize: 18, fontWeight: 600 }}>Council results</h2>
      <p style={{ margin: "0 0 20px", fontSize: 13, color: "#8b949e", maxWidth: 760 }}>
        Every council run writes a durable report, including the ones that failed quorum or
        whose chair failed. The report snapshots the definition it ran with, so editing{" "}
        <code>councils.toml</code> later cannot rewrite what a past run convened.
      </p>

      {onRun && (
        <div style={{ ...panel, marginBottom: 20 }}>
          <div style={{ fontSize: 13, fontWeight: 600, marginBottom: 8 }}>Convene</div>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <input
              data-testid="council-run-name"
              value={council}
              onChange={(event) => setCouncil(event.target.value)}
              placeholder="council name"
              list={councilNames ? "council-run-choices" : undefined}
              style={{ ...inputStyle, width: 220 }}
            />
            <input
              data-testid="council-run-objective"
              value={objective}
              onChange={(event) => setObjective(event.target.value)}
              placeholder="objective"
              style={{ ...inputStyle, flex: 1, minWidth: 260 }}
            />
            <button
              onClick={() => void run()}
              disabled={running || council.trim().length === 0 || objective.trim().length === 0}
              style={primaryButtonStyle}
              data-testid="council-run-submit"
            >
              {running ? "Deliberating…" : "Convene"}
            </button>
          </div>
          {councilNames && (
            <datalist id="council-run-choices">
              {councilNames.map((name) => (
                <option key={name} value={name} />
              ))}
            </datalist>
          )}
          <div style={{ marginTop: 8, fontSize: 12, color: "#8b949e" }}>
            {repository ? (
              <>
                Members run against <code>{repository}</code>.
              </>
            ) : (
              /* No silent default: the shell refuses a run with no repository. */
              <>No repository is selected, so a run will be refused — choose one first.</>
            )}
          </div>
        </div>
      )}

      {(running || progress.length > 0) && (
        <div style={{ ...panel, marginBottom: 20 }} data-testid="council-progress">
          <div style={{ fontSize: 13, fontWeight: 600, marginBottom: 8 }}>
            Progress {running && <span style={{ color: "#8b949e" }}>· {active} active</span>}
          </div>
          <ol style={{ margin: 0, paddingLeft: 18, fontSize: 12, color: "#8b949e" }}>
            {progress.map((frame, index) => (
              <li key={`${frame.occurredAt}-${index}`} style={{ marginBottom: 2 }}>
                <span style={{ color: frame.phase === "member-failed" || frame.phase === "warning" ? "#d29922" : "#8b949e" }}>
                  {frame.message}
                </span>
              </li>
            ))}
          </ol>
        </div>
      )}

      {runFailure && (
        <div role="alert" data-testid="council-run-failure" style={refusalStyle}>
          {runFailure}
        </div>
      )}

      {runResult && (
        <div style={{ marginBottom: 20 }} data-testid="council-run-result">
          <ResultCard result={runResult} expanded />
        </div>
      )}

      <div style={{ display: "flex", alignItems: "baseline", justifyContent: "space-between" }}>
        <h3 style={{ margin: "0 0 12px", fontSize: 15, fontWeight: 600 }}>Saved results</h3>
        <button onClick={() => void load()} style={buttonStyle} data-testid="council-results-refresh">
          Refresh
        </button>
      </div>

      {status.kind === "loading" && (
        <div role="status" data-testid="council-results-loading" style={{ ...panel, color: "#8b949e" }}>
          Reading the council report store…
        </div>
      )}

      {status.kind === "unavailable" && (
        /* Could not read the store. Not the same as "no council has run". */
        <div role="status" data-testid="council-results-unavailable" style={{ ...panel, color: "#d29922" }}>
          Council results unavailable — {status.detail}
        </div>
      )}

      {status.kind === "loaded" && (
        <>
          {status.page.warnings.map((warning, index) => (
            /* One unreadable report degrades its own row, not the page. */
            <div
              key={index}
              role="status"
              data-testid="council-results-warning"
              style={{ ...panel, marginBottom: 8, color: "#d29922", fontSize: 13 }}
            >
              {warning}
            </div>
          ))}
          {status.page.results.length === 0 && status.page.warnings.length === 0 && (
            <div data-testid="council-results-empty" style={{ ...panel, color: "#8b949e" }}>
              No council has run yet.
            </div>
          )}
          <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            {status.page.results.map((result) => (
              <ResultCard key={result.resultId} result={result} />
            ))}
          </div>
        </>
      )}
    </div>
  );
};

const ResultCard: React.FC<{ result: CouncilResultCard; expanded?: boolean }> = ({
  result,
  expanded = false,
}) => {
  const [open, setOpen] = useState(expanded);
  const completed = result.status === "completed";

  return (
    <div style={panel} data-testid={`council-result-${result.resultId}`}>
      <div style={{ display: "flex", justifyContent: "space-between", gap: 12 }}>
        <div style={{ minWidth: 0 }}>
          <div style={{ fontSize: 15, fontWeight: 600 }}>{result.council}</div>
          <div style={{ marginTop: 2, fontSize: 13, color: "#8b949e" }}>{result.objective}</div>
        </div>
        <span
          data-testid={`council-result-status-${result.resultId}`}
          style={{
            alignSelf: "flex-start",
            padding: "3px 8px",
            borderRadius: 999,
            fontSize: 12,
            background: completed ? "#122619" : "#3c181a",
            color: completed ? "#3fb950" : "#ff7b72",
            flexShrink: 0,
          }}
        >
          {result.status}
        </span>
      </div>

      <div style={{ marginTop: 8, fontSize: 12, color: "#8b949e" }}>
        {result.finishedAt} · {result.repository}
        {result.evidence && " · evidence mode"}
      </div>

      {/* The crate's own measured-cost line. It omits what was not measured;
          nothing here fills a gap with a zero. */}
      <div style={{ marginTop: 6, fontSize: 12, color: "#8b949e" }} data-testid={`council-cost-${result.resultId}`}>
        {result.costLine}
      </div>

      {result.failure && (
        <div role="alert" style={{ marginTop: 10, fontSize: 13, color: "#ffa198" }} data-testid={`council-failure-${result.resultId}`}>
          {result.failure}
        </div>
      )}

      {result.warnings.map((warning, index) => (
        <div key={index} role="status" style={{ marginTop: 6, fontSize: 12, color: "#d29922" }}>
          {warning}
        </div>
      ))}

      {result.synthesis ? (
        <div style={{ marginTop: 12 }}>
          <div style={{ fontSize: 12, color: "#8b949e", marginBottom: 4 }}>Chair synthesis</div>
          <div style={{ whiteSpace: "pre-wrap", fontSize: 13 }}>{result.synthesis}</div>
        </div>
      ) : (
        /* No chair answer. `status` above says whether the chair never ran or
           failed — this is never drawn as a chair that said nothing. */
        <div style={{ marginTop: 12, fontSize: 13, color: "#8b949e" }} data-testid={`council-no-synthesis-${result.resultId}`}>
          No chair synthesis in this report.
        </div>
      )}

      <button onClick={() => setOpen((current) => !current)} style={{ ...buttonStyle, marginTop: 12 }} data-testid={`council-toggle-${result.resultId}`}>
        {open ? "Hide rounds" : `Show ${result.rounds.length} round(s)`}
      </button>

      {open && (
        <div style={{ marginTop: 12 }}>
          {result.participants.length > 0 && (
            <div style={{ fontSize: 12, color: "#8b949e", marginBottom: 8 }}>
              {result.participants.map((line, index) => (
                <div key={index}>{line}</div>
              ))}
            </div>
          )}
          {result.rounds.map((round) => (
            <div key={round.round} style={{ borderTop: "1px solid #30363d", paddingTop: 10, marginTop: 10 }}>
              <div style={{ fontSize: 13, fontWeight: 600 }}>Round {round.round}</div>
              {round.members.map((member) => (
                <div key={member.runId} style={{ marginTop: 8 }}>
                  <div style={{ fontSize: 12, color: "#8b949e" }}>
                    {member.role} · <code>{member.model}</code>
                    {/* Absent measurements stay absent. */}
                    {member.tokens !== null && member.tokens !== undefined && ` · ${member.tokens} tokens`}
                  </div>
                  <div style={{ whiteSpace: "pre-wrap", fontSize: 13, marginTop: 2 }}>{member.response}</div>
                </div>
              ))}
              {round.failures.map((failure, index) => (
                <div key={index} role="status" style={{ marginTop: 8, fontSize: 12, color: "#d29922" }}>
                  {failure}
                </div>
              ))}
            </div>
          ))}
          {result.rounds.length === 0 && (
            <div style={{ fontSize: 12, color: "#8b949e" }}>
              This projection carries no per-round detail; the full report is at{" "}
              <code>{result.reportMarkdown}</code>.
            </div>
          )}
        </div>
      )}
    </div>
  );
};

const panel: React.CSSProperties = {
  padding: 16,
  background: "#161b22",
  border: "1px solid #30363d",
  borderRadius: 8,
};

const inputStyle: React.CSSProperties = {
  padding: "6px 10px",
  borderRadius: 6,
  border: "1px solid #30363d",
  background: "#0d1117",
  color: "#e6edf3",
  fontSize: 13,
};

const buttonStyle: React.CSSProperties = {
  padding: "6px 12px",
  borderRadius: 6,
  border: "1px solid #30363d",
  background: "#21262d",
  color: "#e6edf3",
  fontSize: 13,
  cursor: "pointer",
};

const primaryButtonStyle: React.CSSProperties = {
  ...buttonStyle,
  background: "#238636",
  borderColor: "#2ea043",
};

const refusalStyle: React.CSSProperties = {
  marginBottom: 16,
  padding: 12,
  borderRadius: 8,
  border: "1px solid #da3633",
  background: "#2d1214",
  color: "#ffa198",
  fontSize: 13,
  whiteSpace: "pre-wrap",
};

function describe(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (error instanceof Error) {
    return error.message;
  }
  return String(error);
}
