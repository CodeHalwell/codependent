/**
 * The pending-prompt queue: stack up follow-up work while a run is live.
 *
 * The daemon has had this since adoption 06 — `QueuePrompt`,
 * `UpdateQueuedPrompt`, `PromoteQueuedPrompt`, `DeleteQueuedPrompt`, and a
 * `PendingPromptsChanged` event carrying the whole queue after every mutation.
 * The TUI drives all four (`crates/tui/src/reduce.rs`). The desktop drove none
 * of them, and disabled its composer outright while a run was live, so there
 * was nowhere at all to put the next instruction.
 *
 * Three rules this panel is built around:
 *
 *   1. The queue drawn here is the DAEMON's. Every row comes from a
 *      `PendingPromptsChanged` event (or a compact catch-up's
 *      `projection.pending_prompts`); nothing is appended locally on the way to
 *      the daemon and nothing is removed locally on the way back. A mutation
 *      this panel sends changes the list only once the daemon's own event says
 *      it did.
 *   2. A FAILED mutation and an EMPTY queue are different facts and are drawn
 *      differently — a red alert naming the daemon's refusal, versus a plain
 *      "nothing is queued". Collapsing the two would report a refusal as a
 *      successful emptying.
 *   3. Delivery is shown as the daemon reports it, not as a category this
 *      panel invents: `Steer` feeds the live run at its next safe point,
 *      `Queue` waits for the session to go idle, and `Unknown` is a newer
 *      daemon's value this build cannot name — labelled as such rather than
 *      bucketed into one of the two it knows.
 */
import React, { useState } from "react";

import type { PendingPromptView } from "@codypendent/protocol";
import { surfaceButton } from "./surfaceChrome.js";

export interface PromptQueueProps {
  /** The queue exactly as the daemon last reported it. */
  prompts: readonly PendingPromptView[];
  /**
   * Whether the queue can be MUTATED at all — connected, with a session
   * attached. False leaves the rows readable and every control disabled: what
   * the daemon last said is still true, it just cannot be changed right now.
   */
  canMutate: boolean;
  /** Why the queue cannot be mutated, when `canMutate` is false. */
  unavailableDetail?: string | null;
  /**
   * Why the last mutation FAILED, if it did. Rendered as an alert, never as an
   * empty queue.
   */
  error?: string | null;
  /** Promote to `Steer` and move to the front (`PromoteQueuedPrompt`). */
  onPromote: (promptId: string) => void;
  /** Save edited text (`UpdateQueuedPrompt`). Resolves true when accepted. */
  onEdit: (promptId: string, text: string) => Promise<boolean>;
  /** Remove without running (`DeleteQueuedPrompt`). */
  onDelete: (promptId: string) => void;
  /** Close the panel. */
  onClose?: () => void;
}

/** How the daemon's `PromptDelivery` reads to an operator. */
function deliveryLabel(delivery: PendingPromptView["delivery"]): string {
  switch (delivery.type) {
    case "Steer":
      return "steer";
    case "Queue":
      return "queue";
    default:
      // A newer daemon's variant. Naming it "queue" would be a guess about
      // when the prompt runs, which is the one thing delivery decides.
      return "unrecognised delivery";
  }
}

