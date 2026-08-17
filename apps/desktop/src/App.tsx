import React, { useState } from "react";
import { Navigation, type DesktopView } from "./components/Navigation.js";
import { Transcript } from "./components/Transcript.js";
import { Composer } from "./components/Composer.js";
import { InboxView } from "./components/InboxView.js";
import { AnalyticsDashboard } from "./components/AnalyticsDashboard.js";
import { RemoteUiRenderer } from "./components/RemoteUiRenderer.js";
import { useDaemon } from "./useDaemon.js";
import type { DesktopTransport } from "./transport.js";
import type { InboxDeepLink } from "@codypendent/protocol";
import type { UiDocument } from "@codypendent/ui";

interface AppProps {
  /**
   * How to reach `codypendentd`. Defaults to the Tauri shell bridge, which
   * yields `null` outside the shell — the app then shows a disconnected state
   * with the reason. Tests inject a stub to drive a connected client.
   */
  makeTransport?: () => DesktopTransport | null;
  initialView?: DesktopView;
}

export const App: React.FC<AppProps> = ({ makeTransport, initialView = "sessions" }) => {
  const [currentView, setCurrentView] = useState<DesktopView>(initialView);
  const {
    state,
    submit,
    cancel,
    selectSession,
    resolveApproval,
    loadInbox,
    acknowledgeInbox,
    dismissInbox,
    queryAnalytics,
    exportAnalytics,
  } = useDaemon(makeTransport);

  const connected = state.status === "connected";
  // Remote UI documents arrive with adoption 14 milestone 5; until the daemon
  // streams them there are none, and the panel stays closed.
  const documents = new Map<string, UiDocument>();

  const handleNavigateInbox = (deepLink: InboxDeepLink) => {
    if (deepLink.type === "Session") {
      void selectSession(deepLink.session_id);
      setCurrentView("sessions");
    } else if (deepLink.type === "Run") {
      void selectSession(deepLink.session_id);
      setCurrentView("sessions");
    } else if (deepLink.type === "Approval") {
      setCurrentView("sessions");
    } else if (deepLink.type === "Question") {
      setCurrentView("sessions");
    }
  };

  return (
    <div style={{ display: "flex", width: "100vw", height: "100vh", overflow: "hidden", background: "#121417" }}>
      <Navigation
        sessions={state.sessions}
        activeSessionId={state.activeSessionId}
        onSelectSession={(id) => {
          setCurrentView("sessions");
          void selectSession(id);
        }}
        connectionStatus={state.status}
        statusDetail={state.detail}
        currentView={currentView}
        onSelectView={setCurrentView}
        unreadInboxCount={state.unreadInboxCount}
      />

      <main style={{ flex: 1, display: "flex", flexDirection: "column", height: "100vh", overflow: "hidden" }}>
        {currentView === "sessions" && (
          <>
            <Transcript
              items={state.transcript}
              connectionStatus={state.status}
              statusDetail={state.detail}
              onApprove={connected ? (approvalId) => void resolveApproval(approvalId, "approve") : undefined}
              onReject={connected ? (approvalId) => void resolveApproval(approvalId, "reject") : undefined}
            />
            {state.error && (
              <div
                role="alert"
                style={{
                  padding: "8px 24px",
                  background: "#2d1214",
                  borderTop: "1px solid #da3633",
                  color: "#ffa198",
                  fontSize: 12,
                }}
              >
                {state.error}
              </div>
            )}
            <Composer
              onSend={(text) => void submit(text)}
              onCancel={() => void cancel()}
              isRunning={state.isRunning}
              disabled={!connected}
              canCancel={connected && state.activeRunId !== null}
            />
          </>
        )}

        {currentView === "inbox" && (
          <InboxView
            entries={state.inbox}
            onAcknowledge={(id) => void acknowledgeInbox(id)}
            onDismiss={(id) => void dismissInbox(id)}
            onNavigate={handleNavigateInbox}
            onApprove={connected ? (approvalId) => void resolveApproval(approvalId, "approve") : undefined}
            onReject={connected ? (approvalId) => void resolveApproval(approvalId, "reject") : undefined}
            onRefresh={() => void loadInbox()}
            unavailable={state.inboxStatus === "unavailable" ? state.inboxDetail : null}
          />
        )}

        {currentView === "analytics" && (
          <AnalyticsDashboard
            onQueryAnalytics={queryAnalytics}
            onExportAnalytics={exportAnalytics}
          />
        )}
      </main>

      <RemoteUiRenderer documents={documents} />
    </div>
  );
};
