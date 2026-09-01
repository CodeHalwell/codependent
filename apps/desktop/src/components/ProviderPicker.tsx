/**
 * The provider catalog.
 *
 * Rows are the built-in curated catalog layered with the user's
 * `providers.toml`, read in the shell by `codypendent-providers` — the same
 * catalog the CLI and TUI read. Every derived flag on a row (`available`,
 * `requires_key`, `can_list_models`, `has_key`) is the TUI's own gate; this
 * file draws them and derives nothing of its own.
 *
 * Two honesty rules shape the view:
 *
 * 1. A provider whose runtime adapter this build cannot execute is shown and
 *    DISABLED with its reason, not hidden. Hiding it would make the catalog
 *    look smaller than it is; enabling it would produce a model entry that can
 *    only fail at run time.
 * 2. Selecting the community ACP bridge is a trust decision, so it passes
 *    through a host-owned confirmation carrying the actual risk. Declining is
 *    lossless — the query and the row you were on survive it. The row cannot
 *    draw or waive its own gate.
 */
import React, { useCallback, useMemo, useState } from "react";
import { useLoadOnMount } from "../useLoadOnMount.js";

import {
  describeError,
  localConfigClient,
  shellAvailable,
  NO_SHELL,
  type LocalConfigClient,
  type ProviderRow,
  type ProvidersView,
} from "./localConfig";

export interface ProviderPickerProps {
  client?: LocalConfigClient;
  /**
   * Called with a provider the operator picked and this build can execute.
   * Never called for an unavailable row, and never called for the community
   * bridge — that path terminates in the confirmation.
   */
  onSelect?: (provider: ProviderRow) => void;
}

const PANEL: React.CSSProperties = {
  background: "#0d1117",
  color: "#c9d1d9",
  fontSize: 13,
  display: "flex",
  flexDirection: "column",
  height: "100%",
};

const BADGE: React.CSSProperties = {
  fontSize: 11,
  padding: "1px 6px",
  borderRadius: 10,
  background: "#21262d",
  color: "#8b949e",
};

