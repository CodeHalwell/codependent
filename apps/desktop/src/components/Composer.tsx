import React, { useEffect, useRef, useState, KeyboardEvent } from "react";

interface ComposerProps {
  onSend: (text: string) => void;
  /**
   * What the status strip says while idle-and-connected — the STAGED run
   * defaults ("Plan mode · openai/gpt-5.4"), read from the shell. Absent, the
   * strip falls back to the old constant; it must never invent a mode the
   * shell did not report.
   */
  statusLabel?: string | null;
  /**
   * Lifted draft state. The composer unmounts on every trip to another view
   * (`App` conditionally mounts the session surface), so component-local
   * state lost the half-typed objective. When the caller supplies both, the
   * draft lives with it and survives the round trip.
   */
  draft?: string;
  onDraftChange?: (draft: string) => void;
  /**
   * Queue the text as a follow-up prompt instead of starting a run
   * (`QueuePrompt`, adoption 06).
   *
   * This is what the composer does while a run is LIVE. It used to do nothing
   * at all — the textarea was disabled outright, so there was nowhere to put
   * the next instruction and the only way to say anything was to cancel the
   * run. The TUI has never worked that way: submitting during an active run
   * pushes `Intent::QueuePrompt { delivery: Queue }`
   * (`crates/tui/src/reduce.rs`), and this is that behaviour, ported.
   */
  onQueue?: (text: string) => void;
  /**
   * Whether a real `QueuePrompt` can be sent — connected, with a session the
   * shell is attached to and a bridge that offers the command. False keeps the
   * old behaviour (the textarea is disabled while a run is live), because a
   * composer that accepts text it cannot send anywhere is worse than one that
   * says it is closed.
   */
  canQueue?: boolean;
  /** How many prompts the daemon currently reports queued, for the toggle. */
  queuedCount?: number;
  /** Whether the queue panel is open (rendered by the caller). */
  queueOpen?: boolean;
  /** Open or close the queue panel. */
  onToggleQueue?: () => void;
  /**
   * REQUEST a cancellation — it opens the confirmation, it does not send
   * `CancelRun`. The command is only sent once the operator confirms, in
   * `ConfirmCancel.tsx`; this button used to fire it on the click, and a
   * misclick cost the whole run.
   */
  onRequestCancel?: () => void;
  isRunning: boolean;
  disabled?: boolean;
  /**
   * Whether a real `CancelRun` can be sent — i.e. the client is connected and
   * knows the run's id. When it cannot, the button is not offered at all.
   */
  canCancel?: boolean;
  /**
   * Whether a real `QueueSteering` can be sent — connected, with a live run
   * id. When it cannot, the Steer button is not offered at all, because there
   * is nothing it could truthfully do.
   */
  canSteer?: boolean;
  /** Whether the steering panel is currently open (rendered by the caller). */
  steeringOpen?: boolean;
  /** Open or close the steering panel. */
  onToggleSteering?: () => void;
  /**
   * Which run-lifecycle control the DAEMON would accept for the live run right
   * now, or `null` for neither.
   *
   * Supplied by `runLifecycleAffordance`, which transcribes
   * `validate_run_transition` (`crates/daemon/src/commands.rs`) — pause from
   * any live not-already-paused state, resume ONLY from `Paused`. `null` also
   * covers "this client does not know the run's state", which is a real case
   * after a compact catch-up: neither button is drawn, because offering one
   * would be a claim about a state nobody told us.
   */
  lifecycle?: "pause" | "resume" | null;
  /** Send a real `PauseRun`. */
  onPause?: () => void;
  /** Send a real `ResumeRun`. */
  onResume?: () => void;
}

