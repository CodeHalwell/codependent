/**
 * Configured models: list, pin one for the next run, add one, remove one.
 *
 * Rows come from `models.toml`, parsed in the shell by the same
 * `codypendent_runtime::models::load_models` the daemon and the CLI use. A
 * missing file is an empty list ("nothing configured yet"); a file that exists
 * and does not parse is an error, and the two are drawn differently on purpose.
 *
 * What is NOT here, and why: the TUI's model picker carries a readiness badge
 * (Ready / Unverified / Unavailable) computed by probing local endpoints and
 * resolving credentials through `ModelRegistry`. The shell does not compile
 * that machinery — it configures models; the daemon runs them — so no readiness
 * is shown at all rather than a confident badge nothing measured. Credential
 * PRESENCE is real and is shown; the key value is not, and never crosses the
 * bridge.
 *
 * Pinning a model sends nothing. It stages `StartRun.model` for the next run,
 * exactly as `pending_model` does in the TUI, and the shell refuses a pin that
 * names a model `models.toml` does not contain.
 */
import React, { useCallback, useEffect, useMemo, useState } from "react";

import {
  describeError,
  describeKeyStatus,
  localConfigClient,
  shellAvailable,
  NO_SHELL,
  type CatalogModelsView,
  type LocalConfigClient,
  type ModelRow,
  type ModelsView,
  type ProviderRow,
} from "./localConfig";

