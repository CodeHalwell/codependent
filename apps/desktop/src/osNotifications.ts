/**
 * OS notifications for work that is BLOCKING A HUMAN.
 *
 * The desktop app rendered its inbox and its approval cards inside the window
 * and raised nothing at the OS level, so a run parked on an approval was
 * invisible to anyone who had the window behind their editor. This module is
 * that missing surface, and the ONLY one: it turns daemon frames into a
 * notification through Tauri's notification plugin
 * (`@tauri-apps/plugin-notification`, permission declared in
 * `src-tauri/capabilities/default.json`).
 *
 * Exactly two event kinds qualify, because exactly two of them stop a run
 * until a person acts:
 *
 *   - `ApprovalRequested` — the run is waiting on a decision.
 *   - `QuestionAsked`     — the run is parked on an answer.
 *
 * `RunCompleted`, `ToolStarted`/`ToolCompleted`, `BudgetWarning`,
 * `ContextUsage`, `PatchProposed`, `LearningsCaptured` and every variant a
 * newer daemon invents are information: they belong to the transcript and the
 * inbox badge. Notifying on all of them would train the user to swipe
 * notifications away, which loses the two that mattered — over-notifying is
 * treated here as a defect, not as thoroughness.
 *
 * Nothing is ever announced that the daemon has not stated:
 *
 *   - A replayed range (`history`, or a `Catchup` of events) is FOLDED first,
 *     so a request whose `ApprovalResolved`/`QuestionResolved` is in the same
 *     range is history and stays silent.
 *   - A compacted `Snapshot` announces only its `pending_approvals`, which is
 *     the daemon's own pending set. The projection carries no pending
 *     questions, so after a compaction this client says nothing about
 *     questions rather than guessing.
 *   - One notification per daemon id, ever, so a reconnect that replays the
 *     same event does not announce it twice.
 */
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

import type { Catchup, EventBody, SessionEvent } from "@codypendent/protocol";
import { shellAvailable, type DaemonFrame } from "./transport.js";

export type BlockingWorkKind = "approval" | "question";

export interface BlockingWorkNotice {
  kind: BlockingWorkKind;
  /** The daemon's own `ApprovalId` / `QuestionId`. */
  id: string;
  /** The session the parked run belongs to, when the frame named one. */
  sessionId: string | null;
  /** Notification title. */
  title: string;
  /** Notification body: what is blocked, in one line. */
  body: string;
}

/** Where a notice goes. Production sends it to the OS; tests record it. */
export type NotificationSink = (notice: BlockingWorkNotice) => void;

const MAX_REMEMBERED_IDS = 512;

/** The blocking work an event carries, or `null` for every other event. */
export function blockingWorkOf(event: SessionEvent, sessionId: string | null): BlockingWorkNotice | null {
  const body: EventBody = event.body;
  if (body.type === "ApprovalRequested") {
    return {
      kind: "approval",
      id: body.approval_id,
      sessionId,
      title: "Approval needed",
      body: withRisk(describeAction(body.action), body.risk),
    };
  }
  if (body.type === "QuestionAsked") {
    const prompts = Array.isArray(body.questions) ? body.questions : [];
    const first = prompts[0];
    const headline = first
      ? `${first.header ? `${first.header}: ` : ""}${first.question}`
      : "the run is waiting for an answer";
    return {
      kind: "question",
      id: body.question_id,
      sessionId,
      title: "Question waiting",
      body: prompts.length > 1 ? `${headline} (+${prompts.length - 1} more)` : headline,
    };
  }
  return null;
}

/** The key an event resolves, or `null` when it resolves nothing. */
export function resolvedWorkKeyOf(event: SessionEvent): string | null {
  const body: EventBody = event.body;
  if (body.type === "ApprovalResolved") return key("approval", body.approval_id);
  if (body.type === "QuestionResolved") return key("question", body.question_id);
  return null;
}

/** Fold a replayed range down to what is STILL blocking a human. */
export function pendingBlockingWork(
  events: readonly SessionEvent[],
  sessionId: string | null,
): BlockingWorkNotice[] {
  const pending = new Map<string, BlockingWorkNotice>();
  for (const event of events) {
    const work = blockingWorkOf(event, sessionId);
    if (work) {
      pending.set(key(work.kind, work.id), work);
      continue;
    }
    const resolved = resolvedWorkKeyOf(event);
    if (resolved) pending.delete(resolved);
  }
  return [...pending.values()];
}

export class BlockingWorkNotifier {
  private readonly notified = new Set<string>();
  private readonly resolved = new Set<string>();

  constructor(private readonly sink: NotificationSink) {}

  /**
   * Observe one frame from the shell. This is the entire notification path:
   * the same frames that drive the store, read for blocking work only.
   */
  observeFrame(frame: DaemonFrame): void {
    switch (frame.kind) {
      case "event":
        this.observeLiveEvent(frame.event, frame.session_id);
        return;
      case "history":
        this.observeReplay(frame.events, frame.session_id);
        return;
      case "catchup":
        this.observeCatchup(frame.snapshot, frame.session_id);
        return;
      case "disconnected":
        // A dropped socket says nothing about what is pending. Announce
        // nothing, and keep the ids so a reconnect's replay stays quiet.
        return;
    }
  }

