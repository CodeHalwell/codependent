/**
 * Notifications for work that is BLOCKING A HUMAN.
 *
 * Until this module existed the inbox rendered inside the editor and nothing
 * ever raised a notification, so a run parked on an approval was invisible
 * unless the Codypendent view already happened to be in front of the user.
 *
 * What gets a notification, and why only these two:
 *
 *   - `ApprovalRequested` — the run is stopped until a human decides.
 *   - `QuestionAsked`     — the run is parked until a human answers.
 *
 * Nothing else. `RunCompleted`, `ToolStarted`/`ToolCompleted`,
 * `BudgetWarning`, `ContextUsage`, `PatchProposed`, `NoteAppended`,
 * `ClientPresenceChanged` and every future variant are *information*: they can
 * wait for the inbox and the status bar. A harness that pops a toast per tool
 * call trains the user to dismiss toasts, which loses the two that mattered.
 *
 * Honesty rules this module obeys:
 *
 *   - It only ever says something is pending when the daemon's own event
 *     stream says so. Replayed history is FOLDED first
 *     (`ApprovalResolved`/`QuestionResolved` cancel their request), so
 *     re-attaching to an old session does not announce decisions the user
 *     already made.
 *   - One notification per daemon id, ever (`notified`), so a reconnect that
 *     replays the same event does not re-announce it.
 *   - A decision is never sent for an item this client has seen resolved: the
 *     stale toast says so instead of pretending the click landed.
 *
 * The module is deliberately the only notification surface in the extension;
 * `extension.ts` owns the wiring (live events, and the catch-up replay).
 */
import * as vscode from "vscode";

import type {
  Catchup,
  EventBody,
  PendingApprovalProjection,
  SessionEvent,
} from "@codypendent/protocol";

/** The two event kinds that genuinely block a human. */
export type BlockingWorkKind = "approval" | "question";

export interface BlockingWork {
  kind: BlockingWorkKind;
  /** The daemon's own id: an `ApprovalId` for an approval, a `QuestionId` for a question. */
  id: string;
  /** The session whose run is parked on this item. */
  sessionId: string;
  /** One line naming what is blocked, already rendered for a human. */
  summary: string;
}

/** What the notifier needs from its host to act on the user's choice. */
export interface NotifierHost {
  /** Relay a decision for an approval raised by `sessionId`. */
  resolveApproval(sessionId: string, approvalId: string, decision: "Approve" | "Reject"): void;
  /** Bring the session that raised the item in front of the user. */
  focusSession(sessionId: string): void;
}

/** Label of the action that focuses the session the item belongs to. */
export const FOCUS_SESSION_ACTION = "Open Session";

/**
 * How many ids to remember. A long-lived window must not grow a set per
 * approval forever; the oldest ids are evicted, and an evicted id can at worst
 * be announced twice, never announced falsely.
 */
const MAX_REMEMBERED_IDS = 512;

/**
 * The blocking work carried by one event, or `null` for every other event —
 * including a variant this build does not know (fail closed: an unknown
 * variant is never announced as blocking).
 */
export function blockingWorkOf(event: SessionEvent, sessionId: string): BlockingWork | null {
  const body: EventBody = event.body;
  if (body.type === "ApprovalRequested") {
    return {
      kind: "approval",
      id: body.approval_id,
      sessionId,
      summary: `Approval required: ${withRisk(describeApprovalAction(body.action), body.risk)}`,
    };
  }
  if (body.type === "QuestionAsked") {
    const prompts = Array.isArray(body.questions) ? body.questions : [];
    const first = prompts[0];
    const headline = first
      ? `${first.header ? `${first.header}: ` : ""}${first.question}`
      : "the run is waiting for an answer";
    const more = prompts.length > 1 ? ` (+${prompts.length - 1} more)` : "";
    return { kind: "question", id: body.question_id, sessionId, summary: `Question: ${headline}${more}` };
  }
  return null;
}

