import React, { useState, useEffect } from "react";
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

function AppContent(): React.JSX.Element {
  const { isAuthenticated } = useAuth();
  const [currentView, setCurrentView] = useState<NavView>("overview");

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

  // If unauthenticated and hash is login
  if (!isAuthenticated && window.location.hash === "#login") {
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
    <ControlPlaneProvider baseUrl={import.meta.env?.VITE_CONTROL_PLANE_URL || "http://localhost:8080"}>
      <AppContent />
    </ControlPlaneProvider>
  );
}
