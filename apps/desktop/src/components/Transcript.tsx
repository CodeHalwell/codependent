import React, { useCallback, useEffect, useRef } from "react";
import type { ConnectionStatus, RunActivity, TranscriptItem } from "../types.js";
import { renderMarkdown } from "../markdown.js";
import { describeActivity } from "../runActivity.js";
import type { DesktopView } from "./Navigation.js";
import type { QuestionOutcomeView } from "../types.js";
import { QuestionCard } from "./QuestionCard.js";

interface TranscriptProps {
  items: TranscriptItem[];
  connectionStatus: ConnectionStatus;
  /** Why the client is in this connection state — shown verbatim. */
  statusDetail?: string;
  onApprove?: (approvalId: string) => void;
  onReject?: (approvalId: string) => void;
  /**
   * What the live run is doing, for the working row under the last item.
   * Absent (or `idle`) draws nothing; `streaming` draws nothing either, because
   * the growing reply is its own signal.
   */
  activity?: RunActivity;
  /**
   * Start a new run with the same objective, from a failure card. Absent when
   * nothing can be submitted (not connected), in which case the card offers
   * no Retry rather than a button that fails.
   */
  onRetry?: (objective: string) => void;
  /** Open a configuration surface a failure card points at. */
  onOpenView?: (view: DesktopView) => void;
  /** Answer or reject a parked question. Absent when nothing can be sent. */
  onResolveQuestion?: (questionId: string, outcome: QuestionOutcomeView) => void;
}

const EMPTY_HEADING: Record<ConnectionStatus, string> = {
  disconnected: "Not connected to codypendentd",
  connecting: "Connecting to codypendentd",
  connected: "Ready",
};

/**
 * How close to the bottom still counts as following the stream, in pixels.
 * Further up than this and the reader has deliberately scrolled back, so the
 * view stops chasing new output and leaves them where they are.
 */
const PINNED_SLACK_PX = 80;

