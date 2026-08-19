/**
 * The confirmation the desktop was missing before a run is cancelled.
 *
 * `cancel_run` used to fire on the click. The TUI has never worked that way —
 * `Overlay::ConfirmCancel` stands between the keystroke and the command
 * (`request_cancel` / `confirm_cancel` in `crates/tui/src/reduce.rs`) — and
 * losing a long run to a stray click is exactly the kind of thing that stops
 * an operator trusting the tool.
 *
 * A bare "are you sure?" would not be worth the extra click, so this shows what
 * is actually at stake: which run, what it was asked to do, and how long it has
 * been at it. All three come from the run's own `RunStarted` event. When this
 * client never saw that event — it attached mid-run — they are shown as
 * unknown. Nothing is defaulted, and elapsed time is never rendered as zero for
 * a run whose start is unknown.
 *
 * The consequence quoted is the daemon's rule, not one invented here:
 * `validate_run_transition` in `crates/daemon/src/commands.rs` treats
 * `Cancelled` as terminal and admits `ResumeRun` only from `Paused`, so a
 * cancelled run cannot be resumed. If the daemon nevertheless refuses this
 * particular cancel, its own refusal text is what the operator sees, on the
 * client-error row above the composer.
 */
import React, { useEffect, useRef, useState } from "react";

import type { SessionEvent } from "@codypendent/protocol";

/** What a run's own `RunStarted` event says about it. Absent when unseen. */
export interface RunAtStake {
  objective: string | null;
  startedAt: string | null;
}

/**
 * Read the run's objective and start time out of the durable event stream.
 *
 * Only the `RunStarted` carrying this exact `run_id` counts: a session can
 * hold several runs, and attributing another run's objective to this one would
 * be worse than admitting the objective is unknown.
 */
export function runAtStake(
  events: readonly SessionEvent[],
  runId: string | null,
): RunAtStake {
  if (!runId) {
    return { objective: null, startedAt: null };
  }
  // `durableEvents` is always in non-decreasing sequence order (the reducer
  // keeps that invariant), so a backward scan finds the LATEST matching
  // RunStarted with no copy and no sort — the old `[...events].sort(...)`
  // cost O(n log n) every call, i.e. per streamed token.
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const event = events[index];
    const body = event.body;
    if (body.type === "RunStarted" && body.run_id === runId) {
      return { objective: body.objective || null, startedAt: event.occurred_at };
    }
  }
  return { objective: null, startedAt: null };
}

export interface ConfirmCancelProps {
  /** The run the daemon named. Cancellation targets this id and no other. */
  runId: string;
  /** The objective from its `RunStarted`; null when this client never saw it. */
  objective: string | null;
  /** Its start time from the same event; null when unseen. */
  startedAt: string | null;
  /** Send the real `CancelRun`. */
  onConfirm: () => void;
  /** Leave the run alone. */
  onDismiss: () => void;
}

