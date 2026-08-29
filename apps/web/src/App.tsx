import React, { useState, useEffect, useRef } from "react";
import { ControlPlaneProvider, useAuth } from "@codypendent/control-plane-react";
import { Header } from "./components/Header.js";
import { Sidebar, type NavView } from "./components/Sidebar.js";
import { OverviewView } from "./views/OverviewView.js";
import { SessionsView } from "./views/SessionsView.js";
import { ApprovalsView } from "./views/ApprovalsView.js";
import { AuditLogsView } from "./views/AuditLogsView.js";
import { UsersView } from "./views/UsersView.js";
import { ApiKeysView } from "./views/ApiKeysView.js";
import { SettingsView } from "./views/SettingsView.js";
import { LoginView } from "./views/LoginView.js";
import { CallbackView } from "./views/CallbackView.js";
import { readStoredToken, writeStoredToken } from "./session.js";

/** What the OAuth redirect is currently doing. */
type CallbackStatus =
  | { kind: "none" }
  | { kind: "exchanging" }
  | { kind: "failed"; message: string };

/**
 * Complete an OAuth sign-in when the provider redirects back.
 *
 * The login buttons sent the browser to GitHub/OIDC and nothing here ever
 * consumed the redirect: `handleCallback` had no caller, so no token was ever
 * installed and the only working path was pasting a bearer token by hand.
 *
 * The authorization code is single-use, so it is stripped from the URL
 * *before* the exchange is awaited. That way a reload, a re-render, or
 * StrictMode's deliberate double-invoke cannot present a spent code and turn a
 * successful sign-in into a failed one. The ref guards the same race within a
 * single mount.
 */
function useOAuthCallback(): CallbackStatus {
  const { handleCallback } = useAuth();
  const [status, setStatus] = useState<CallbackStatus>(() => {
    const params = new URLSearchParams(window.location.search);
    if (params.get("error")) return { kind: "failed", message: describeOAuthError(params) };
    return params.get("code") && params.get("state") ? { kind: "exchanging" } : { kind: "none" };
  });
  const started = useRef(false);

  useEffect(() => {
    if (started.current) return;
    started.current = true;

    const params = new URLSearchParams(window.location.search);
    const code = params.get("code");
    const state = params.get("state");
    const failed = params.get("error");

    if (!failed && !(code && state)) return;

    // Strip the credentials from the address bar before anything can be
    // retried against them, and before they reach a bookmark or a referrer.
    window.history.replaceState({}, "", window.location.pathname + window.location.hash);

    if (failed || !code || !state) {
      setStatus({ kind: "failed", message: describeOAuthError(params) });
      return;
    }

    void handleCallback({ code, state })
      .then(() => {
        setStatus({ kind: "none" });
        window.location.hash = "#overview";
      })
      .catch((err: unknown) => {
        setStatus({
          kind: "failed",
          message: err instanceof Error ? err.message : "The sign-in could not be completed.",
        });
      });
  }, [handleCallback]);

  return status;
}

/** The provider's own words when it has them, ours when it does not. */
function describeOAuthError(params: URLSearchParams): string {
  const description = params.get("error_description");
  const code = params.get("error");
  if (description) return description;
  if (code) return `The identity provider refused the sign-in (${code}).`;
  return "The sign-in could not be completed.";
}

function AppContent(): React.JSX.Element {
  const { isAuthenticated, token } = useAuth();
  const callback = useOAuthCallback();
  const [currentView, setCurrentView] = useState<NavView>("overview");

  // Persist whatever token is in play — the one just minted by an OAuth
  // exchange, one pasted by hand, and the rotated one `onTokenRefresh`
  // installs. Watching the token rather than the act of setting it is what
  // covers that last case, which is the one that silently signed people out
  // on reload.
  useEffect(() => {
    writeStoredToken(token);
  }, [token]);

  // Sync hash routing
  useEffect(() => {
    const handleHashChange = () => {
      const hash = window.location.hash.replace(/^#/, "");
      if (
        [
          "overview",
          "sessions",
          "approvals",
          "audit",
          "users",
          "apikeys",
          "settings",
        ].includes(hash)
      ) {
        setCurrentView(hash as NavView);
      }
    };

    handleHashChange();
    window.addEventListener("hashchange", handleHashChange);
    return () => window.removeEventListener("hashchange", handleHashChange);
  }, []);

  const handleNavigate = (view: NavView) => {
    setCurrentView(view);
    window.location.hash = `#${view}`;
  };

  // The redirect back from the identity provider outranks everything: until
  // the exchange settles there is no session to judge, and rendering the login
  // screen underneath it reads as a failure at the moment it is succeeding.
  if (callback.kind === "exchanging") {
    return <CallbackView />;
  }
  if (callback.kind === "failed") {
    return <CallbackView error={callback.message} />;
  }

  // Every console route exposes organization data. A hash is navigation, not
  // authorization: an anonymous visitor must never mount protected views just
  // because they entered `#sessions` (or no hash) directly.
  if (!isAuthenticated) {
    return <LoginView />;
  }

  return (
    <div className="app-layout">
      <Header onNavigateToInbox={() => handleNavigate("approvals")} />
      <div className="app-main-container">
        <Sidebar currentView={currentView} onNavigate={handleNavigate} />
        <main className="app-content">
          {currentView === "overview" && (
            <OverviewView
              onNavigateToSessions={() => handleNavigate("sessions")}
              onNavigateToApprovals={() => handleNavigate("approvals")}
              onNavigateToAudit={() => handleNavigate("audit")}
              onNavigateToDaemons={() => handleNavigate("apikeys")}
            />
          )}
          {currentView === "sessions" && <SessionsView />}
          {currentView === "approvals" && <ApprovalsView />}
          {currentView === "audit" && <AuditLogsView />}
          {currentView === "users" && <UsersView />}
          {currentView === "apikeys" && <ApiKeysView />}
          {currentView === "settings" && <SettingsView />}
        </main>
      </div>
    </div>
  );
}

export function App(): React.JSX.Element {
  return (
    <ControlPlaneProvider
      baseUrl={import.meta.env?.VITE_CONTROL_PLANE_URL || "http://localhost:8080"}
      token={readStoredToken()}
    >
      <AppContent />
    </ControlPlaneProvider>
  );
}
