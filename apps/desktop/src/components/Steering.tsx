/**
 * Steering: redirect a run that is already in flight, without killing it.
 *
 * The TUI has had this since `Overlay::Steering(String)` — `s` opens a prompt,
 * Enter queues the text against the live run (`crates/tui/src/reduce.rs`). The
 * desktop had no trace of it, so the only way to change a running agent's mind
 * was to cancel it and start again, losing the run.
 *
 * The one thing this panel must never do is flatten three different facts into
 * one. The daemon keeps them apart and so does this:
 *
 *   1. ACCEPTED — the `QueueSteering` command came back `CommandAccepted`.
 *      A client-side fact about a command, drawn as such and clearly marked
 *      as awaiting the daemon.
 *   2. QUEUED — a `SteeringQueued` event arrived on the session's durable
 *      stream. The daemon is holding the text for the run's next safe point.
 *   3. APPLIED — a `SteeringApplied` event arrived. The run actually took it.
 *
 * Only (2) and (3) are read off the event stream, and neither is ever inferred
 * from (1). "Queued" is not "applied": between them the agent is still doing
 * whatever it was doing before, and telling an operator otherwise is the same
 * class of lie as rendering a failed read as an empty one.
 */
import React, { useEffect, useMemo, useRef, useState } from "react";
import { clock } from "../time.js";

import type { SessionEvent } from "@codypendent/protocol";
import type { SteerOutcome } from "../useDaemon.js";
import { surfaceButton } from "./surfaceChrome.js";

/**
 * One steering turn as the daemon reported it.
 *
 * `queuedAt` is null only in the case the TUI also tolerates: a
 * `SteeringApplied` with no unapplied `SteeringQueued` before it (this client
 * attached mid-run and never saw the queue event). It is shown as unknown, not
 * back-filled with a guess.
 */
export interface SteeringMarker {
  key: string;
  queuedAt: string | null;
  appliedAt: string | null;
}

/**
 * Fold a run's steering markers out of its durable events.
 *
 * Mirrors the TUI's reducer exactly (`EventBody::SteeringApplied` in
 * `crates/tui/src/reduce.rs`): an applied event marks the most recent queued
 * marker that is still unapplied, and when there is none it stands alone as an
 * applied marker of unknown origin.
 */
export function steeringMarkers(
  events: readonly SessionEvent[],
  runId: string | null,
): SteeringMarker[] {
  if (!runId) {
    return [];
  }
  const markers: SteeringMarker[] = [];
  for (const event of [...events].sort((left, right) => left.sequence - right.sequence)) {
    const body = event.body;
    if (body.type === "SteeringQueued" && body.run_id === runId) {
      markers.push({
        key: `steering-${event.sequence}`,
        queuedAt: event.occurred_at,
        appliedAt: null,
      });
      continue;
    }
    if (body.type === "SteeringApplied" && body.run_id === runId) {
      const pending = [...markers].reverse().find((marker) => marker.appliedAt === null);
      if (pending) {
        pending.appliedAt = event.occurred_at;
      } else {
        markers.push({
          key: `steering-${event.sequence}`,
          queuedAt: null,
          appliedAt: event.occurred_at,
        });
      }
    }
  }
  return markers;
}

export interface SteeringProps {
  /** The live run the steering targets; null when the daemon named none. */
  runId: string | null;
  /**
   * The session's durable events — the ONLY source of queued/applied truth
   * here. Passed in rather than fetched so this panel and the transcript are
   * looking at the same authoritative stream.
   */
  events: readonly SessionEvent[];
  /** Send the text. Resolves to what actually happened; see `SteerOutcome`. */
  onSteer: (text: string) => Promise<SteerOutcome>;
  /**
   * Whether a real `QueueSteering` can be sent at all — connected, with a run
   * id in hand. False disables the composer and says why.
   */
  canSteer: boolean;
  /** Why steering is unavailable, when `canSteer` is false. Shown verbatim. */
  unavailableDetail?: string | null;
  /** Close the panel. */
  onClose?: () => void;
}