// Styles are module constants rather than object literals in the render body.
// A fresh style object per row per frame is allocation the streaming path would
// otherwise repeat for every item in the transcript, on every token.
const LIST: React.CSSProperties = {
  flex: 1,
  overflowY: "auto",
  padding: "24px 32px",
  display: "flex",
  flexDirection: "column",
  gap: 16,
};
const EMPTY_WRAP: React.CSSProperties = { margin: "auto", textAlign: "center", color: "var(--cody-text-faint)" };
const EMPTY_TITLE: React.CSSProperties = { margin: "0 0 8px 0", color: "var(--cody-text-secondary)" };
const EMPTY_BODY: React.CSSProperties = { margin: 0, fontSize: 14, maxWidth: 520 };
const ROW_USER: React.CSSProperties = {
  alignSelf: "flex-end",
  maxWidth: "75%",
  background: "var(--cody-panel-hover)",
  border: "1px solid var(--cody-accent)",
  padding: "12px 16px",
  borderRadius: "12px 12px 2px 12px",
  fontSize: 14,
  lineHeight: 1.5,
};
const ROW_ASSISTANT: React.CSSProperties = {
  alignSelf: "flex-start",
  maxWidth: "85%",
  background: "var(--cody-panel)",
  border: "1px solid var(--cody-border)",
  padding: "16px 20px",
  borderRadius: "12px 12px 12px 2px",
  fontSize: 14,
  lineHeight: 1.6,
  color: "var(--cody-text)",
  // No `whiteSpace: pre-wrap` here: the Markdown renderer sets it per
  // paragraph, and setting it on the row as well collapses its block spacing
  // into the raw newlines it was meant to replace.
  overflowWrap: "anywhere",
};
const ROW_THOUGHT: React.CSSProperties = {
  alignSelf: "flex-start",
  maxWidth: "85%",
  background: "var(--cody-bg)",
  border: "1px solid var(--cody-inset)",
  padding: "8px 12px",
  borderRadius: 6,
  fontSize: 12,
  color: "var(--cody-text-muted)",
};
const THOUGHT_SUMMARY: React.CSSProperties = { cursor: "pointer", fontWeight: 500 };
const THOUGHT_BODY: React.CSSProperties = { marginTop: 8, whiteSpace: "pre-wrap" };
const ROW_TOOL: React.CSSProperties = {
  alignSelf: "flex-start",
  width: "85%",
  background: "var(--cody-canvas)",
  border: "1px solid var(--cody-border-strong)",
  borderRadius: 8,
  overflow: "hidden",
};
const TOOL_HEAD: React.CSSProperties = {
  background: "var(--cody-panel-raised)",
  padding: "8px 12px",
  fontSize: 12,
  fontWeight: 600,
  color: "var(--cody-link)",
  display: "flex",
  justifyContent: "space-between",
  borderBottom: "1px solid var(--cody-border-strong)",
};
const TOOL_BODY: React.CSSProperties = { padding: "8px 12px", fontSize: 13, color: "var(--cody-text)" };
const TOOL_ARGS: React.CSSProperties = {
  margin: 0,
  padding: 12,
  fontSize: 12,
  overflowX: "auto",
  color: "var(--cody-text-muted)",
};
const ROW_QUESTION: React.CSSProperties = {
  alignSelf: "flex-start",
  width: "85%",
  background: "var(--cody-panel-hover)",
  border: "1px solid var(--cody-accent)",
  borderRadius: 8,
  padding: 16,
};
const QUESTION_TITLE: React.CSSProperties = {
  fontWeight: 600,
  color: "var(--cody-link)",
  marginBottom: 8,
  fontSize: 14,
};
const QUESTION_BODY: React.CSSProperties = { fontSize: 13, color: "var(--cody-text)" };
const ROW_APPROVAL: React.CSSProperties = {
  alignSelf: "flex-start",
  width: "85%",
  background: "var(--cody-warning-bg)",
  border: "1px solid var(--cody-warning-border)",
  borderRadius: 8,
  padding: 16,
};
const APPROVAL_TITLE: React.CSSProperties = {
  fontWeight: 600,
  color: "var(--cody-warning)",
  marginBottom: 8,
  fontSize: 14,
};
const APPROVAL_BODY: React.CSSProperties = { fontSize: 13, color: "var(--cody-text)", marginBottom: 12 };
const APPROVAL_ACTIONS: React.CSSProperties = { display: "flex", gap: 8 };
const APPROVE_BUTTON: React.CSSProperties = {
  background: "var(--cody-success-strong)",
  border: "none",
  color: "var(--cody-on-accent)",
  padding: "6px 12px",
  borderRadius: 6,
  fontSize: 12,
  cursor: "pointer",
  fontWeight: 600,
};
const REJECT_BUTTON: React.CSSProperties = {
  background: "var(--cody-inset)",
  border: "1px solid var(--cody-border-strong)",
  color: "var(--cody-text-secondary)",
  padding: "6px 12px",
  borderRadius: 6,
  fontSize: 12,
  cursor: "pointer",
};
const SYSTEM_ROW: React.CSSProperties = { fontSize: 12, color: "var(--cody-text-muted)", textAlign: "center" };
const SYSTEM_ROW_INFO: React.CSSProperties = { ...SYSTEM_ROW, color: "var(--cody-link-soft)" };
const SYSTEM_ROW_WARNING: React.CSSProperties = {
  ...SYSTEM_ROW,
  color: "var(--cody-warning-text)",
  alignSelf: "stretch",
  textAlign: "left",
  border: "1px solid var(--cody-warning-border)",
  background: "var(--cody-warning-bg)",
  borderRadius: 6,
  padding: "6px 10px",
};
const ROW_FAILURE: React.CSSProperties = {
  alignSelf: "stretch",
  background: "var(--cody-danger-bg)",
  border: "1px solid var(--cody-danger)",
  borderRadius: 8,
  padding: "12px 16px",
};
const FAILURE_TITLE: React.CSSProperties = {
  fontWeight: 600,
  color: "var(--cody-danger-text)",
  fontSize: 14,
  marginBottom: 4,
  overflowWrap: "anywhere",
};
const FAILURE_HINT: React.CSSProperties = { fontSize: 13, color: "var(--cody-text)", marginBottom: 10 };
const FAILURE_ACTIONS: React.CSSProperties = { display: "flex", gap: 8, flexWrap: "wrap" };
const FAILURE_DETAIL: React.CSSProperties = {
  marginTop: 10,
  fontSize: 12,
  color: "var(--cody-text-secondary)",
};
const FAILURE_DETAIL_BODY: React.CSSProperties = {
  marginTop: 6,
  whiteSpace: "pre-wrap",
  fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
  fontSize: 11,
  overflowWrap: "anywhere",
  maxHeight: 320,
  overflowY: "auto",
};
const FAILURE_BUTTON: React.CSSProperties = {
  background: "var(--cody-inset)",
  border: "1px solid var(--cody-border-strong)",
  color: "var(--cody-text)",
  padding: "6px 12px",
  borderRadius: 6,
  fontSize: 12,
  cursor: "pointer",
  fontWeight: 600,
};
const FAILURE_PRIMARY: React.CSSProperties = {
  ...FAILURE_BUTTON,
  background: "var(--cody-success-strong)",
  border: "none",
  color: "var(--cody-on-accent)",
};
const WORKING_ROW: React.CSSProperties = {
  alignSelf: "flex-start",
  display: "flex",
  alignItems: "center",
  gap: 8,
  fontSize: 12,
  color: "var(--cody-text-muted)",
  padding: "2px 4px",
};
const WORKING_DOT: React.CSSProperties = {
  width: 8,
  height: 8,
  borderRadius: "50%",
  background: "var(--cody-warning)",
  animation: "cody-pulse 1.2s ease-in-out infinite",
};
/** Mirrors the TUI's `NOTE_INLINE_LINE_THRESHOLD`: past this, a note folds. */
const SYSTEM_INLINE_LINE_LIMIT = 2;
/** A single line this long is a wall too, however few newlines it contains. */
const SYSTEM_INLINE_CHARS = 160;
const SYSTEM_FOLD: React.CSSProperties = {
  alignSelf: "stretch",
  fontSize: 12,
  color: "var(--cody-text-muted)",
  border: "1px dashed var(--cody-inset)",
  borderRadius: 6,
  padding: "6px 10px",
};
const SYSTEM_FOLD_SUMMARY: React.CSSProperties = { cursor: "pointer", userSelect: "none" };
const SYSTEM_FOLD_BODY: React.CSSProperties = {
  marginTop: 8,
  whiteSpace: "pre-wrap",
  fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
  fontSize: 11,
  overflowWrap: "anywhere",
};
const ROW_BACKSTAGE: React.CSSProperties = {
  alignSelf: "stretch",
  background: "transparent",
  border: "1px dashed var(--cody-inset)",
  borderRadius: 6,
  padding: "6px 10px",
  fontSize: 11,
  color: "var(--cody-text-faint)",
};
const BACKSTAGE_SUMMARY: React.CSSProperties = { cursor: "pointer", userSelect: "none" };
const BACKSTAGE_BODY: React.CSSProperties = {
  marginTop: 8,
  whiteSpace: "pre-wrap",
  fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
  fontSize: 11,
  maxHeight: 320,
  overflowY: "auto",
};

