import React from "react";
import { Navigation } from "./components/Navigation.js";
import { Transcript } from "./components/Transcript.js";
import { Composer } from "./components/Composer.js";
import { RemoteUiRenderer } from "./components/RemoteUiRenderer.js";
import { useDaemon } from "./useDaemon.js";
import type { DesktopTransport } from "./transport.js";
import type { UiDocument } from "@codypendent/ui";

interface AppProps {
  /**
   * How to reach `codypendentd`. Defaults to the Tauri shell bridge, which
   * yields `null` outside the shell — the app then shows a disconnected state
   * with the reason. Tests inject a stub to drive a connected client.
   */
  makeTransport?: () => DesktopTransport | null;
}

export const App: React.FC<AppProps> = ({ makeTransport }) => {
  const { state, submit, cancel, selectSession } = useDaemon(makeTransport);
  const connected = state.status === "connected";
  // Remote UI documents arrive with adoption 14 milestone 5; until the daemon
  // streams them there are none, and the panel stays closed.
  const documents = new Map<string, UiDocument>();

  return (
    <div style={{ display: "flex", width: "100vw", height: "100vh", overflow: "hidden", background: "#121417" }}>
      <Navigation
        sessions={state.sessions}
        activeSessionId={state.activeSessionId}
        onSelectSession={(id) => void selectSession(id)}
        connectionStatus={state.status}
        statusDetail={state.detail}
      />
      <main style={{ flex: 1, display: "flex", flexDirection: "column", height: "100vh" }}>
        <Transcript
          items={state.transcript}
          connectionStatus={state.status}
          statusDetail={state.detail}
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
      </main>
      <RemoteUiRenderer documents={documents} />
    </div>
  );
};
