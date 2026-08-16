import React, { useState } from "react";
import { Navigation } from "./components/Navigation.js";
import { Transcript } from "./components/Transcript.js";
import { Composer } from "./components/Composer.js";
import { RemoteUiRenderer } from "./components/RemoteUiRenderer.js";
import type { SessionSummary, SessionId, TranscriptItem, RunRecord } from "./types.js";
import type { UiDocument } from "@codypendent/ui";

export const App: React.FC = () => {
  const [connected] = useState(true);
  const [sessions, setSessions] = useState<SessionSummary[]>([
    {
      id: "sess-1",
      title: "Fix unicode normalization in edit_file",
      created_at: new Date().toISOString(),
      last_activity_at: new Date().toISOString(),
      run_count: 2,
    },
    {
      id: "sess-2",
      title: "Hardening and Tauri Client build",
      created_at: new Date().toISOString(),
      last_activity_at: new Date().toISOString(),
      run_count: 1,
    },
  ]);
  const [activeSessionId, setActiveSessionId] = useState<SessionId | null>("sess-2");
  const [activeRun, setActiveRun] = useState<RunRecord | null>(null);
  const [transcript, setTranscript] = useState<TranscriptItem[]>([
    {
      id: "msg-1",
      type: "assistant",
      text: "Codypendent desktop attached to local daemon (codypendentd). Ready for requests.",
      timestamp: new Date().toISOString(),
    },
  ]);
  const [documents] = useState<Map<string, UiDocument>>(new Map());

  const handleSend = (text: string) => {
    const userMsg: TranscriptItem = {
      id: `user-${Date.now()}`,
      type: "user",
      text,
      timestamp: new Date().toISOString(),
    };
    setTranscript((prev) => [...prev, userMsg]);

    setActiveRun({
      id: `run-${Date.now()}`,
      session_id: activeSessionId || "sess-1",
      objective: text,
      state: "Running",
      model: "claude-3-7-sonnet",
      cost_usd: 0.0,
      duration_ms: 0,
      input_tokens: 120,
      output_tokens: 0,
      created_at: new Date().toISOString(),
    });

    setTimeout(() => {
      setTranscript((prev) => [
        ...prev,
        {
          id: `asst-${Date.now()}`,
          type: "assistant",
          text: `Executing task: "${text}" with verified security boundaries and isolated worktree.`,
          timestamp: new Date().toISOString(),
        },
      ]);
      setActiveRun(null);
    }, 1000);
  };

  const handleCreateSession = () => {
    const newId = `sess-${Date.now()}`;
    const newSession: SessionSummary = {
      id: newId,
      title: "New Session",
      created_at: new Date().toISOString(),
      last_activity_at: new Date().toISOString(),
      run_count: 0,
    };
    setSessions((prev) => [newSession, ...prev]);
    setActiveSessionId(newId);
    setTranscript([
      {
        id: `init-${Date.now()}`,
        type: "assistant",
        text: "New session created. What would you like to work on?",
        timestamp: new Date().toISOString(),
      },
    ]);
  };

  return (
    <div style={{ display: "flex", width: "100vw", height: "100vh", overflow: "hidden", background: "#121417" }}>
      <Navigation
        sessions={sessions}
        activeSessionId={activeSessionId}
        onSelectSession={setActiveSessionId}
        onCreateSession={handleCreateSession}
        connected={connected}
      />
      <main style={{ flex: 1, display: "flex", flexDirection: "column", height: "100vh" }}>
        <Transcript items={transcript} />
        <Composer
          onSend={handleSend}
          onCancel={() => setActiveRun(null)}
          isRunning={activeRun !== null}
        />
      </main>
      <RemoteUiRenderer documents={documents} />
    </div>
  );
};