export const PromptQueue: React.FC<PromptQueueProps> = ({
  prompts,
  canMutate,
  unavailableDetail,
  error,
  onPromote,
  onEdit,
  onDelete,
  onClose,
}) => {
  /** The row being edited, and its working text. One at a time. */
  const [editing, setEditing] = useState<{ id: string; text: string } | null>(null);

  const save = async () => {
    if (!editing) {
      return;
    }
    const accepted = await onEdit(editing.id, editing.text);
    if (accepted) {
      // The edit box closes only when the daemon took the change. A refusal
      // leaves the text in place so it can be corrected rather than retyped.
      setEditing(null);
    }
  };

  return (
    <section
      data-testid="prompt-queue"
      aria-label="Pending prompt queue"
      style={{
        padding: "12px 24px",
        background: "var(--cody-panel)",
        borderTop: "1px solid var(--cody-border)",
        display: "flex",
        flexDirection: "column",
        gap: 10,
      }}
    >
      <header
        style={{ display: "flex", alignItems: "baseline", justifyContent: "space-between", gap: 12 }}
      >
        <div>
          <div style={{ fontSize: 13, fontWeight: 600, color: "var(--cody-text)" }}>
            Queued prompts ({prompts.length})
          </div>
          <div style={{ fontSize: 11, color: "var(--cody-text-muted)", marginTop: 2 }}>
            Follow-up work the daemon is holding for this session. The daemon decides when each
            one runs.
          </div>
        </div>
        {onClose && (
          <button onClick={onClose} style={surfaceButton()} data-testid="prompt-queue-close">
            Close
          </button>
        )}
      </header>

      {error && (
        <div
          role="alert"
          data-testid="prompt-queue-error"
          style={{
            border: "1px solid var(--cody-danger)",
            background: "var(--cody-danger-bg)",
            color: "var(--cody-danger-text)",
            borderRadius: 6,
            padding: "8px 10px",
            fontSize: 12,
          }}
        >
          <strong>The queue command failed.</strong> {error} The queue below is still whatever the
          daemon last reported.
        </div>
      )}

      {!canMutate && (
        <div
          role="status"
          data-testid="prompt-queue-unavailable"
          style={{
            border: "1px solid var(--cody-warning-border)",
            background: "var(--cody-warning-bg)",
            color: "var(--cody-warning-text)",
            borderRadius: 6,
            padding: "8px 10px",
            fontSize: 12,
          }}
        >
          <strong>The queue cannot be changed.</strong>{" "}
          {unavailableDetail ?? "No connected session to queue prompts on."}
        </div>
      )}

      {prompts.length === 0 ? (
        <div style={{ fontSize: 12, color: "var(--cody-text-faint)" }} data-testid="prompt-queue-empty">
          No prompt is queued. The daemon has reported no <code>PendingPromptsChanged</code> entries
          for this session.
        </div>
      ) : (
        <ol
          style={{
            listStyle: "none",
            margin: 0,
            padding: 0,
            display: "flex",
            flexDirection: "column",
            gap: 6,
          }}
        >
          {prompts.map((prompt, index) => {
            const isEditing = editing?.id === prompt.id;
            return (
              <li
                key={prompt.id}
                data-testid="prompt-queue-row"
                data-prompt-id={prompt.id}
                data-delivery={prompt.delivery.type}
                style={{
                  border: "1px solid var(--cody-border-strong)",
                  background: "var(--cody-canvas)",
                  borderRadius: 6,
                  padding: "8px 10px",
                  display: "flex",
                  flexDirection: "column",
                  gap: 6,
                }}
              >
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <span style={{ fontSize: 11, color: "var(--cody-text-faint)" }}>#{index + 1}</span>
                  <span
                    style={{
                      fontSize: 10,
                      fontWeight: 700,
                      letterSpacing: 0.4,
                      textTransform: "uppercase",
                      borderRadius: 4,
                      padding: "1px 6px",
                      border: `1px solid ${prompt.delivery.type === "Steer" ? "var(--cody-accent-strong)" : "var(--cody-border-strong)"}`,
                      color: prompt.delivery.type === "Steer" ? "var(--cody-link-soft)" : "var(--cody-text-muted)",
                    }}
                  >
                    {deliveryLabel(prompt.delivery)}
                  </span>
                  <span style={{ fontSize: 11, color: "var(--cody-text-muted)" }}>{prompt.mode.type} mode</span>
                </div>

                {isEditing ? (
                  <textarea
                    value={editing.text}
                    onChange={(e) => setEditing({ id: prompt.id, text: e.target.value })}
                    rows={2}
                    aria-label="Queued prompt text"
                    data-testid="prompt-queue-edit"
                    style={{
                      width: "100%",
                      boxSizing: "border-box",
                      background: "var(--cody-canvas)",
                      border: "1px solid var(--cody-border-strong)",
                      borderRadius: 6,
                      outline: "none",
                      color: "var(--cody-text)",
                      padding: "6px 8px",
                      fontSize: 13,
                      resize: "none",
                      fontFamily: "inherit",
                    }}
                  />
                ) : (
                  <div style={{ fontSize: 13, color: "var(--cody-text-secondary)", whiteSpace: "pre-wrap" }}>
                    {prompt.text}
                  </div>
                )}

                <div style={{ display: "flex", gap: 6, justifyContent: "flex-end" }}>
                  {isEditing ? (
                    <>
                      <button
                        onClick={() => setEditing(null)}
                        style={surfaceButton()}
                        data-testid="prompt-queue-cancel-edit"
                      >
                        Cancel
                      </button>
                      <button
                        onClick={() => void save()}
                        disabled={!canMutate || !editing.text.trim()}
                        style={surfaceButton(
                          canMutate && editing.text.trim() ? "primary" : "neutral",
                        )}
                        data-testid="prompt-queue-save"
                      >
                        Save
                      </button>
                    </>
                  ) : (
                    <>
                      <button
                        onClick={() => setEditing({ id: prompt.id, text: prompt.text })}
                        disabled={!canMutate}
                        style={surfaceButton()}
                        data-testid="prompt-queue-start-edit"
                      >
                        Edit
                      </button>
                      <button
                        onClick={() => onPromote(prompt.id)}
                        disabled={!canMutate}
                        style={surfaceButton(canMutate ? "primary" : "neutral")}
                        data-testid="prompt-queue-promote"
                        title="Deliver this one by steering the live run at its next safe point"
                      >
                        Send next
                      </button>
                      <button
                        onClick={() => onDelete(prompt.id)}
                        disabled={!canMutate}
                        style={surfaceButton(canMutate ? "danger" : "neutral")}
                        data-testid="prompt-queue-delete"
                      >
                        Remove
                      </button>
                    </>
                  )}
                </div>
              </li>
            );
          })}
        </ol>
      )}
    </section>
  );
};
