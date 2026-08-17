import React, { useState } from "react";
import { useAuth } from "@codypendent/control-plane-react";
import { Key, Shield } from "lucide-react";

export function LoginView(): React.JSX.Element {
  const { loginWithGitHub, loginWithOidc, setAuthToken, isLoading, error } = useAuth();
  const [manualToken, setManualToken] = useState("");

  const handleManualTokenSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!manualToken) return;
    setAuthToken(manualToken.trim());
    window.location.hash = "#overview";
  };

  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        minHeight: "100vh",
        padding: "20px",
      }}
      data-testid="login-view"
    >
      <div className="card" style={{ width: "100%", maxWidth: "420px", padding: "32px 24px" }}>
        <div style={{ textAlign: "center", marginBottom: "24px" }}>
          <div
            className="brand-icon"
            style={{ width: "40px", height: "40px", margin: "0 auto 12px auto", fontSize: "20px" }}
          >
            C
          </div>
          <h1 style={{ fontSize: "20px", fontWeight: 700 }}>Codypendent Control Plane</h1>
          <p style={{ fontSize: "13px", color: "var(--text-secondary)", marginTop: "6px" }}>
            Sign in to access your organization workspace, runs, and audit logs.
          </p>
        </div>

        {error && (
          <div
            style={{
              backgroundColor: "var(--danger-surface)",
              color: "var(--danger)",
              padding: "10px",
              borderRadius: "var(--radius-sm)",
              fontSize: "13px",
              marginBottom: "16px",
            }}
          >
            {error.message}
          </div>
        )}

        <div style={{ display: "flex", flexDirection: "column", gap: "12px", marginBottom: "24px" }}>
          <button
            className="btn btn-secondary"
            style={{ width: "100%", padding: "10px" }}
            onClick={async () => {
              const url = await loginWithGitHub();
              window.location.href = url;
            }}
            disabled={isLoading}
            data-testid="github-login-btn"
          >
            <Shield size={16} /> Sign in with GitHub
          </button>

          <button
            className="btn btn-secondary"
            style={{ width: "100%", padding: "10px" }}
            onClick={async () => {
              const url = await loginWithOidc();
              window.location.href = url;
            }}
            disabled={isLoading}
            data-testid="oidc-login-btn"
          >
            <Shield size={16} /> Sign in with Single Sign-On (OIDC)
          </button>
        </div>

        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: "10px",
            color: "var(--text-muted)",
            fontSize: "12px",
            marginBottom: "20px",
          }}
        >
          <div style={{ flex: 1, height: "1px", backgroundColor: "var(--border-subtle)" }} />
          <span>OR USE BEARER / API KEY</span>
          <div style={{ flex: 1, height: "1px", backgroundColor: "var(--border-subtle)" }} />
        </div>

        <form onSubmit={handleManualTokenSubmit}>
          <div className="form-group">
            <label className="form-label">API Key / Access Token</label>
            <input
              type="password"
              className="form-input"
              value={manualToken}
              onChange={(e) => setManualToken(e.target.value)}
              placeholder="cody_live_..."
              required
              data-testid="manual-token-input"
            />
          </div>
          <button
            type="submit"
            className="btn btn-primary"
            style={{ width: "100%", padding: "10px" }}
            data-testid="manual-login-btn"
          >
            <Key size={15} /> Continue with Key
          </button>
        </form>
      </div>
    </div>
  );
}