/** The blocking-work key an event RESOLVES, or `null` when it resolves nothing. */
export function resolvedWorkKeyOf(event: SessionEvent): string | null {
  const body: EventBody = event.body;
  if (body.type === "ApprovalResolved") return key("approval", body.approval_id);
  if (body.type === "QuestionResolved") return key("question", body.question_id);
  return null;
}

/**
 * Fold a replayed event range down to the items STILL blocking a human.
 *
 * Order matters and is the daemon's: a request followed by its resolution
 * leaves nothing pending, which is why catch-up cannot simply notify on every
 * `ApprovalRequested` it replays.
 */
export function pendingBlockingWork(events: readonly SessionEvent[], sessionId: string): BlockingWork[] {
  const pending = new Map<string, BlockingWork>();
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

  constructor(private readonly host: NotifierHost) {}

  /** Forget every id for a session change: a new attach is a new projection. */
  reset(): void {
    this.notified.clear();
    this.resolved.clear();
  }

  /**
   * Observe one LIVE event. Called for every event on the attached stream;
   * announces the two blocking kinds and records resolutions of the rest.
   */
  observeLiveEvent(event: SessionEvent, sessionId: string): void {
    const resolvedKey = resolvedWorkKeyOf(event);
    if (resolvedKey) {
      remember(this.resolved, resolvedKey);
      return;
    }
    const work = blockingWorkOf(event, sessionId);
    if (work) this.announce(work);
  }

  /**
   * Observe a replayed range (attach/reconnect catch-up). Only what the fold
   * leaves pending is announced, and a batch is coalesced into one
   * notification so re-attaching to a busy session raises one toast, not five.
   */
  observeReplay(events: readonly SessionEvent[], sessionId: string): void {
    for (const event of events) {
      const resolvedKey = resolvedWorkKeyOf(event);
      if (resolvedKey) remember(this.resolved, resolvedKey);
    }
    this.announceBatch(pendingBlockingWork(events, sessionId));
  }

  /**
   * Observe the pending approvals of a compacted catch-up snapshot — the
   * daemon's own answer to "what is still pending", so these need no folding.
   *
   * A snapshot's `SessionProjection` carries no pending *questions*, so this
   * client cannot know whether a parked question survived compaction and
   * therefore says nothing about questions here (see `follow_ups`).
   */
  observePendingApprovals(
    approvals: readonly { approval_id: string; summary: string }[],
    sessionId: string,
  ): void {
    this.announceBatch(
      approvals.map((approval) => ({
        kind: "approval" as const,
        id: approval.approval_id,
        sessionId,
        summary: `Approval required: ${approval.summary}`,
      })),
    );
  }

  private announceBatch(items: readonly BlockingWork[]): void {
    const fresh = items.filter((item) => this.isFresh(item));
    const first = fresh[0];
    if (first === undefined) return;
    if (fresh.length === 1) {
      this.announce(first);
      return;
    }
    for (const item of fresh) remember(this.notified, key(item.kind, item.id));
    const sessionId = first.sessionId;
    void vscode.window
      .showWarningMessage(
        `Codypendent: ${fresh.length} items are waiting for you — ${fresh
          .map((item) => item.summary)
          .join(" · ")}`,
        FOCUS_SESSION_ACTION,
      )
      .then((choice) => {
        if (choice === FOCUS_SESSION_ACTION) this.host.focusSession(sessionId);
      });
  }

  private isFresh(work: BlockingWork): boolean {
    const id = key(work.kind, work.id);
    return !this.notified.has(id) && !this.resolved.has(id);
  }

  private announce(work: BlockingWork): void {
    if (!this.isFresh(work)) return;
    const id = key(work.kind, work.id);
    remember(this.notified, id);

    const actions =
      work.kind === "approval"
        ? ["Approve", "Reject", FOCUS_SESSION_ACTION]
        : [FOCUS_SESSION_ACTION];

    void vscode.window
      .showWarningMessage(`Codypendent: ${work.summary}`, { modal: false }, ...actions)
      .then((choice) => {
        if (choice === undefined) return;
        if (choice === FOCUS_SESSION_ACTION) {
          this.host.focusSession(work.sessionId);
          return;
        }
        // The item may have been resolved elsewhere (TUI, another editor)
        // while this non-modal toast sat on screen. Never relay a decision for
        // something already decided, and never leave the user believing a
        // click landed when it did not.
        if (this.resolved.has(id)) {
          void vscode.window.showInformationMessage(
            "Codypendent: that item was already resolved elsewhere; nothing was sent.",
          );
          return;
        }
        if (choice === "Approve" || choice === "Reject") {
          this.host.resolveApproval(work.sessionId, work.id, choice);
        }
      });
  }
}

