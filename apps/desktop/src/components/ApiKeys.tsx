/**
 * API keys — presence, never value.
 *
 * This is the one surface in the desktop client where a secret exists, so it is
 * the one with the strictest rule: NO KEY MATERIAL EVER TRAVELS BACK. The shell
 * answers with `KeyStatus` (stored / the NAME of an environment variable /
 * missing / unknown) and nothing else, mirroring
 * `crates/tui/src/state.rs::KeyStatus`. There is no reveal control here, and
 * there is nothing to reveal.
 *
 * The entry field is the web analogue of the TUI's `render_masked_prompt`
 * (`crates/tui/src/render.rs`), which draws one bullet per character and never
 * lets the buffer reach a span: a password input, no autofill, no spellcheck,
 * no show/hide toggle, the value never rendered as text and never carried on to
 * any other component. It is cleared as soon as the write returns.
 *
 * Two refusals are carried from the TUI:
 *
 * * a blank key is refused and nothing is written — storing an empty string
 *   would silently shadow a valid `api_key_env` and look like it worked;
 * * remove is offered only for a row that is actually `stored`, so a row backed
 *   by an environment variable (or one whose status is unknown) cannot be
 *   "removed" into a no-op that reads as success.
 */
import React, { useCallback, useEffect, useState } from "react";

import {
  describeError,
  describeKeyStatus,
  localConfigClient,
  shellAvailable,
  NO_SHELL,
  type KeyRow,
  type KeysView,
  type LocalConfigClient,
} from "./localConfig";

export interface ApiKeysProps {
  client?: LocalConfigClient;
}

const PANEL: React.CSSProperties = {
  background: "#0d1117",
  color: "#c9d1d9",
  fontSize: 13,
  display: "flex",
  flexDirection: "column",
  height: "100%",
};

const targetKey = (row: KeyRow) => `${row.target.kind}:${row.target.id}`;