interface RowProps {
  item: TranscriptItem;
  onApprove?: (approvalId: string) => void;
  onReject?: (approvalId: string) => void;
  onRetry?: (objective: string) => void;
  onOpenView?: (view: DesktopView) => void;
  onResolveQuestion?: (questionId: string, outcome: QuestionOutcomeView) => void;
}

/** The buttons a failure card offers, from the remedy the failure text gave. */
function failureActions(
  item: TranscriptItem,
  onRetry: RowProps["onRetry"],
  onOpenView: RowProps["onOpenView"],
): React.ReactNode[] {
  const actions: React.ReactNode[] = [];
  if (item.remedy === "keys" && onOpenView) {
    actions.push(
      <button key="keys" style={FAILURE_PRIMARY} onClick={() => onOpenView("keys")}>
        Open API keys
      </button>,
    );
  }
  if (item.remedy === "models" && onOpenView) {
    actions.push(
      <button key="models" style={FAILURE_PRIMARY} onClick={() => onOpenView("models")}>
        Choose a model
      </button>,
    );
  }
  if (item.objective && onRetry) {
    const objective = item.objective;
    actions.push(
      <button
        key="retry"
        style={item.remedy === "retry" || item.remedy === "none" ? FAILURE_PRIMARY : FAILURE_BUTTON}
        onClick={() => onRetry(objective)}
      >
        Retry
      </button>,
    );
  }
  if (item.remedy !== "models" && onOpenView) {
    actions.push(
      <button key="pick" style={FAILURE_BUTTON} onClick={() => onOpenView("models")}>
        Models
      </button>,
    );
  }
  if (item.remedy !== "keys" && onOpenView) {
    actions.push(
      <button key="keys-secondary" style={FAILURE_BUTTON} onClick={() => onOpenView("keys")}>
        API keys
      </button>,
    );
  }
  return actions;
}