export const ConfirmCancel: React.FC<ConfirmCancelProps> = ({
  runId,
  objective,
  startedAt,
  onConfirm,
  onDismiss,
}) => {
  const [now, setNow] = useState(() => Date.now());
  const dismissRef = useRef<HTMLButtonElement | null>(null);

  // Focus lands on "Keep running": the destructive button is never the one a
  // stray Enter presses.
  useEffect(() => {
    dismissRef.current?.focus();
  }, []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onDismiss();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onDismiss]);

  // Only ticks while there is a start time to count from; an unknown start
  // stays unknown rather than becoming a growing number counted from now.
  useEffect(() => {
    if (!startedAt) {
      return;
    }
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [startedAt]);

  const elapsed = describeElapsed(startedAt, now);

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(1, 4, 9, 0.72)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 40,
      }}
      onClick={onDismiss}
    >
      <div
        role="alertdialog"
        aria-modal="true"
        aria-label="Confirm run cancellation"
        data-testid="cancel-confirm"
        onClick={(event) => event.stopPropagation()}
        style={{
          width: "min(520px, 92vw)",
          background: "#161b22",
          border: "1px solid #da3633",
          borderRadius: 10,
          padding: 20,
          display: "flex",
          flexDirection: "column",
          gap: 14,
          boxShadow: "0 18px 48px rgba(0, 0, 0, 0.55)",
        }}
      >
        <div style={{ fontSize: 15, fontWeight: 700, color: "#e6edf3" }}>Cancel this run?</div>

        <dl style={{ margin: 0, display: "flex", flexDirection: "column", gap: 8 }}>
          <div>
            <dt style={labelStyle}>Objective</dt>
            <dd style={{ ...valueStyle, margin: 0 }} data-testid="cancel-confirm-objective">
              {objective ?? (
                <span style={unknownStyle}>
                  Unknown — this client did not observe this run&rsquo;s <code>RunStarted</code>.
                </span>
              )}
            </dd>
          </div>
          <div>
            <dt style={labelStyle}>Running for</dt>
            <dd style={{ ...valueStyle, margin: 0 }} data-testid="cancel-confirm-elapsed">
              {elapsed ?? (
                <span style={unknownStyle}>
                  Unknown — no start time for this run reached this client.
                </span>
              )}
            </dd>
          </div>
          <div>
            <dt style={labelStyle}>Run</dt>
            <dd
              style={{
                ...valueStyle,
                margin: 0,
                fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
                fontSize: 12,
              }}
            >
              {runId}
            </dd>
          </div>
        </dl>

        <p style={{ margin: 0, fontSize: 12, color: "#e3b341", lineHeight: 1.5 }}>
          codypendentd treats <code>Cancelled</code> as a terminal run state, and admits
          <code> ResumeRun</code> only from <code>Paused</code>. A cancelled run cannot be resumed —
          continuing the work means starting another run.
        </p>

        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8 }}>
          <button
            ref={dismissRef}
            onClick={onDismiss}
            data-testid="cancel-confirm-no"
            style={{
              background: "#21262d",
              color: "#e6edf3",
              border: "1px solid #30363d",
              borderRadius: 6,
              padding: "6px 14px",
              fontSize: 12,
              fontWeight: 600,
              cursor: "pointer",
            }}
          >
            Keep running
          </button>
          <button
            onClick={onConfirm}
            data-testid="cancel-confirm-yes"
            style={{
              background: "#da3633",
              color: "#fff",
              border: "none",
              borderRadius: 6,
              padding: "6px 14px",
              fontSize: 12,
              fontWeight: 600,
              cursor: "pointer",
            }}
          >
            Cancel run
          </button>
        </div>
      </div>
    </div>
  );
};

const labelStyle: React.CSSProperties = {
  fontSize: 11,
  textTransform: "uppercase",
  letterSpacing: 0.5,
  color: "#8b949e",
};

const valueStyle: React.CSSProperties = { fontSize: 13, color: "#e6edf3", lineHeight: 1.5 };

const unknownStyle: React.CSSProperties = { color: "#8b949e", fontStyle: "italic" };

/**
 * How long the run has been going, or `null` when that is not known.
 *
 * `null` is returned for an unparseable or absent start, and for a start in
 * the future (clock skew): an absent measurement stays absent rather than
 * being rendered as `0s`, which would read as a fact.
 */
export function describeElapsed(startedAt: string | null, now: number): string | null {
  if (!startedAt) {
    return null;
  }
  const started = Date.parse(startedAt);
  if (Number.isNaN(started)) {
    return null;
  }
  const seconds = Math.floor((now - started) / 1000);
  if (seconds < 0) {
    return null;
  }
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const rest = seconds % 60;
  if (hours > 0) {
    return `${hours}h ${minutes}m ${rest}s`;
  }
  if (minutes > 0) {
    return `${minutes}m ${rest}s`;
  }
  return `${rest}s`;
}