export const ProviderPicker: React.FC<ProviderPickerProps> = ({ client, onSelect }) => {
  const api = client ?? localConfigClient;
  const [view, setView] = useState<ProvidersView | null>(null);
  const [unavailable, setUnavailable] = useState<string | null>(
    shellAvailable() || client ? null : NO_SHELL,
  );
  const [query, setQuery] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  /** The row awaiting a community-install trust decision, if any. */
  const [consent, setConsent] = useState<ProviderRow | null>(null);

  const load = useCallback(async () => {
    if (!shellAvailable() && !client) {
      setUnavailable(NO_SHELL);
      return;
    }
    try {
      setView(await api.listProviders());
      setUnavailable(null);
    } catch (error) {
      setView(null);
      setUnavailable(describeError(error));
    }
  }, [api, client]);

  useLoadOnMount(load);

  const matches = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const rows = view?.providers ?? [];
    if (needle.length === 0) {
      return rows;
    }
    return rows.filter(
      (row) =>
        row.id.toLowerCase().includes(needle) ||
        row.name.toLowerCase().includes(needle) ||
        row.protocol.toLowerCase().includes(needle),
    );
  }, [view, query]);

  const pick = (row: ProviderRow) => {
    if (row.community_consent_required) {
      // The confirmation is host chrome. Nothing is installed, downloaded or
      // configured by opening it.
      setConsent(row);
      return;
    }
    if (!row.available) {
      setNotice(
        row.unusable_reason ??
          `${row.id} is catalog-only — its ${row.protocol} runtime adapter is not installed`,
      );
      return;
    }
    setNotice(null);
    onSelect?.(row);
  };

  return (
    <div style={PANEL} data-testid="provider-picker">
      <header style={{ padding: "16px 24px", borderBottom: "1px solid #21262d" }}>
        <h2 style={{ margin: 0, fontSize: 16, fontWeight: 600 }}>Providers</h2>
        <p style={{ margin: "4px 0 0", color: "#8b949e", fontSize: 12 }}>
          The curated catalog, layered with your <code>providers.toml</code>.
        </p>
      </header>

      {unavailable ? (
        <div
          role="status"
          data-testid="provider-picker-unavailable"
          style={{ padding: "32px 24px", color: "#d29922" }}
        >
          Provider catalog unavailable — {unavailable}
        </div>
      ) : view === null ? (
        <div role="status" style={{ padding: "32px 24px", color: "#8b949e" }}>
          Reading the provider catalog&hellip;
        </div>
      ) : (
        <>
          <div style={{ padding: "12px 24px", borderBottom: "1px solid #21262d" }}>
            <input
              type="search"
              value={query}
              placeholder="Filter providers"
              onChange={(event) => setQuery(event.target.value)}
              style={{
                width: "100%",
                padding: "6px 10px",
                borderRadius: 6,
                border: "1px solid #30363d",
                background: "#0d1117",
                color: "#c9d1d9",
                font: "inherit",
              }}
            />
          </div>

          {/* ACP agents come from a crate this build does not link. Said, not
              silently omitted — the list is otherwise indistinguishable from a
              catalog that has none. */}
          <div
            role="note"
            data-testid="provider-acp-unavailable"
            style={{
              margin: "8px 24px 0",
              padding: "8px 12px",
              color: "#c9d1d9",
              fontSize: 12,
              lineHeight: 1.5,
              border: "1px solid #30363d",
              background: "#161b22",
              borderRadius: 6,
            }}
          >
            <strong>Looking for Claude Code, Codex, Gemini CLI or another coding agent?</strong>{" "}
            {view.acp_unavailable} In a terminal:{" "}
            <code style={{ background: "#0d1117", padding: "0 4px", borderRadius: 4 }}>
              codypendent acp list
            </code>{" "}
            then{" "}
            <code style={{ background: "#0d1117", padding: "0 4px", borderRadius: 4 }}>
              codypendent acp connect claude-code
            </code>
            . The connected agent then appears under Models here.
          </div>

          {view.warnings.map((warning) => (
            <div key={warning} role="status" style={{ padding: "4px 24px", color: "#d29922", fontSize: 12 }}>
              {warning}
            </div>
          ))}

          <div style={{ flex: 1, overflowY: "auto", padding: "8px 16px" }}>
            {matches.length === 0 ? (
              <div data-testid="provider-picker-empty" style={{ padding: 24, color: "#8b949e" }}>
                {view.providers.length === 0
                  ? `No providers in ${view.providers_path} or the built-in catalog.`
                  : `No provider matches “${query}”.`}
              </div>
            ) : (
              <ul style={{ listStyle: "none", margin: 0, padding: 0, display: "flex", flexDirection: "column", gap: 6 }}>
                {matches.map((row) => (
                  <li key={row.id}>
                    <button
                      type="button"
                      onClick={() => pick(row)}
                      // Disabled rows stay focusable and clickable so their
                      // reason can be read; the click never selects them —
                      // and a screen reader hears that, instead of an
                      // ordinary actionable button.
                      aria-disabled={!row.available}
                      style={{
                        width: "100%",
                        textAlign: "left",
                        padding: "10px 12px",
                        borderRadius: 6,
                        cursor: "pointer",
                        background: "#161b22",
                        border: `1px solid ${row.community_consent_required ? "#d29922" : "#21262d"}`,
                        color: row.available ? "#c9d1d9" : "#8b949e",
                        font: "inherit",
                      }}
                    >
                      <div style={{ display: "flex", alignItems: "baseline", gap: 8, flexWrap: "wrap" }}>
                        <span style={{ fontWeight: 600 }}>{row.name}</span>
                        <span style={{ ...BADGE }}>{row.protocol}</span>
                        {row.local && <span style={{ ...BADGE, color: "#3fb950" }}>local</span>}
                        {row.has_key && <span style={{ ...BADGE, color: "#3fb950" }}>key set</span>}
                        {row.requires_key && !row.has_key && (
                          <span style={{ ...BADGE, color: "#d29922" }}>needs key</span>
                        )}
                        {row.catalog_models > 0 && (
                          <span style={{ ...BADGE }}>{row.catalog_models} catalog models</span>
                        )}
                      </div>
                      <div style={{ color: "#8b949e", fontSize: 12, marginTop: 2 }}>
                        {row.id} · {row.auth}
                      </div>
                      {row.unusable_reason ? (
                        <div style={{ color: "#d29922", fontSize: 12, marginTop: 4 }}>
                          {row.unusable_reason}
                        </div>
                      ) : !row.available ? (
                        // The same explanation `pick()` puts in the footer
                        // notice, ON the row — in a long list the footer can
                        // be below the fold of the very click it explains.
                        <div style={{ color: "#d29922", fontSize: 12, marginTop: 4 }}>
                          catalog-only — its {row.protocol} runtime adapter is not installed
                        </div>
                      ) : null}
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </>
      )}

      {notice && (
        <div role="status" style={{ padding: "8px 24px", color: "#d29922", fontSize: 12 }}>
          {notice}
        </div>
      )}

      {consent && (
        /* Host-owned trust confirmation. The evidence the operator is consenting
           to travels ON the prompt, and declining restores the list exactly as
           it was — the filter and the row are untouched. */
        <div
          role="alertdialog"
          aria-label="Community ACP bridge"
          data-testid="community-acp-confirm"
          style={{
            borderTop: "1px solid #d29922",
            background: "#1c1710",
            padding: "16px 24px",
          }}
        >
          <h3 style={{ margin: 0, fontSize: 14, color: "#d29922" }}>
            {consent.name} — third-party trust decision
          </h3>
          <p style={{ margin: "8px 0 0", color: "#c9d1d9", fontSize: 12 }}>
            {consent.community_consent_detail}
          </p>
          {consent.unusable_reason && (
            <p style={{ margin: "8px 0 0", color: "#8b949e", fontSize: 12 }}>
              {consent.unusable_reason}
            </p>
          )}
          <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
            <button
              type="button"
              onClick={() => {
                // Confirming cannot install here, and says so rather than
                // appearing to succeed.
                setNotice(
                  consent.unusable_reason ??
                    `${consent.id} cannot be installed from the desktop shell`,
                );
                setConsent(null);
              }}
              style={{
                padding: "6px 12px",
                borderRadius: 6,
                border: "1px solid #d29922",
                background: "transparent",
                color: "#d29922",
                cursor: "pointer",
                font: "inherit",
              }}
            >
              I understand the risk
            </button>
            <button
              type="button"
              onClick={() => setConsent(null)}
              style={{
                padding: "6px 12px",
                borderRadius: 6,
                border: "1px solid #30363d",
                background: "transparent",
                color: "#c9d1d9",
                cursor: "pointer",
                font: "inherit",
              }}
            >
              Cancel
            </button>
          </div>
        </div>
      )}
    </div>
  );
};

export default ProviderPicker;