/**
 * The part of a `DaemonClient` this module needs: its live event stream and
 * its catch-up. Structural so a test can drive the real subscription with a
 * stand-in emitter instead of a socket.
 */
export interface BlockingWorkStream {
  on(event: "event", listener: (event: SessionEvent) => void): unknown;
  on(event: "catchup", listener: (catchup: Catchup) => void): unknown;
}

/**
 * Wire a connected client to the notifier. This is the whole notification
 * path: no other call site announces blocking work, so there is exactly one
 * place where "the daemon said a human is needed" becomes a notification.
 *
 * `describeApproval` renders a snapshot's pending approval for the message;
 * it is the extension's existing action/risk renderer, passed in so this
 * module stays free of the transcript's formatting.
 */
export function subscribeBlockingWork(
  stream: BlockingWorkStream,
  sessionId: string,
  notifier: BlockingWorkNotifier,
  describeApproval: (approval: PendingApprovalProjection) => string,
): void {
  stream.on("event", (event: SessionEvent) => {
    notifier.observeLiveEvent(event, sessionId);
  });
  stream.on("catchup", (catchup: Catchup) => {
    if (catchup.type === "Events") {
      notifier.observeReplay(catchup.events, sessionId);
      return;
    }
    if (catchup.type === "Snapshot") {
      notifier.observePendingApprovals(
        (catchup.projection.pending_approvals ?? []).map((approval) => ({
          approval_id: approval.approval_id,
          summary: describeApproval(approval),
        })),
        sessionId,
      );
    }
  });
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
 * Append the risk level, and only when the event actually carried one. The
 * wire declares `risk`, but the value crossed a JSON boundary from a daemon
 * that may not be this build: an unreadable risk is left out rather than
 * rendered as a level, and never throws inside the event handler.
 */
function withRisk(summary: string, risk: unknown): string {
  if (risk && typeof risk === "object") {
    const level = (risk as { level?: unknown }).level;
    if (level && typeof level === "object") {
      const type = (level as { type?: unknown }).type;
      if (typeof type === "string") return `${summary} (risk: ${type})`;
    }
  }
  return summary;
}

/** A one-line, human-readable rendering of a proposed action. */
function describeApprovalAction(action: Extract<EventBody, { type: "ApprovalRequested" }>["action"]): string {
  switch (action.type) {
    case "ExecuteCommand":
      return `run ${action.program} ${Array.isArray(action.args) ? action.args.join(" ") : ""}`.trim();
    case "ReadFiles":
      return `read ${Array.isArray(action.paths) ? action.paths.length : 0} file(s)`;
    case "WritePatch":
      return "write a patch";
    case "NetworkRequest":
      return `network request to ${action.destination}`;
    case "GitCommit":
      return `git commit in ${action.repository}`;
    case "GitPush":
      return `git push ${action.branch} -> ${action.remote}`;
    case "GitHubMutation":
      return action.summary;
    case "McpToolCall":
      return action.summary;
    case "AcpToolCall":
      return `${action.agent} · ${action.title}`;
    case "PublishDocument":
      return `publish document to ${action.target}`;
    default:
      // An action variant this build does not name still blocks the run, so it
      // is still announced — by its tag, with nothing invented about it.
      return `action ${action.type}`;
  }
}