export interface ModelPickerProps {
  client?: LocalConfigClient;
  /** Told the pinned model after the shell accepted it, for a status line. */
  onModelPinned?: (modelId: string | null) => void;
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

const BUTTON: React.CSSProperties = {
  padding: "4px 10px",
  borderRadius: 6,
  border: "1px solid #30363d",
  background: "transparent",
  color: "#c9d1d9",
  cursor: "pointer",
  font: "inherit",
  fontSize: 12,
};

/** The host part of a base URL, for a label. A trim, not a URL parse. */
function endpointHost(baseUrl: string): string {
  const rest = (baseUrl.includes("://") ? baseUrl.split("://")[1] : baseUrl).replace(/^\/+/, "");
  const host = rest.split("/")[0];
  return host.length > 0 ? host : baseUrl;
}

export const ModelPicker: React.FC<ModelPickerProps> = ({ client, onModelPinned }) => {
  const api = client ?? localConfigClient;
  const [view, setView] = useState<ModelsView | null>(null);
  const [unavailable, setUnavailable] = useState<string | null>(
    shellAvailable() || client ? null : NO_SHELL,
  );
  const [query, setQuery] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const [removing, setRemoving] = useState<ModelRow | null>(null);
  const [adding, setAdding] = useState(false);

  const load = useCallback(async () => {
    if (!shellAvailable() && !client) {
      setUnavailable(NO_SHELL);
      return;
    }
    try {
      setView(await api.listModels());
      setUnavailable(null);
    } catch (error) {
      // Not an empty list: `models.toml` could not be read.
      setView(null);
      setUnavailable(describeError(error));
    }
  }, [api, client]);

  useEffect(() => {
    void load();
  }, [load]);

  const matches = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const rows = view?.models ?? [];
    if (needle.length === 0) {
      return rows;
    }
    return rows.filter(
      (row) => row.id.toLowerCase().includes(needle) || row.model.toLowerCase().includes(needle),
    );
  }, [view, query]);

  const pin = async (modelId: string | null) => {
    try {
      await api.setRunModel(modelId);
      setNotice(
        modelId === null
          ? "model pin cleared — the daemon chooses for your next run"
          : `model set to ${modelId} — applies to your next run`,
      );
      onModelPinned?.(modelId);
    } catch (error) {
      setNotice(`could not pin model: ${describeError(error)}`);
    }
    await load();
  };

  const confirmRemove = async () => {
    if (!removing) {
      return;
    }
    try {
      await api.removeModel(removing.id);
      setNotice(`removed ${removing.id} from models.toml`);
    } catch (error) {
      setNotice(`could not remove model: ${describeError(error)}`);
    } finally {
      setRemoving(null);
    }
    await load();
  };

  return (
    <div style={PANEL} data-testid="model-picker">
      <header style={{ padding: "16px 24px", borderBottom: "1px solid #21262d" }}>
        <h2 style={{ margin: 0, fontSize: 16, fontWeight: 600 }}>Models</h2>
        <p style={{ margin: "4px 0 0", color: "#8b949e", fontSize: 12 }}>
          Configured in <code>{view?.models_path ?? "models.toml"}</code>. A pinned model applies to
          your next run.
        </p>
      </header>

      {unavailable ? (
        <div
          role="status"
          data-testid="model-picker-unavailable"
          style={{ padding: "32px 24px", color: "#d29922" }}
        >
          Models unavailable — {unavailable}
        </div>
      ) : view === null ? (
        <div role="status" style={{ padding: "32px 24px", color: "#8b949e" }}>
          Reading models&hellip;
        </div>
      ) : (
        <>
          <div style={{ padding: "12px 24px", display: "flex", gap: 8, borderBottom: "1px solid #21262d" }}>
            <input
              type="search"
              value={query}
              placeholder="Filter models"
              onChange={(event) => setQuery(event.target.value)}
              style={{
                flex: 1,
                padding: "6px 10px",
                borderRadius: 6,
                border: "1px solid #30363d",
                background: "#0d1117",
                color: "#c9d1d9",
                font: "inherit",
              }}
            />
            <button type="button" style={BUTTON} onClick={() => setAdding(true)}>
              Add model
            </button>
            {view.pinned && (
              <button type="button" style={BUTTON} onClick={() => void pin(null)}>
                Clear pin
              </button>
            )}
          </div>

          {view.warnings.map((warning) => (
            <div key={warning} role="status" style={{ padding: "4px 24px", color: "#d29922", fontSize: 12 }}>
              {warning}
            </div>
          ))}

          <div style={{ flex: 1, overflowY: "auto", padding: "8px 16px" }}>
            {matches.length === 0 ? (
              <div data-testid="model-picker-empty" style={{ padding: 24, color: "#8b949e" }}>
                {view.models.length === 0
                  ? view.configured
                    ? `${view.models_path} has no [[model]] entries yet.`
                    : `No ${view.models_path} yet — add a model to create one.`
                  : `No model matches “${query}”.`}
              </div>
            ) : (
              <ul style={{ listStyle: "none", margin: 0, padding: 0, display: "flex", flexDirection: "column", gap: 6 }}>
                {matches.map((row) => {
                  const status = describeKeyStatus(row.key);
                  const pinned = view.pinned === row.id;
                  return (
                    <li
                      key={row.id}
                      style={{
                        padding: "10px 12px",
                        borderRadius: 6,
                        background: "#161b22",
                        border: `1px solid ${pinned ? "#58a6ff" : "#21262d"}`,
                        display: "flex",
                        alignItems: "center",
                        gap: 12,
                      }}
                    >
                      <div style={{ flex: 1, minWidth: 0 }}>
                        <div style={{ display: "flex", alignItems: "baseline", gap: 8, flexWrap: "wrap" }}>
                          <span style={{ fontWeight: 600 }}>{row.id}</span>
                          {pinned && (
                            <span style={{ ...BADGE, color: "#58a6ff" }}>next run</span>
                          )}
                          {row.provider_id && <span style={BADGE}>{row.provider_id}</span>}
                          <span style={{ ...BADGE, color: status.color }}>
                            {status.glyph} {status.label}
                          </span>
                        </div>
                        <div style={{ color: "#8b949e", fontSize: 12, marginTop: 2 }}>
                          {row.model}
                          {row.base_url.trim().length > 0 && ` · ${endpointHost(row.base_url)}`}
                          {" · context "}
                          {/* Unknown stays unknown. An em dash, never a 0. */}
                          {row.context_tokens === null
                            ? "—"
                            : `${row.context_tokens.toLocaleString()} tokens`}
                        </div>
                      </div>
                      <button type="button" style={BUTTON} disabled={pinned} onClick={() => void pin(row.id)}>
                        {pinned ? "Pinned" : "Use next"}
                      </button>
                      <button
                        type="button"
                        style={{ ...BUTTON, color: "#ff7b72" }}
                        onClick={() => setRemoving(row)}
                      >
                        Remove
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
        </>
      )}

      {removing && (
        <div
          role="alertdialog"
          aria-label="Remove model"
          data-testid="model-remove-confirm"
          style={{ borderTop: "1px solid #21262d", padding: "16px 24px" }}
        >
          <p style={{ margin: 0, fontSize: 13 }}>
            Remove <strong>{removing.id}</strong> from <code>{view?.models_path}</code>? Its stored
            key is deleted with it. Comments and every other table in the file are preserved.
          </p>
          <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
            <button
              type="button"
              onClick={() => void confirmRemove()}
              style={{ ...BUTTON, border: "1px solid #da3633", background: "#da3633", color: "#fff" }}
            >
              Remove
            </button>
            <button type="button" style={BUTTON} onClick={() => setRemoving(null)}>
              Cancel
            </button>
          </div>
        </div>
      )}

      {adding && (
        <AddModelFlow
          api={api}
          onCancel={() => setAdding(false)}
          onAdded={async (id) => {
            setAdding(false);
            setNotice(`added ${id} to models.toml`);
            await load();
          }}
        />
      )}

      {notice && (
        <div role="status" style={{ padding: "8px 24px", color: "#8b949e", fontSize: 12 }}>
          {notice}
        </div>
      )}
    </div>
  );
};

/**
 * The add-model flow: provider, then model, then key, then the id it will be
 * selected by. The same order as the TUI's five-step overlay chain.
 *
 * The model list offered is the provider catalog's CURATED rows — real data,
 * read offline. The TUI can additionally GET `{base_url}/models` with the
 * provider's key; the shell does not, so the list is labelled as the catalog's
 * rather than presented as exhaustive, and any model name can still be typed.
 *
 * Every refusal that matters is enforced in the shell (blank id, blank key
 * treated as absent, corrupt `auth.json` aborting before `models.toml` is
 * touched, a provider whose adapter is not installed). What is duplicated here
 * is only what stops a pointless round trip.
 */
const AddModelFlow: React.FC<{
  api: LocalConfigClient;
  onCancel: () => void;
  onAdded: (displayId: string) => void | Promise<void>;
}> = ({ api, onCancel, onAdded }) => {
  const [providers, setProviders] = useState<ProviderRow[] | null>(null);
  const [providersUnavailable, setProvidersUnavailable] = useState<string | null>(null);
  const [provider, setProvider] = useState<ProviderRow | null>(null);
  const [catalog, setCatalog] = useState<CatalogModelsView | null>(null);
  const [catalogError, setCatalogError] = useState<string | null>(null);
  const [model, setModel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [displayId, setDisplayId] = useState("");
  const [displayIdTouched, setDisplayIdTouched] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const view = await api.listProviders();
        if (!cancelled) {
          // The community bridge is a trust decision handled by the provider
          // catalog view, and this build cannot install it — it is not an
          // option in an add flow.
          setProviders(
            view.providers.filter((row) => row.available && !row.community_consent_required),
          );
        }
      } catch (loadError) {
        if (!cancelled) {
          setProviders(null);
          setProvidersUnavailable(describeError(loadError));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [api]);

  useEffect(() => {
    if (!provider) {
      return;
    }
    let cancelled = false;
    setCatalog(null);
    setCatalogError(null);
    void (async () => {
      try {
        const view = await api.listCatalogModels(provider.id);
        if (!cancelled) {
          setCatalog(view);
        }
      } catch (loadError) {
        if (!cancelled) {
          setCatalogError(describeError(loadError));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [api, provider]);

  // The TUI's default display id is `<provider>/<model>`; a typed one wins.
  const effectiveId =
    displayIdTouched || displayId.length > 0
      ? displayId
      : provider && model
        ? `${provider.id}/${model}`
        : "";

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!provider) {
      setError("choose a provider first");
      return;
    }
    if (model.trim().length === 0) {
      setError("model name must not be blank");
      return;
    }
    if (effectiveId.trim().length === 0) {
      setError("model id must not be blank");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const contextTokens =
        catalog?.models.find((row) => row.id === model.trim())?.context_tokens ?? null;
      await api.addModel({
        displayId: effectiveId.trim(),
        providerId: provider.id,
        model: model.trim(),
        // A blank key is `null`: the shell treats blank exactly as absent, and
        // sending an empty string would risk shadowing a valid api_key_env.
        apiKey: apiKey.trim().length > 0 ? apiKey : null,
        contextTokens,
      });
      // Cleared before anything else can observe it.
      setApiKey("");
      await onAdded(effectiveId.trim());
    } catch (addError) {
      setApiKey("");
      setError(describeError(addError));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      onSubmit={submit}
      data-testid="add-model-flow"
      style={{ borderTop: "1px solid #21262d", padding: "16px 24px", display: "flex", flexDirection: "column", gap: 10 }}
    >
      <h3 style={{ margin: 0, fontSize: 14 }}>Add a model</h3>

      {providersUnavailable ? (
        <div role="status" style={{ color: "#d29922", fontSize: 12 }}>
          Provider catalog unavailable — {providersUnavailable}
        </div>
      ) : providers === null ? (
        <div role="status" style={{ color: "#8b949e", fontSize: 12 }}>
          Reading the provider catalog&hellip;
        </div>
      ) : (
        <label style={{ fontSize: 12, color: "#8b949e" }}>
          Provider
          <select
            value={provider?.id ?? ""}
            onChange={(event) => {
              const next = providers.find((row) => row.id === event.target.value) ?? null;
              setProvider(next);
              setModel("");
            }}
            style={{
              display: "block",
              width: "100%",
              marginTop: 4,
              padding: "6px 10px",
              borderRadius: 6,
              border: "1px solid #30363d",
              background: "#0d1117",
              color: "#c9d1d9",
              font: "inherit",
            }}
          >
            <option value="">Choose a provider…</option>
            {providers.map((row) => (
              <option key={row.id} value={row.id}>
                {row.name} ({row.protocol}
                {row.local ? ", local" : ""})
              </option>
            ))}
          </select>
        </label>
      )}

      {provider && (
        <>
          <label style={{ fontSize: 12, color: "#8b949e" }}>
            Model
            <input
              list="catalog-models"
              value={model}
              onChange={(event) => setModel(event.target.value)}
              placeholder="provider-side model name"
              style={{
                display: "block",
                width: "100%",
                marginTop: 4,
                padding: "6px 10px",
                borderRadius: 6,
                border: "1px solid #30363d",
                background: "#0d1117",
                color: "#c9d1d9",
                font: "inherit",
              }}
            />
          </label>
          <datalist id="catalog-models">
            {(catalog?.models ?? []).map((row) => (
              <option key={row.id} value={row.id}>
                {row.name ?? row.id}
              </option>
            ))}
          </datalist>
          {catalogError ? (
            <div role="status" style={{ color: "#d29922", fontSize: 12 }}>
              Catalog models unavailable — {catalogError}
            </div>
          ) : catalog ? (
            <div style={{ color: "#8b949e", fontSize: 12 }}>
              {catalog.models.length === 0
                ? "The catalog ships no curated rows for this provider — type the model name."
                : `${catalog.models.length} curated rows offered. `}
              {catalog.live_listing_unavailable}
            </div>
          ) : null}

          {/* The key step exists only when the provider needs one and none
              already resolves — the TUI's `requires_key && !has_key` branch. */}
          {provider.requires_key && !provider.has_key && (
            <label style={{ fontSize: 12, color: "#8b949e" }}>
              API key
              <input
                // Masked, with no reveal control. The value is written to
                // auth.json by the shell and is cleared here on every path.
                type="password"
                value={apiKey}
                autoComplete="off"
                autoCorrect="off"
                autoCapitalize="off"
                spellCheck={false}
                onChange={(event) => setApiKey(event.target.value)}
                style={{
                  display: "block",
                  width: "100%",
                  marginTop: 4,
                  padding: "6px 10px",
                  borderRadius: 6,
                  border: "1px solid #30363d",
                  background: "#0d1117",
                  color: "#c9d1d9",
                  font: "inherit",
                }}
              />
            </label>
          )}
          {provider.requires_key && provider.has_key && (
            <div style={{ color: "#3fb950", fontSize: 12 }}>
              A key already resolves for {provider.name} — no key needed here.
            </div>
          )}

          <label style={{ fontSize: 12, color: "#8b949e" }}>
            Id to select it by
            <input
              value={effectiveId}
              onChange={(event) => {
                setDisplayIdTouched(true);
                setDisplayId(event.target.value);
              }}
              style={{
                display: "block",
                width: "100%",
                marginTop: 4,
                padding: "6px 10px",
                borderRadius: 6,
                border: "1px solid #30363d",
                background: "#0d1117",
                color: "#c9d1d9",
                font: "inherit",
              }}
            />
          </label>
        </>
      )}

      {error && (
        <div role="status" style={{ color: "#ff7b72", fontSize: 12 }}>
          {error}
        </div>
      )}

      <div style={{ display: "flex", gap: 8 }}>
        <button
          type="submit"
          disabled={busy || !provider}
          style={{ ...BUTTON, border: "1px solid #238636", background: "#238636", color: "#fff" }}
        >
          {busy ? "Adding…" : "Add model"}
        </button>
        <button type="button" style={BUTTON} onClick={onCancel}>
          Cancel
        </button>
      </div>
    </form>
  );
};

export default ModelPicker;
