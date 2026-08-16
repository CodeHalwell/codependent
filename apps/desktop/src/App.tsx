import React from "react";
import { Navigation } from "./components/Navigation.js";
import { Transcript } from "./components/Transcript.js";
import { Composer } from "./components/Composer.js";
import { RemoteUiRenderer } from "./components/RemoteUiRenderer.js";
import type { ConnectionStatus } from "./types.js";
import type { UiDocument } from "@codypendent/ui";

export const App: React.FC = () => {
  const connectionStatus: ConnectionStatus = "disconnected";
  const documents = new Map<string, UiDocument>();

  return (
    <div style={{ display: "flex", width: "100vw", height: "100vh", overflow: "hidden", background: "#121417" }}>
      <Navigation
        sessions={[]}
        activeSessionId={null}
        onSelectSession={() => undefined}
        onCreateSession={() => undefined}
        connectionStatus={connectionStatus}
      />
      <main style={{ flex: 1, display: "flex", flexDirection: "column", height: "100vh" }}>
        <Transcript items={[]} connectionStatus={connectionStatus} />
        <Composer
          onSend={() => undefined}
          isRunning={false}
          disabled
        />
      </main>
      <RemoteUiRenderer documents={documents} />
    </div>
  );
};
