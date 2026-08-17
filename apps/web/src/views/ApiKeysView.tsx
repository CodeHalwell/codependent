import React, { useState } from "react";
import { useApiKeys, useDaemons } from "@codypendent/control-plane-react";
import { Key, Laptop, Plus, Trash2, Copy, Check, Shield } from "lucide-react";
import { PublicationBadge, StatusBadge } from "../components/Badge.js";
import { EmptyState } from "../components/EmptyState.js";
import { LoadingSpinner } from "../components/LoadingSpinner.js";
import { Modal } from "../components/Modal.js";
import type { PairingChallenge } from "@codypendent/control-plane";

export function ApiKeysView(): React.JSX.Element {
  const [activeTab, setActiveTab] = useState<"keys" | "daemons">("keys");

  const {
    apiKeys,
    isLoading: keysLoading,
    createApiKey,
    revokeApiKey,
  } = useApiKeys();

  const {
    daemons,
    isLoading: daemonsLoading,
    createPairingChallenge,
    revokeDaemon,
  } = useDaemons();

  // Create API Key state
  const [showCreateKeyModal, setShowCreateKeyModal] = useState(false);
  const [keyName, setKeyName] = useState("");
  const [keyRole, setKeyRole] = useState("contributor");
  const [createdSecretToken, setCreatedSecretToken] = useState<string | null>(null);
  const [copiedToken, setCopiedToken] = useState(false);

  // Pairing challenge state
  const [showPairingModal, setShowPairingModal] = useState(false);
  const [activeChallenge, setActiveChallenge] = useState<PairingChallenge | null>(null);

  const handleCreateKey = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!keyName) return;
    const res = await createApiKey({
      name: keyName,
      role: keyRole,
    });
    setCreatedSecretToken(res.token);
    setKeyName("");
  };

  const handleStartPairing = async () => {
    const challenge = await createPairingChallenge({
      maxPublicationClass: "metadata-shared",
      acceptsRemoteApprovals: true,
      acceptsRunnerDispatch: false,
    });
    setActiveChallenge(challenge);
    setShowPairingModal(true);
  };

  const handleCopyToken = () => {
    if (createdSecretToken) {
      navigator.clipboard.writeText(createdSecretToken);
      setCopiedToken(true);
      setTimeout(() => setCopiedToken(false), 2000);
    }
  };

  return (
    <div data-testid="apikeys-view">
      <div className="page-header">
        <div>
          <h1 className="page-title">API Keys & Workstation Daemons</h1>
          <p className="page-description">
            Manage programmatic API access credentials and paired workstation daemons.
          </p>
        </div>
        {activeTab === "keys" ? (
          <button
            className="btn btn-primary"
            onClick={() => {
              setCreatedSecretToken(null);
              setShowCreateKeyModal(true);
            }}
            data-testid="create-key-btn"
          >
            <Plus size={16} />
            <span>Generate API Key</span>
          </button>
        ) : (
          <button
            className="btn btn-primary"
            onClick={handleStartPairing}
            data-testid="pair-daemon-btn"
          >
            <Laptop size={16} />
            <span>Pair New Workstation</span>
          </button>
        )}
      </div>

      {/* Tabs */}
      <div style={{ display: "flex", gap: "8px", marginBottom: "16px" }}>
        <button
          className={`btn ${activeTab === "keys" ? "btn-primary" : "btn-secondary"}`}
          onClick={() => setActiveTab("keys")}
          data-testid="tab-api-keys"
        >
          <Key size={15} /> API Keys ({apiKeys.length})
        </button>
        <button
          className={`btn ${activeTab === "daemons" ? "btn-primary" : "btn-secondary"}`}
          onClick={() => setActiveTab("daemons")}
          data-testid="tab-daemons"
        >
          <Laptop size={15} /> Paired Daemons ({daemons.length})
        </button>
      </div>

      {activeTab === "keys" ? (
        keysLoading ? (
          <LoadingSpinner label="Loading API keys..." />
        ) : apiKeys.length === 0 ? (
          <EmptyState
            icon={<Key size={36} />}
            title="No API Keys"
            description="Generate an API key for CI/CD automation or CLI access."
            action={
              <button
                className="btn btn-primary"
                onClick={() => setShowCreateKeyModal(true)}
              >
                Generate API Key
              </button>
            }
          />
        ) : (
          <div className="table-container">
            <table className="data-table" data-testid="api-keys-table">
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Key Prefix</th>
                  <th>Role</th>
                  <th>Created</th>
                  <th>Last Used</th>
                  <th style={{ textAlign: "right" }}>Actions</th>
                </tr>
              </thead>
              <tbody>
                {apiKeys.map((key) => (
                  <tr key={key.id} data-testid={`key-row-${key.id}`}>
                    <td>
                      <strong>{key.name}</strong>
                    </td>
                    <td>
                      <code>{key.keyPrefix}...</code>
                    </td>
                    <td>
                      <span className="badge badge-metadata">{key.role}</span>
                    </td>
                    <td>{new Date(key.createdAt).toLocaleDateString()}</td>
                    <td>{key.lastUsedAt ? new Date(key.lastUsedAt).toLocaleDateString() : "Never"}</td>
                    <td style={{ textAlign: "right" }}>
                      <button
                        className="btn btn-secondary btn-sm"
                        style={{ color: "var(--danger)" }}
                        onClick={() => revokeApiKey(key.id)}
                        data-testid={`revoke-key-btn-${key.id}`}
                      >
                        <Trash2 size={13} /> Revoke
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )
      ) : daemonsLoading ? (
        <LoadingSpinner label="Loading paired daemons..." />
      ) : daemons.length === 0 ? (
        <EmptyState
          icon={<Laptop size={36} />}
          title="No Paired Daemons"
          description="Connect a workstation daemon to synchronize metadata and remote approvals."
          action={
            <button className="btn btn-primary" onClick={handleStartPairing}>
              Pair New Workstation
            </button>
          }
        />
      ) : (
        <div className="table-container">
          <table className="data-table" data-testid="daemons-table">
            <thead>
              <tr>
                <th>Workstation Display Name</th>
                <th>Max Publication Class</th>
                <th>Remote Approvals</th>
                <th>Status</th>
                <th>Last Seen</th>
                <th style={{ textAlign: "right" }}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {daemons.map((daemon) => (
                <tr key={daemon.id} data-testid={`daemon-row-${daemon.id}`}>
                  <td>
                    <strong>{daemon.displayName}</strong>
                  </td>
                  <td>
                    <PublicationBadge publicationClass={daemon.maxPublicationClass} />
                  </td>
                  <td>
                    {daemon.acceptsRemoteApprovals ? (
                      <span style={{ color: "var(--success)" }}>Enabled</span>
                    ) : (
                      <span style={{ color: "var(--text-muted)" }}>Disabled</span>
                    )}
                  </td>
                  <td>
                    <StatusBadge status={daemon.state} />
                  </td>
                  <td>{daemon.lastSeenAt ? new Date(daemon.lastSeenAt).toLocaleString() : "Never"}</td>
                  <td style={{ textAlign: "right" }}>
                    {daemon.state === "active" && (
                      <button
                        className="btn btn-secondary btn-sm"
                        style={{ color: "var(--danger)" }}
                        onClick={() => revokeDaemon(daemon.id, "Revoked by user in web console")}
                        data-testid={`revoke-daemon-btn-${daemon.id}`}
                      >
                        Revoke Pairing
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {/* Generate API Key Modal */}
      <Modal
        isOpen={showCreateKeyModal}
        onClose={() => {
          setShowCreateKeyModal(false);
          setCreatedSecretToken(null);
        }}
        title={createdSecretToken ? "API Key Generated" : "Generate API Key"}
        footer={
          createdSecretToken ? (
            <button
              className="btn btn-primary"
              onClick={() => {
                setShowCreateKeyModal(false);
                setCreatedSecretToken(null);
              }}
              data-testid="done-key-btn"
            >
              Done
            </button>
          ) : (
            <>
              <button
                className="btn btn-secondary"
                onClick={() => setShowCreateKeyModal(false)}
              >
                Cancel
              </button>
              <button className="btn btn-primary" onClick={handleCreateKey} data-testid="confirm-create-key-btn">
                Generate Key
              </button>
            </>
          )
        }
      >
        {createdSecretToken ? (
          <div>
            <div
              style={{
                backgroundColor: "var(--warning-surface)",
                border: "1px solid var(--warning)",
                borderRadius: "var(--radius-md)",
                padding: "12px",
                marginBottom: "16px",
                fontSize: "13px",
              }}
            >
              <strong>Important:</strong> Copy this secret token now. You will not be able to see it again!
            </div>
            <div style={{ display: "flex", gap: "8px", alignItems: "center" }}>
              <input
                className="form-input"
                readOnly
                value={createdSecretToken}
                style={{ fontFamily: "var(--font-family-mono)", fontSize: "12px" }}
                data-testid="secret-token-display"
              />
              <button className="btn btn-secondary" onClick={handleCopyToken} data-testid="copy-token-btn">
                {copiedToken ? <Check size={14} color="var(--success)" /> : <Copy size={14} />}
              </button>
            </div>
          </div>
        ) : (
          <form onSubmit={handleCreateKey}>
            <div className="form-group">
              <label className="form-label">Key Name</label>
              <input
                className="form-input"
                value={keyName}
                onChange={(e) => setKeyName(e.target.value)}
                placeholder="e.g. GitHub Actions CI"
                required
                data-testid="key-name-input"
              />
            </div>
            <div className="form-group">
              <label className="form-label">Role Scope</label>
              <select
                className="form-select"
                value={keyRole}
                onChange={(e) => setKeyRole(e.target.value)}
                data-testid="key-role-select"
              >
                <option value="observer">Observer (Read-only)</option>
                <option value="contributor">Contributor (Execute runs)</option>
                <option value="approver">Approver (Review and decide actions)</option>
                <option value="maintainer">Maintainer (Full project admin)</option>
              </select>
            </div>
          </form>
        )}
      </Modal>

      {/* Pairing Challenge Modal */}
      <Modal
        isOpen={showPairingModal}
        onClose={() => setShowPairingModal(false)}
        title="Pair Workstation Daemon"
        footer={
          <button className="btn btn-primary" onClick={() => setShowPairingModal(false)}>
            Close
          </button>
        }
      >
        {activeChallenge && (
          <div>
            <p style={{ fontSize: "13px", color: "var(--text-secondary)", marginBottom: "16px" }}>
              Run the following command on your local machine to complete the pairing challenge.
            </p>

            <div className="form-group">
              <label className="form-label">One-Time Pairing Code</label>
              <div className="code-block" style={{ fontSize: "16px", fontWeight: "bold", textAlign: "center" }}>
                {activeChallenge.code}
              </div>
            </div>

            <div className="form-group">
              <label className="form-label">Consent Manifest Preview</label>
              <div
                style={{
                  backgroundColor: "var(--bg-app)",
                  padding: "12px",
                  borderRadius: "var(--radius-sm)",
                  fontSize: "12px",
                  color: "var(--text-secondary)",
                }}
              >
                <div style={{ display: "flex", alignItems: "center", gap: "6px", marginBottom: "6px" }}>
                  <Shield size={14} color="var(--accent)" />
                  <strong>Consent Scope:</strong>
                </div>
                <div>Max Publication: <code>{activeChallenge.requestedScope.maxPublicationClass}</code></div>
                <div>Remote Approvals: <code>{activeChallenge.requestedScope.acceptsRemoteApprovals ? "Yes" : "No"}</code></div>
                <div>Runner Dispatch: <code>{activeChallenge.requestedScope.acceptsRunnerDispatch ? "Yes" : "No"}</code></div>
              </div>
            </div>
          </div>
        )}
      </Modal>
    </div>
  );
}
