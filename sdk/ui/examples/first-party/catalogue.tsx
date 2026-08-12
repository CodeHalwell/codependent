/** @jsxImportSource react */
import type { ReactNode } from "react";
import { Stack } from "../../src/react/primitives.js";
import {
  ApplicationShell,
  ConversationComposer,
  ConversationTranscript,
  KanbanBoard,
  RunProgress,
  type BoardCard,
  type BoardColumn,
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

const boardColumns: readonly BoardColumn[] = [
  { id: "backlog", label: "Backlog", emptyMessage: "Nothing queued" },
  { id: "in-progress", label: "In progress", limit: 2 },
  { id: "review", label: "Review" },
  { id: "done", label: "Done" },
];

const boardCards: readonly BoardCard[] = [
  { id: "card-1", title: "Draw the workflow DAG", columnId: "in-progress", status: "running", assignee: "codypendent", kind: "task" },
  { id: "card-2", title: "Blackboard projection", columnId: "review", status: "waiting", assignee: "daniel", kind: "task", summary: "Read-only board projection for UI producers." },
  { id: "card-3", title: "Rate limit alignment", columnId: "done", status: "completed", kind: "task" },
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
        <KanbanBoard
          id="board"
          title="Backlog"
          description="Move cards with the keyboard; every move is a mediated intent."
          columns={boardColumns}
          cards={boardCards}
          selectedCardId="card-2"
          selectCardAction="board.card.select"
          moveCardAction="board.card.move"
          cardIntents={[{ action: "board.card.assign", label: "Assign to me" }]}
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
