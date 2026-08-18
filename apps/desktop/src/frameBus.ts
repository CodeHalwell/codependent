/**
 * A fan-out for daemon frames that are **not** session-scoped.
 *
 * The shell opens exactly one Tauri channel per connection (`daemon_connect`),
 * and `useDaemon` owns it. That is right for the transcript, whose frames are
 * all about the attached session — but `workflow_event` and `blackboard_posted`
 * are not: each carries its own `workflow_run_id` and belongs to whichever
 * panel is showing that run or board, which may be none of them.
 *
 * Rather than teach the session reducer about boards it does not model, the
 * frames are published here and the panels subscribe. A panel with no
 * subscriber simply has no listener; nothing is buffered and nothing is
 * invented — a panel opened later takes its baseline from a real read
 * (`watch_workflow` / `watch_board`), never from replayed scrollback.
 */
import type { DaemonFrame } from "./transport.js";

export type FrameListener = (frame: DaemonFrame) => void;

const listeners = new Set<FrameListener>();

/**
 * Listen to every daemon frame until the returned function is called.
 *
 * A listener that throws must not stop the others from being told, so each is
 * invoked independently.
 */
export function subscribeToFrames(listener: FrameListener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Publish one frame. Called by the single channel owner (`useDaemon`). */
export function publishFrame(frame: DaemonFrame): void {
  for (const listener of [...listeners]) {
    try {
      listener(frame);
    } catch {
      // A panel that failed to fold a frame is that panel's problem; it must
      // not silence the live stream for every other panel.
    }
  }
}

/** Test seam: drop every listener between cases. */
export function resetFrameBus(): void {
  listeners.clear();
}
