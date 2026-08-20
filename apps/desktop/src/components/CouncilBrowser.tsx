import React, { useCallback, useState } from "react";
import { useLoadOnMount } from "../useLoadOnMount.js";
import type { CouncilCard } from "../localConfig.js";

/**
 * The configured councils, from `<config_dir>/councils.toml`.
 *
 * A council is LOCAL CONFIGURATION — there is no council command on the daemon
 * wire — so this list is the council crate's `list_definitions` and nothing
 * else. An empty list means the file has no councils in it; a failure to read
 * it renders as unavailable, because "you have configured none" and "we could
 * not find out" are different facts.
 *
 * Deleting is always confirmed: it edits a shared configuration file, and the
 * confirmation names what will and will not be removed (the definition goes,
 * the saved run reports stay).
 */
export interface CouncilBrowserProps {
  onLoad: () => Promise<CouncilCard[]>;
  onDelete?: (name: string) => Promise<void>;
  /** Convene this council — hands off to the results/run view. */
  onRun?: (name: string) => void;
  /** Open the builder. */
  onCreate?: () => void;
}

type Status =
  | { kind: "loading" }
  | { kind: "loaded"; councils: CouncilCard[] }
  | { kind: "unavailable"; detail: string };

export const CouncilBrowser: React.FC<CouncilBrowserProps> = ({
  onLoad,
  onDelete,
  onRun,
  onCreate,
}) => {
  const [status, setStatus] = useState<Status>({ kind: "loading" });
  const [confirming, setConfirming] = useState<CouncilCard | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    setStatus({ kind: "loading" });
    try {
      setStatus({ kind: "loaded", councils: await onLoad() });
    } catch (failure) {
      setStatus({ kind: "unavailable", detail: describe(failure) });
    }
  }, [onLoad]);

  useLoadOnMount(load);

  const confirmDelete = async () => {
    if (!onDelete || !confirming) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await onDelete(confirming.name);
      setConfirming(null);
      await load();
    } catch (failure) {
      setError(describe(failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div style={{ padding: 24, overflowY: "auto", color: "#e6edf3" }}>
      <div style={{ display: "flex", alignItems: "baseline", justifyContent: "space-between" }}>
        <div>
          <h2 style={{ margin: "0 0 4px", fontSize: 18, fontWeight: 600 }}>Councils</h2>
          <p style={{ margin: "0 0 20px", fontSize: 13, color: "#8b949e", maxWidth: 720 }}>
            A council convenes several models on one objective and has a chair synthesize
            their reports. Definitions are local configuration, held in{" "}
            <code>councils.toml</code> — the daemon has no council command.
          </p>
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          {onCreate && (
            <button onClick={onCreate} style={primaryButtonStyle} data-testid="council-new">
              New council
            </button>
          )}
          <button onClick={() => void load()} style={buttonStyle} data-testid="council-refresh">
            Refresh
          </button>
        </div>
      </div>

      {status.kind === "loading" && (
        <div role="status" data-testid="council-loading" style={{ ...panel, color: "#8b949e" }}>
          Reading councils.toml…
        </div>
      )}

      {status.kind === "unavailable" && (
        /* Not "no councils" — we could not read the file at all. */
        <div role="status" data-testid="council-unavailable" style={{ ...panel, color: "#d29922" }}>
          Councils unavailable — {status.detail}
        </div>
      )}

      {status.kind === "loaded" && status.councils.length === 0 && (
        <div data-testid="council-empty" style={{ ...panel, color: "#8b949e" }}>
          No councils are configured. A council needs at least two members and a chair, and
          every one of them must already be a model in <code>models.toml</code>.
        </div>
      )}

      {status.kind === "loaded" && status.councils.length > 0 && (
        <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
          {status.councils.map((council) => (
            <div key={council.name} data-testid={`council-${council.name}`} style={panel}>
              <div style={{ display: "flex", justifyContent: "space-between", gap: 12 }}>
                <div style={{ minWidth: 0 }}>
                  <div style={{ fontSize: 15, fontWeight: 600 }}>{council.name}</div>
                  {council.description && (
                    <div style={{ marginTop: 4, fontSize: 13, color: "#8b949e" }}>
                      {council.description}
                    </div>
                  )}
                </div>
                <div style={{ display: "flex", gap: 8, flexShrink: 0 }}>
                  {onRun && (
                    <button
                      onClick={() => onRun(council.name)}
                      style={primaryButtonStyle}
                      data-testid={`council-run-${council.name}`}
                    >
                      Convene
                    </button>
                  )}
                  {onDelete && (
                    <button
                      onClick={() => setConfirming(council)}
                      style={buttonStyle}
                      data-testid={`council-delete-${council.name}`}
                    >
                      Delete
                    </button>
                  )}
                </div>
              </div>

              <dl style={{ display: "flex", gap: 24, margin: "12px 0 0", fontSize: 12, flexWrap: "wrap" }}>
                <Field label="Chair" value={council.chair} />
                <Field label="Rounds" value={String(council.rounds)} />
                {/* The definition's OWN quorum when it names one, and what will
                    actually be enforced when it does not. Never one number
                    presented as if the operator had chosen it. */}
                <Field
                  label="Quorum"
                  value={
                    council.quorum === null || council.quorum === undefined
                      ? `${council.requiredQuorum} (majority, not set)`
                      : String(council.quorum)
                  }
                />
                <Field label="Evidence mode" value={council.evidence ? "on" : "off"} />
              </dl>

              <div style={{ marginTop: 12, display: "flex", gap: 6, flexWrap: "wrap" }}>
                {council.members.map((member) => (
                  <span
                    key={`${member.model}:${member.role}`}
                    style={{
                      padding: "3px 8px",
                      borderRadius: 999,
                      background: "#21262d",
                      border: "1px solid #30363d",
                      fontSize: 12,
                    }}
                  >
                    {member.role} · <code style={{ color: "#8b949e" }}>{member.model}</code>
                  </span>
                ))}
              </div>

              {council.chairIsMember && (
                /* Legal, but the chair then weighs its own report. The council
                   crate warns about it at creation; say so here too. */
                <div
                  data-testid={`council-chair-warning-${council.name}`}
                  role="status"
                  style={{ marginTop: 10, fontSize: 12, color: "#d29922" }}
                >
                  The chair is also a member, so its synthesis will weigh its own report.
                </div>
              )}
            </div>
          ))}
        </div>
      )}

      {confirming && (
        <div role="alertdialog" aria-label="Confirm council deletion" data-testid="council-delete-confirm" style={confirmStyle}>
          <div style={{ fontWeight: 600 }}>Delete council “{confirming.name}”?</div>
          <div style={{ marginTop: 6, fontSize: 13, color: "#8b949e" }}>
            The definition is removed from <code>councils.toml</code>. Saved run reports are
            kept — a deliberation that happened still happened.
          </div>
          {error && (
            <div role="alert" style={{ marginTop: 8, fontSize: 13, color: "#ffa198" }}>
              {error}
            </div>
          )}
          <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
            <button onClick={() => void confirmDelete()} disabled={busy} style={dangerButtonStyle} data-testid="council-delete-yes">
              Delete
            </button>
            <button onClick={() => setConfirming(null)} disabled={busy} style={buttonStyle} data-testid="council-delete-no">
              Cancel
            </button>
          </div>
        </div>
      )}
    </div>
  );
};

const Field: React.FC<{ label: string; value: string }> = ({ label, value }) => (
  <div>
    <dt style={{ color: "#8b949e" }}>{label}</dt>
    <dd style={{ margin: 0 }}>{value}</dd>
  </div>
);

const panel: React.CSSProperties = {
  padding: 16,
  background: "#161b22",
  border: "1px solid #30363d",
  borderRadius: 8,
};

const confirmStyle: React.CSSProperties = {
  ...panel,
  marginTop: 16,
  borderColor: "#da3633",
  background: "#2d1214",
  color: "#e6edf3",
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

const dangerButtonStyle: React.CSSProperties = {
  ...buttonStyle,
  background: "#da3633",
  borderColor: "#f85149",
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
