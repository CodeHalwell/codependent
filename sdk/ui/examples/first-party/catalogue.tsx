/** @jsxImportSource react */
import type { ReactNode } from "react";
import { Stack } from "../../src/react/primitives.js";
import {
  ApplicationShell,
  ConversationComposer,
  ConversationTranscript,
  RunProgress,
  type ConversationMessage,
} from "../../src/first-party/index.js";

const messages: readonly ConversationMessage[] = [
  {
    id: "message-1",
    role: "user",
    author: "Daniel",
    content: "Review the current workspace and propose the safest change.",
    format: "text",
    createdAt: "2026-08-11T10:00:00Z",
    status: "complete",
  },
  {
    id: "message-2",
    role: "assistant",
    author: "Codypendent",
    content: "I’m inspecting the affected modules now.",
    format: "markdown",
    createdAt: "2026-08-11T10:00:02Z",
    status: "streaming",
  },
];

export interface CatalogueExampleProps {
  draft: string;
  runStatus: "queued" | "running" | "completed" | "failed" | "cancelled" | "blocked";
}

/** Controlled example: all values enter as props and all changes are semantic intents. */
export function CatalogueExample({ draft, runStatus }: CatalogueExampleProps): ReactNode {
  return (
    <ApplicationShell
      id="app"
      title="Workspace"
      productName="Codypendent"
      workspaceLabel="semantic-ui"
      navigation={[
        { id: "conversation", label: "Conversation", destination: "/conversation", shortcut: "g c" },
        { id: "runs", label: "Runs", destination: "/runs", status: runStatus, shortcut: "g r" },
      ]}
      activeDestination="/conversation"
      navigateAction="shell.navigate"
      globalActions={[{ action: "palette.open", label: "Commands", shortcut: "Ctrl+K" }]}
      status={[{ id: "daemon", label: "Daemon", value: "Connected", status: "healthy" }]}
    >
      <Stack gap="sm">
        <ConversationTranscript
          id="transcript"
          title="Conversation"
          state={{ phase: "streaming", label: "Codypendent is responding" }}
          messages={messages}
          selectAction="conversation.message.select"
        />
        <RunProgress
          id="run"
          title="Active run"
          runId="run-42"
          status={runStatus}
          stages={[{ id: "inspect", label: "Inspect workspace", status: "running" }]}
          elapsed="12s"
          steerIntent={{ action: "run.steer", label: "Steer", shortcut: "Ctrl+Enter" }}
          cancelIntent={{ action: "run.cancel", label: "Cancel", tone: "critical" }}
        />
        <ConversationComposer
          id="composer"
          title="Message"
          draft={draft}
          attachments={[]}
          mode="steering"
          draftChangeAction="conversation.draft.change"
          attachIntent={{ action: "conversation.attachment.add", label: "Attach" }}
          removeAttachmentAction="conversation.attachment.remove"
          submitIntent={{ action: "conversation.send", label: "Send", shortcut: "Ctrl+Enter" }}
        />
      </Stack>
    </ApplicationShell>
  );
}