  private observeLiveEvent(event: SessionEvent, sessionId: string | null): void {
    const resolvedKey = resolvedWorkKeyOf(event);
    if (resolvedKey) {
      remember(this.resolved, resolvedKey);
      return;
    }
    const work = blockingWorkOf(event, sessionId);
    if (work) this.announceBatch([work]);
  }

  private observeReplay(events: readonly SessionEvent[], sessionId: string | null): void {
    for (const event of events) {
      const resolvedKey = resolvedWorkKeyOf(event);
      if (resolvedKey) remember(this.resolved, resolvedKey);
    }
    this.announceBatch(pendingBlockingWork(events, sessionId));
  }

  private observeCatchup(snapshot: Catchup, sessionId: string | null): void {
    if (snapshot.type === "Events") {
      this.observeReplay(snapshot.events, sessionId);
      return;
    }
    if (snapshot.type === "Snapshot") {
      const pending = snapshot.projection.pending_approvals ?? [];
      this.announceBatch(
        pending.map((approval) => ({
          kind: "approval" as const,
          id: approval.approval_id,
          sessionId,
          title: "Approval needed",
          body: withRisk(describeAction(approval.action), approval.risk),
        })),
      );
    }
    // `Unknown` is a catch-up shape this build cannot read: it is not evidence
    // of anything pending, so it raises nothing (fail closed).
  }

  /**
   * Send what is fresh. A backlog is coalesced into ONE notification: opening
   * the app to five parked approvals must not fire five notifications.
   */
  private announceBatch(items: readonly BlockingWorkNotice[]): void {
    const fresh = items.filter((item) => {
      const id = key(item.kind, item.id);
      return !this.notified.has(id) && !this.resolved.has(id);
    });
    const first = fresh[0];
    if (first === undefined) return;
    for (const item of fresh) remember(this.notified, key(item.kind, item.id));
    if (fresh.length === 1) {
      this.sink(first);
      return;
    }
    this.sink({
      ...first,
      title: `${fresh.length} items need you`,
      body: fresh.map((item) => item.body).join(" · "),
    });
  }
}

/**
 * The production sink: Tauri's notification plugin.
 *
 * Permission is asked for once, lazily, and its answer cached. If the user (or
 * the OS) refuses, notifications are simply not delivered — the app says
 * nothing about it and never claims a notification was shown.
 *
 * The plugin's notification *actions* (`onAction`) are mobile-only, so a
 * desktop notification carries no button; clicking it activates the app the
 * way the platform does. The session is named in the body so the user knows
 * where to go (see `follow_ups` for the deep-link the plugin cannot do yet).
 */
export function createOsNotificationSink(
  describeSession: (sessionId: string | null) => string | undefined = () => undefined,
): NotificationSink {
  let permission: Promise<boolean> | undefined;
  const allowed = (): Promise<boolean> => {
    permission ??= (async () => {
      if (await isPermissionGranted()) return true;
      return (await requestPermission()) === "granted";
    })().catch(() => false);
    return permission;
  };

  return (notice) => {
    const session = describeSession(notice.sessionId);
    const body = session ? `${notice.body}\n(${session})` : notice.body;
    void allowed()
      .then((granted) => {
        if (granted) sendNotification({ title: `Codypendent: ${notice.title}`, body });
      })
      .catch(() => undefined);
  };
}

/**
 * The sink the app uses by default: the OS one inside the Tauri shell, and a
 * no-op outside it (a plain `vite dev` tab has no plugin to call).
 */
export function defaultNotificationSink(
  describeSession?: (sessionId: string | null) => string | undefined,
): NotificationSink {
  if (!shellAvailable()) {
    return () => undefined;
  }
  return createOsNotificationSink(describeSession);
}

function key(kind: BlockingWorkKind, id: string): string {
  return `${kind}:${id}`;
}

function remember(set: Set<string>, id: string): void {
  set.add(id);
  while (set.size > MAX_REMEMBERED_IDS) {
    const oldest = set.values().next().value;
    if (oldest === undefined) break;
    set.delete(oldest);
  }
}

/**
 * Append the risk level, and only when the event actually carried one.
 *
 * The wire type declares `risk`, but these values cross a JSON boundary from a
 * daemon that may be older or newer than this client: an unreadable risk is
 * reported as absent rather than as a level, and never crashes the frame
 * handler that the store also runs on.
 */
function withRisk(summary: string, risk: unknown): string {
  const level = riskLevel(risk);
  return level === undefined ? summary : `${summary} — risk ${level}`;
}

function riskLevel(risk: unknown): string | undefined {
  if (!risk || typeof risk !== "object") return undefined;
  const level = (risk as { level?: unknown }).level;
  if (!level || typeof level !== "object") return undefined;
  const type = (level as { type?: unknown }).type;
  return typeof type === "string" ? type : undefined;
}

/** One line describing a proposed action, with nothing invented about it. */
function describeAction(action: unknown): string {
  if (action && typeof action === "object") {
    const record = action as Record<string, unknown>;
    const kind = text(record.type) || "action";
    const program = record.program
      ? `${String(record.program)} ${Array.isArray(record.args) ? record.args.join(" ") : ""}`.trim()
      : "";
    const detail =
      program
      || text(record.command)
      || text(record.path)
      || text(record.summary)
      || text(record.destination)
      || text(record.url);
    return detail ? `${kind}: ${detail}` : kind;
  }
  return "action";
}

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}