/**
 * One transcript row, memoized.
 *
 * A streaming reply replaces ONLY the last item of the transcript array; every
 * earlier item keeps its object identity for the whole stream. Without this
 * boundary React reconciles the entire transcript once per token, which
 * measured at 4.29 ms per token against a 400-item history — linear in the
 * history, so the longer a session runs the further behind the stream the UI
 * falls.
 *
 * `onApprove`/`onReject` must be referentially stable at the call site or the
 * memo never hits and this is decoration.
 */
const TranscriptRow = React.memo(function TranscriptRow({
  item,
  onApprove,
  onReject,
  onRetry,
  onOpenView,
  onResolveQuestion,
}: RowProps) {
  switch (item.type) {
    case "failure":
      // The run failed. This is the card the operator has to find, so it is
      // loud, it says what to do next, and the full sanitised chain is one
      // click away rather than dumped inline. Mirrors the TUI's failure card
      // and its Alt-R / Alt-A / Alt-M chips (`crates/tui/src/render.rs`).
      return (
        <div style={ROW_FAILURE} role="alert" data-testid="run-failure">
          <div style={FAILURE_TITLE}>Run failed: {item.text}</div>
          {item.hint && <div style={FAILURE_HINT}>{item.hint}</div>}
          <div style={FAILURE_ACTIONS}>{failureActions(item, onRetry, onOpenView)}</div>
          {item.failureDetail && item.failureDetail !== item.text && (
            <details style={FAILURE_DETAIL}>
              <summary style={{ cursor: "pointer" }}>Full error</summary>
              <div style={FAILURE_DETAIL_BODY}>{item.failureDetail}</div>
            </details>
          )}
        </div>
      );

    case "user":
      return <div style={ROW_USER}>{item.text}</div>;

    case "assistant":
      // The model writes Markdown; showing it raw meant reading `##` and `**`
      // as literal characters. Rendered to React elements only — never HTML —
      // so model output cannot inject into the webview.
      return <div style={ROW_ASSISTANT}>{renderMarkdown(item.text)}</div>;

    case "thought":
      return (
        <details style={ROW_THOUGHT}>
          <summary style={THOUGHT_SUMMARY}>Thought Process</summary>
          <div style={THOUGHT_BODY}>{item.text}</div>
        </details>
      );

    case "tool_call":
      return (
        <div style={ROW_TOOL}>
          <div style={TOOL_HEAD}>
            <span>Tool: {item.toolName}</span>
            <span
              style={{
                // An error used to paint in the SUCCESS green (the ternary
                // only distinguished "running"), so a failed call read as a
                // finished one.
                color:
                  item.status === "running"
                    ? "var(--cody-warning)"
                    : item.status === "error"
                      ? "var(--cody-danger-soft)"
                      : "var(--cody-success)",
              }}
            >
              {item.status ?? "running"}
            </span>
          </div>
          {item.text ? <div style={TOOL_BODY}>{item.text}</div> : null}
          {item.toolArgs && <pre style={TOOL_ARGS}>{JSON.stringify(item.toolArgs, null, 2)}</pre>}
          {typeof item.toolResult === "string" && item.toolResult ? (
            <div style={{ ...TOOL_BODY, color: "var(--cody-danger-soft)" }}>{item.toolResult}</div>
          ) : null}
        </div>
      );

    case "question":
      // Answerable when the daemon named the question and sent its prompts;
      // an older store shape (text only) keeps the read-only card.
      if (item.questionId && item.questionPrompts && item.questionPrompts.length > 0) {
        return (
          <QuestionCard
            questionId={item.questionId}
            prompts={item.questionPrompts}
            onResolve={onResolveQuestion}
          />
        );
      }
      return (
        <div style={ROW_QUESTION}>
          <div style={QUESTION_TITLE}>Question</div>
          <div style={QUESTION_BODY}>{item.text}</div>
        </div>
      );

    case "approval":
      return (
        <div style={ROW_APPROVAL}>
          <div style={APPROVAL_TITLE}>Approval Required</div>
          <div style={APPROVAL_BODY}>{item.text}</div>
          {item.approvalId && (
            <div style={APPROVAL_ACTIONS}>
              <button onClick={() => onApprove?.(item.approvalId!)} style={APPROVE_BUTTON}>
                Approve
              </button>
              <button onClick={() => onReject?.(item.approvalId!)} style={REJECT_BUTTON}>
                Reject
              </button>
            </div>
          )}
        </div>
      );

    case "backstage": {
      // The context manifest and curated-memory writes: real, but not the
      // conversation. One dim expandable line per run, mirroring the TUI's
      // `TranscriptEntry::Backstage`. Printing the manifest inline buried the
      // answer under a screenful of tool descriptions.
      const contextLines = item.contextLines;
      const memoryUpdates = item.memoryUpdates ?? 0;
      const parts: string[] = [];
      if (contextLines !== undefined) {
        parts.push(`${contextLines} context line${contextLines === 1 ? "" : "s"}`);
      }
      if (memoryUpdates > 0) {
        parts.push(`${memoryUpdates} memory update${memoryUpdates === 1 ? "" : "s"}`);
      }
      return (
        <details style={ROW_BACKSTAGE}>
          <summary style={BACKSTAGE_SUMMARY}>
            Backstage — {parts.length > 0 ? parts.join(" · ") : "run context"}
          </summary>
          <div style={BACKSTAGE_BODY}>{(item.raw ?? []).join("\n\n")}</div>
        </details>
      );
    }

    default: {
      // A proposed patch renders as a reviewable card: the touched files and
      // the wire's bounded diff preview, folded — not just an artifact id.
      if (item.diffPreview || (item.patchFiles && item.patchFiles.length > 0)) {
        return (
          <details style={SYSTEM_FOLD}>
            <summary style={SYSTEM_FOLD_SUMMARY}>{item.text}</summary>
            <div style={SYSTEM_FOLD_BODY}>
              {item.patchFiles && item.patchFiles.length > 0 && (
                <div style={{ marginBottom: 6 }}>
                  {item.patchFiles.map((file) => (
                    <div key={file} style={{ fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace", fontSize: 12 }}>
                      {file}
                    </div>
                  ))}
                </div>
              )}
              {item.diffPreview && (
                <pre
                  style={{
                    margin: 0,
                    fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
                    fontSize: 12,
                    whiteSpace: "pre-wrap",
                    wordBreak: "break-word",
                    maxHeight: 320,
                    overflowY: "auto",
                  }}
                >
                  {item.diffPreview}
                </pre>
              )}
              {item.artifactId && (
                <div style={{ marginTop: 6, fontSize: 11, color: "var(--cody-text-faint)" }}>
                  full patch: artifact {item.artifactId}
                </div>
              )}
            </div>
          </details>
        );
      }
      // A long system note folds, exactly as the TUI folds one past
      // `NOTE_INLINE_LINE_THRESHOLD`. The daemon's worktree-retention note is
      // the case that prompted this: full paths plus the `git worktree remove`
      // and `git branch -D` commands, which is real recovery information and so
      // must not be discarded — but printed inline it buried the conversation
      // under a wall of plumbing. One line, with the rest a click away.
      const lines = item.text.split("\n");
      if (lines.length > SYSTEM_INLINE_LINE_LIMIT || item.text.length > SYSTEM_INLINE_CHARS) {
        const summary = lines.find((line) => line.trim() !== "") ?? item.text;
        return (
          <details style={SYSTEM_FOLD}>
            <summary style={SYSTEM_FOLD_SUMMARY}>
              {summary.length > SYSTEM_INLINE_CHARS
                ? `${summary.slice(0, SYSTEM_INLINE_CHARS)}…`
                : summary}
            </summary>
            <div style={SYSTEM_FOLD_BODY}>{item.text}</div>
          </details>
        );
      }
      return (
        <div
          style={
            item.tone === "warning"
              ? SYSTEM_ROW_WARNING
              : item.tone === "info"
                ? SYSTEM_ROW_INFO
                : SYSTEM_ROW
          }
          role={item.tone === "warning" ? "status" : undefined}
        >
          {item.text}
        </div>
      );
    }
  }
});

export const Transcript: React.FC<TranscriptProps> = ({
  items,
  connectionStatus,
  statusDetail,
  onApprove,
  onReject,
  activity,
  onRetry,
  onOpenView,
  onResolveQuestion,
}) => {
  const working = activity ? describeActivity(activity) : null;
  const listRef = useRef<HTMLDivElement>(null);
  /** Whether the reader is still following the bottom of the stream. */
  const pinned = useRef(true);
  /** In-flight rAF handle, so many tokens in one frame coalesce into one scroll. */
  const scrollFrame = useRef(0);

  const handleScroll = useCallback(() => {
    const el = listRef.current;
    if (!el) {
      return;
    }
    pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight <= PINNED_SLACK_PX;
  }, []);

  useEffect(() => {
    // Two things this must NOT do. It must not scroll when the reader has
    // scrolled up to read something — the previous version yanked them back to
    // the bottom on every token. And it must not use `scrollIntoView` with
    // smooth behaviour: `items` is a new array per token, so that started and
    // abandoned an animation for every token of every reply, forcing layout
    // each time and never once completing. Setting `scrollTop` is instant, and
    // the rAF gate means at most one per frame however fast the stream runs.
    if (!pinned.current || scrollFrame.current !== 0) {
      return;
    }
    scrollFrame.current = requestAnimationFrame(() => {
      scrollFrame.current = 0;
      const el = listRef.current;
      if (el) {
        el.scrollTop = el.scrollHeight;
      }
    });
  }, [items, working]);

  useEffect(
    () => () => {
      if (scrollFrame.current !== 0) {
        cancelAnimationFrame(scrollFrame.current);
      }
    },
    [],
  );

  return (
    <div ref={listRef} onScroll={handleScroll} style={LIST}>
      {items.length === 0 ? (
        <div style={EMPTY_WRAP}>
          <h3 style={EMPTY_TITLE}>{EMPTY_HEADING[connectionStatus]}</h3>
          <p style={EMPTY_BODY}>
            {connectionStatus === "connected"
              ? "Start a run with an objective below."
              : (statusDetail ?? "No daemon transport.")}
          </p>
        </div>
      ) : (
        items.map((item) => (
          <TranscriptRow
            key={item.id}
            item={item}
            onApprove={onApprove}
            onReject={onReject}
            onRetry={onRetry}
            onOpenView={onOpenView}
            onResolveQuestion={onResolveQuestion}
          />
        ))
      )}
      {working && (
        // The run is live and nothing is moving on screen: between
        // `RunStarted` and the first token of a cold provider that used to be
        // indistinguishable from a hang. The dot pulses (CSS, honours
        // `prefers-reduced-motion` in theme.css) and the text names the phase.
        <div style={WORKING_ROW} role="status" data-testid="run-working">
          <span aria-hidden="true" style={WORKING_DOT} />
          <span>{working}</span>
        </div>
      )}
    </div>
  );
};