export const Steering: React.FC<SteeringProps> = ({
  runId,
  events,
  onSteer,
  canSteer,
  unavailableDetail,
  onClose,
}) => {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  /**
   * An acceptance we are still waiting on the daemon to confirm, remembered
   * with the marker count at the moment it was accepted. Once the stream grows
   * a new marker the acceptance has been answered and the note retires — it
   * must never linger next to the event it was superseded by.
   */
  const [awaiting, setAwaiting] = useState<{ at: string; markersThen: number } | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  const markers = useMemo(() => steeringMarkers(events, runId), [events, runId]);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  useEffect(() => {
    if (awaiting && markers.length > awaiting.markersThen) {
      setAwaiting(null);
    }
  }, [awaiting, markers.length]);

  const send = async () => {
    const trimmed = text.trim();
    if (!trimmed || sending || !canSteer) {
      return;
    }
    setSending(true);
    setFailure(null);
    const markersThen = markers.length;
    const outcome = await onSteer(trimmed);
    setSending(false);
    if (outcome.accepted) {
      // The text is cleared because the daemon took it. Nothing here claims it
      // is queued — that word belongs to the event.
      setText("");
      setAwaiting({ at: outcome.at, markersThen });
      return;
    }
    // A refused send leaves the text exactly where it was, so it can be
    // retried or edited rather than retyped.
    setAwaiting(null);
    setFailure(outcome.detail);
  };

  return (
    <section
      data-testid="steering-panel"
      aria-label="Steer the live run"
      style={{
        padding: "12px 24px",
        background: "var(--cody-panel)",
        borderTop: "1px solid var(--cody-border)",
        display: "flex",
        flexDirection: "column",
        gap: 10,
      }}
    >
      <header style={{ display: "flex", alignItems: "baseline", justifyContent: "space-between", gap: 12 }}>
        <div>
          <div style={{ fontSize: 13, fontWeight: 600, color: "var(--cody-text)" }}>Steer the live run</div>
          <div style={{ fontSize: 11, color: "var(--cody-text-muted)", marginTop: 2 }}>
            Redirects the run in flight. It does not stop it, and the daemon decides when the
            text takes effect.
          </div>
        </div>
        {onClose && (
          <button onClick={onClose} style={surfaceButton()} data-testid="steering-close">
            Close
          </button>
        )}
      </header>

      {!canSteer && (
        <div
          role="status"
          data-testid="steering-unavailable"
          style={{
            border: "1px solid var(--cody-warning-border)",
            background: "var(--cody-warning-bg)",
            color: "var(--cody-warning-text)",
            borderRadius: 6,
            padding: "8px 10px",
            fontSize: 12,
          }}
        >
          <strong>Steering unavailable.</strong>{" "}
          {unavailableDetail ?? "No connected daemon run to steer."}
        </div>
      )}

      <textarea
        ref={inputRef}
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            void send();
          }
        }}
        rows={2}
        disabled={!canSteer || sending}
        placeholder={
          canSteer
            ? "Redirect the agent — e.g. “stop rewriting tests, fix the parser first” (Enter to queue)"
            : "Steering unavailable."
        }
        aria-label="Steering text"
        style={{
          width: "100%",
          boxSizing: "border-box",
          background: "var(--cody-canvas)",
          border: "1px solid var(--cody-border-strong)",
          borderRadius: 6,
          outline: "none",
          color: "var(--cody-text)",
          padding: "8px 10px",
          fontSize: 13,
          resize: "none",
          fontFamily: "inherit",
        }}
      />

      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 12 }}>
        <span style={{ fontSize: 11, color: "var(--cody-text-muted)" }}>
          {runId ? `Run ${runId}` : "No run id"}
        </span>
        <button
          onClick={() => void send()}
          disabled={!canSteer || sending || !text.trim()}
          data-testid="steering-send"
          style={{
            ...surfaceButton(canSteer && !sending && text.trim() ? "primary" : "neutral"),
            color: canSteer && !sending && text.trim() ? "var(--cody-on-accent)" : "var(--cody-text-disabled)",
            cursor: canSteer && !sending && text.trim() ? "pointer" : "default",
            fontWeight: 600,
          }}
        >
          {sending ? "Sending…" : "Queue steering"}
        </button>
      </div>

      {failure && (
        <div
          role="alert"
          data-testid="steering-error"
          style={{
            border: "1px solid var(--cody-danger)",
            background: "var(--cody-danger-bg)",
            color: "var(--cody-danger-text)",
            borderRadius: 6,
            padding: "8px 10px",
            fontSize: 12,
          }}
        >
          <strong>Steering was not sent.</strong> {failure}
        </div>
      )}

      {awaiting && (
        <div
          role="status"
          data-testid="steering-awaiting"
          style={{ fontSize: 12, color: "var(--cody-text-muted)" }}
        >
          Accepted by the daemon at {clock(awaiting.at)} — <strong>not yet queued</strong>. Waiting
          for its <code>SteeringQueued</code> event.
        </div>
      )}

      <div>
        <div style={{ fontSize: 11, color: "var(--cody-text-muted)", marginBottom: 4 }}>
          Steering the daemon has reported for this run
        </div>
        {markers.length === 0 ? (
          <div style={{ fontSize: 12, color: "var(--cody-text-faint)" }} data-testid="steering-none">
            No <code>SteeringQueued</code> or <code>SteeringApplied</code> event has arrived on this
            session&rsquo;s stream.
          </div>
        ) : (
          <ul style={{ listStyle: "none", margin: 0, padding: 0, display: "flex", flexDirection: "column", gap: 4 }}>
            {markers.map((marker) => (
              <li
                key={marker.key}
                data-testid="steering-marker"
                data-state={marker.appliedAt ? "applied" : "queued"}
                style={{
                  fontSize: 12,
                  color: "var(--cody-text-secondary)",
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                }}
              >
                <span
                  style={{
                    fontSize: 10,
                    fontWeight: 700,
                    letterSpacing: 0.4,
                    textTransform: "uppercase",
                    borderRadius: 4,
                    padding: "1px 6px",
                    border: `1px solid ${marker.appliedAt ? "var(--cody-success)" : "var(--cody-warning-border)"}`,
                    color: marker.appliedAt ? "var(--cody-success-text)" : "var(--cody-warning-text)",
                    background: marker.appliedAt ? "var(--cody-success-bg)" : "var(--cody-warning-bg)",
                  }}
                >
                  {marker.appliedAt ? "applied" : "queued"}
                </span>
                <span>
                  {marker.queuedAt
                    ? `queued ${clock(marker.queuedAt)}`
                    : "queued at an unknown time (this client attached after the queue event)"}
                  {marker.appliedAt ? ` · applied ${clock(marker.appliedAt)}` : " · awaiting the run's next safe point"}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
};