export const ApiKeys: React.FC<ApiKeysProps> = ({ client }) => {
  const api = client ?? localConfigClient;
  const [view, setView] = useState<KeysView | null>(null);
  const [unavailable, setUnavailable] = useState<string | null>(
    shellAvailable() || client ? null : NO_SHELL,
  );
  const [notice, setNotice] = useState<string | null>(null);
  /** The row whose key is being entered, if any. */
  const [entering, setEntering] = useState<KeyRow | null>(null);
  /**
   * The key being typed. Held only while the prompt is open, cleared on submit
   * and on cancel, and never passed to a child, a log or a status message.
   */
  const [draft, setDraft] = useState("");
  /** The row awaiting a delete confirmation. */
  const [removing, setRemoving] = useState<KeyRow | null>(null);

  const load = useCallback(async () => {
    if (!shellAvailable() && !client) {
      setUnavailable(NO_SHELL);
      return;
    }
    try {
      setView(await api.listApiKeys());
      setUnavailable(null);
    } catch (error) {
      // Not "no keys configured" — we never learned which keys are set.
      setView(null);
      setUnavailable(describeError(error));
    }
  }, [api, client]);

  useEffect(() => {
    void load();
  }, [load]);

  const submitKey = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!entering) {
      return;
    }
    if (draft.trim().length === 0) {
      // The same refusal the shell makes, made here too so nothing is sent.
      setNotice("key must not be blank — nothing was written");
      return;
    }
    const target = entering.target;
    try {
      await api.setApiKey(target, draft);
      setNotice(`key saved for ${entering.label} — applies to the next run`);
    } catch (error) {
      setNotice(`could not save key: ${describeError(error)}`);
    } finally {
      // Cleared on every path, including failure.
      setDraft("");
      setEntering(null);
    }
    await load();
  };

  const confirmRemove = async () => {
    if (!removing) {
      return;
    }
    try {
      await api.removeApiKey(removing.target);
      setNotice(`key removed for ${removing.label}`);
    } catch (error) {
      setNotice(`could not remove key: ${describeError(error)}`);
    } finally {
      setRemoving(null);
    }
    await load();
  };

  return (
    <div style={PANEL} data-testid="api-keys">
      <header style={{ padding: "16px 24px", borderBottom: "1px solid #21262d" }}>
        <h2 style={{ margin: 0, fontSize: 16, fontWeight: 600 }}>API keys</h2>
        <p style={{ margin: "4px 0 0", color: "#8b949e", fontSize: 12 }}>
          Which credentials are set. Stored keys live in{" "}
          <code>{view?.auth_path ?? "auth.json"}</code> at mode 0600 and are never displayed.
        </p>
      </header>

      {unavailable ? (
        <div
          role="status"
          data-testid="api-keys-unavailable"
          style={{ padding: "32px 24px", color: "#d29922" }}
        >
          Key status unavailable — {unavailable}
        </div>
      ) : view === null ? (
        <div role="status" style={{ padding: "32px 24px", color: "#8b949e" }}>
          Reading key status&hellip;
        </div>
      ) : (
        <>
          {view.warnings.map((warning) => (
            <div key={warning} role="status" style={{ padding: "4px 24px", color: "#d29922", fontSize: 12 }}>
              {warning}
            </div>
          ))}

          <div style={{ flex: 1, overflowY: "auto", padding: "8px 16px" }}>
            {view.keys.length === 0 ? (
              <div data-testid="api-keys-empty" style={{ padding: 24, color: "#8b949e" }}>
                No models are configured, so there are no credentials to manage yet.
              </div>
            ) : (
              <ul style={{ listStyle: "none", margin: 0, padding: 0, display: "flex", flexDirection: "column", gap: 6 }}>
                {view.keys.map((row) => {
                  const status = describeKeyStatus(row.status);
                  const stored = row.status.state === "stored";
                  return (
                    <li
                      key={targetKey(row)}
                      style={{
                        padding: "10px 12px",
                        borderRadius: 6,
                        background: "#161b22",
                        border: "1px solid #21262d",
                        display: "flex",
                        alignItems: "center",
                        gap: 12,
                      }}
                    >
                      <span aria-hidden style={{ color: status.color, fontSize: 16 }}>
                        {status.glyph}
                      </span>
                      <div style={{ flex: 1, minWidth: 0 }}>
                        <div style={{ fontWeight: 600 }}>{row.label}</div>
                        <div style={{ color: "#8b949e", fontSize: 12 }}>{row.detail}</div>
                        <div style={{ color: status.color, fontSize: 12 }}>{status.label}</div>
                      </div>
                      <button
                        type="button"
                        onClick={() => {
                          setDraft("");
                          setEntering(row);
                        }}
                        style={{
                          padding: "4px 10px",
                          borderRadius: 6,
                          border: "1px solid #30363d",
                          background: "transparent",
                          color: "#c9d1d9",
                          cursor: "pointer",
                          font: "inherit",
                          fontSize: 12,
                        }}
                      >
                        {stored ? "Replace" : "Set"}
                      </button>
                      {/* Fail closed: only a row that really holds a stored key
                          offers removal. */}
                      {stored && (
                        <button
                          type="button"
                          onClick={() => setRemoving(row)}
                          style={{
                            padding: "4px 10px",
                            borderRadius: 6,
                            border: "1px solid #30363d",
                            background: "transparent",
                            color: "#ff7b72",
                            cursor: "pointer",
                            font: "inherit",
                            fontSize: 12,
                          }}
                        >
                          Remove
                        </button>
                      )}
                    </li>
                  );
                })}
              </ul>
            )}
          </div>

          {/* Credential rows this build does not project, and why. */}
          <div role="note" style={{ padding: "8px 24px", color: "#8b949e", fontSize: 12 }}>
            {view.unavailable}
          </div>
        </>
      )}

      {entering && (
        <form
          onSubmit={submitKey}
          data-testid="api-key-prompt"
          style={{ borderTop: "1px solid #21262d", padding: "16px 24px" }}
        >
          <label htmlFor="api-key-input" style={{ display: "block", fontSize: 12, color: "#8b949e" }}>
            API key for {entering.label}
          </label>
          <input
            id="api-key-input"
            // Masked, never revealed: no type toggle exists on this input, and
            // the value is not rendered anywhere as text.
            type="password"
            value={draft}
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
            spellCheck={false}
            onChange={(event) => setDraft(event.target.value)}
            style={{
              width: "100%",
              marginTop: 6,
              padding: "6px 10px",
              borderRadius: 6,
              border: "1px solid #30363d",
              background: "#0d1117",
              color: "#c9d1d9",
              font: "inherit",
            }}
          />
          <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
            <button
              type="submit"
              style={{
                padding: "6px 12px",
                borderRadius: 6,
                border: "1px solid #238636",
                background: "#238636",
                color: "#fff",
                cursor: "pointer",
                font: "inherit",
              }}
            >
              Save
            </button>
            <button
              type="button"
              onClick={() => {
                setDraft("");
                setEntering(null);
              }}
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
        </form>
      )}

      {removing && (
        <div
          role="alertdialog"
          aria-label="Remove stored key"
          data-testid="api-key-remove-confirm"
          style={{ borderTop: "1px solid #21262d", padding: "16px 24px" }}
        >
          <p style={{ margin: 0, fontSize: 13 }}>
            Remove the stored key for <strong>{removing.label}</strong>? Runs will fall back to an
            environment variable if one is configured, and fail to authenticate if none is.
          </p>
          <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
            <button
              type="button"
              onClick={() => void confirmRemove()}
              style={{
                padding: "6px 12px",
                borderRadius: 6,
                border: "1px solid #da3633",
                background: "#da3633",
                color: "#fff",
                cursor: "pointer",
                font: "inherit",
              }}
            >
              Remove
            </button>
            <button
              type="button"
              onClick={() => setRemoving(null)}
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

      {notice && (
        <div role="status" style={{ padding: "8px 24px", color: "#8b949e", fontSize: 12 }}>
          {notice}
        </div>
      )}
    </div>
  );
};

export default ApiKeys;