export const Composer: React.FC<ComposerProps> = ({
  onSend,
  statusLabel,
  draft,
  onDraftChange,
  onQueue,
  canQueue,
  queuedCount,
  queueOpen,
  onToggleQueue,
  onRequestCancel,
  isRunning,
  disabled,
  canCancel,
  canSteer,
  steeringOpen,
  onToggleSteering,
  lifecycle,
  onPause,
  onResume,
}) => {
  const [localInput, setLocalInput] = useState("");
  // Controlled when the caller lifts the draft; local otherwise (tests and
  // older callers keep working unchanged).
  const input = draft !== undefined ? draft : localInput;
  const setInput = (next: string) => {
    if (onDraftChange) {
      onDraftChange(next);
    }
    if (draft === undefined) {
      setLocalInput(next);
    }
  };

  /**
   * Auto-grow to the text, up to ~10 rows: a pasted stack trace used to
   * scroll inside a fixed 3-row box while the transcript above had the
   * space. Height is derived from `scrollHeight` after each change.
   */
  const areaRef = useRef<HTMLTextAreaElement | null>(null);
  useEffect(() => {
    const area = areaRef.current;
    if (!area) {
      return;
    }
    area.style.height = "auto";
    const line = 21; // ~14px font at 1.5 line height
    const max = line * 10 + 24;
    area.style.height = `${Math.min(area.scrollHeight, max)}px`;
  }, [input]);

  /**
   * While a run is live the composer QUEUES rather than starts a run — but only
   * when a real `QueuePrompt` can be sent. Without that it stays closed, as it
   * always was.
   */
  const queueing = Boolean(isRunning && canQueue && onQueue);
  /** Whether the textarea accepts text at all right now. */
  const open = !disabled && (!isRunning || queueing);

  const submit = () => {
    const text = input.trim();
    if (!text || !open) {
      return;
    }
    if (queueing) {
      onQueue?.(text);
    } else {
      onSend(text);
    }
    // The text is cleared because the command went out, not because anything
    // was confirmed: a refusal is reported by the caller, in the error banner
    // and in the queue panel.
    setInput("");
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submit();
    }
  };

  const handleSend = () => submit();

  return (
    <div
      style={{
        padding: "16px 24px 20px",
        background: "#16191f",
        borderTop: "1px solid #282e39",
      }}
    >
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          background: "#0d1117",
          border: "1px solid #30363d",
          borderRadius: 8,
          overflow: "hidden",
        }}
      >
        <textarea
          ref={areaRef}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={
            disabled
              ? "Not connected to codypendentd — runs cannot be submitted."
              : queueing
                ? "Queue a follow-up for this session (Enter to queue, Shift+Enter for newline)..."
                : isRunning
                  ? "Run in progress..."
                  : "Ask Codypendent or describe a task (Enter to send, Shift+Enter for newline)..."
          }
          disabled={!open}
          rows={3}
          style={{
            width: "100%",
            boxSizing: "border-box",
            background: "transparent",
            border: "none",
            outline: "none",
            color: "#e6edf3",
            padding: "12px 16px",
            fontSize: 14,
            resize: "none",
            fontFamily: "inherit",
          }}
        />
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            padding: "8px 12px",
            background: "#161b22",
            borderTop: "1px solid #21262d",
          }}
        >
          <span style={{ fontSize: 12, color: "#8b949e" }}>
            {disabled
              ? "Not connected"
              : queueing
                ? "Queueing for this session"
                : (statusLabel ?? "Build Mode")}
          </span>
          <div style={{ display: "flex", gap: 8 }}>
            {/*
              Pause and resume sit beside Cancel because they answer the same
              question — "stop this" — without the cost. Which of the two is
              drawn (if either) is the DAEMON's answer, folded from
              `RunStateChanged`: `validate_run_transition` admits resume only
              from `Paused`, so a run that cannot take it is never offered it,
              and a run whose state this client does not know is offered
              neither.
            */}
            {lifecycle === "pause" && onPause && (
              <button
                onClick={onPause}
                data-testid="composer-pause"
                style={{
                  background: "#21262d",
                  border: "1px solid #30363d",
                  color: "#e6edf3",
                  padding: "6px 14px",
                  borderRadius: 6,
                  fontSize: 12,
                  cursor: "pointer",
                  fontWeight: 600,
                }}
              >
                Pause Run
              </button>
            )}
            {lifecycle === "resume" && onResume && (
              <button
                onClick={onResume}
                data-testid="composer-resume"
                style={{
                  background: "#238636",
                  border: "none",
                  color: "#fff",
                  padding: "6px 14px",
                  borderRadius: 6,
                  fontSize: 12,
                  cursor: "pointer",
                  fontWeight: 600,
                }}
              >
                Resume Run
              </button>
            )}
            {onToggleQueue && (
              <button
                onClick={onToggleQueue}
                aria-expanded={Boolean(queueOpen)}
                data-testid="composer-queue"
                style={{
                  background: queueOpen ? "#1f6feb" : "#21262d",
                  border: `1px solid ${queueOpen ? "#1f6feb" : "#30363d"}`,
                  color: queueOpen ? "#fff" : "#e6edf3",
                  padding: "6px 14px",
                  borderRadius: 6,
                  fontSize: 12,
                  cursor: "pointer",
                  fontWeight: 600,
                }}
              >
                Queue ({queuedCount ?? 0})
              </button>
            )}
            {/*
              Steering lives here rather than behind a nav entry: it is what an
              operator reaches for to change a live agent's course, so it
              belongs next to the box they are already typing in.
            */}
            {isRunning && canSteer && onToggleSteering && (
              <button
                onClick={onToggleSteering}
                aria-expanded={Boolean(steeringOpen)}
                data-testid="composer-steer"
                style={{
                  background: steeringOpen ? "#1f6feb" : "#21262d",
                  border: `1px solid ${steeringOpen ? "#1f6feb" : "#30363d"}`,
                  color: steeringOpen ? "#fff" : "#e6edf3",
                  padding: "6px 14px",
                  borderRadius: 6,
                  fontSize: 12,
                  cursor: "pointer",
                  fontWeight: 600,
                }}
              >
                {steeringOpen ? "Hide steering" : "Steer Run"}
              </button>
            )}
            {isRunning && canCancel && onRequestCancel && (
              <button
                onClick={onRequestCancel}
                style={{
                  background: "#da3633",
                  border: "none",
                  color: "#fff",
                  padding: "6px 14px",
                  borderRadius: 6,
                  fontSize: 12,
                  cursor: "pointer",
                  fontWeight: 600,
                }}
              >
                Cancel Run
              </button>
            )}
            <button
              onClick={handleSend}
              disabled={!input.trim() || !open}
              style={{
                background: input.trim() && open ? "#238636" : "#21262d",
                color: input.trim() && open ? "#fff" : "#484f58",
                border: "none",
                padding: "6px 14px",
                borderRadius: 6,
                fontSize: 12,
                cursor: input.trim() && open ? "pointer" : "default",
                fontWeight: 600,
              }}
            >
              {queueing ? "Queue" : "Send"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
};
