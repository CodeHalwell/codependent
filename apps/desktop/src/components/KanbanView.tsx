/**
 * The repository task board.
 *
 * It has no machinery of its own: a task board is stored as a **synthetic
 * workflow run** whose id is `board:<repository>`, so the ordinary blackboard
 * commands serve it — `ReadBlackboard { board_repository }` to read,
 * `PostBlackboardItem { scope: RepositoryBoard }` to create, and
 * `UpdateBlackboardItem` to move a card.
 *
 * Three rules this view exists to keep:
 *
 * 1. **The board is keyed by the checkout, not the launch directory.** The
 *    shell anchors to the git toplevel (`src-tauri/src/repo_anchor.rs`) before
 *    it asks. Hand the daemon a subdirectory instead and you get a second,
 *    permanently empty board with no error at all. The anchored path is shown
 *    in the header so a board that looks wrong can be checked rather than
 *    guessed at.
 * 2. **A card whose column is unrecognized goes in the FIRST column, never
 *    nowhere.** An unrecognized column must not hide work.
 * 3. **A move is a supersession server-side.** The daemon carries the card's
 *    body forward, re-ordinals it and republishes; this pane renders the
 *    replacement it gets back and never edits its own copy.
 */
import React, { useCallback, useEffect, useMemo, useState } from "react";
import { useLoadOnMount } from "../useLoadOnMount.js";
import type { BlackboardItemView } from "@codypendent/protocol";
import type { DesktopTransport } from "../transport.js";
import { subscribeToFrames } from "../frameBus.js";

export interface KanbanViewProps {
  transport: DesktopTransport | null;
  /** Why there is no transport, shown instead of an empty board. */
  unavailable?: string | null;
  /**
   * Bumped by the daemon hook each time a NEW connection attaches. The board's
   * `watchBoard` subscription belonged to the dropped connection, so without
   * re-watching here the live merge above simply stopped receiving cards with
   * nothing on screen saying so — the same reconnect gap `WorkflowView` and
   * `BlackboardView` already close.
   */
  connectionEpoch?: number;
}

/** The columns the board models, in order. Mirrors `KANBAN_COLUMNS`. */
export const KANBAN_COLUMNS = ["todo", "doing", "review", "done"] as const;
export type KanbanColumn = (typeof KANBAN_COLUMNS)[number];

type Board =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "loaded"; repository: string; scopeId: string; cards: BlackboardItemView[] }
  /** The board was never read. Distinct from a board that is read and empty. */
  | { status: "failed"; detail: string };

const button: React.CSSProperties = {
  padding: "4px 10px",
  background: "var(--cody-inset)",
  border: "1px solid var(--cody-border-strong)",
  borderRadius: 6,
  color: "var(--cody-text-secondary)",
  fontSize: 12,
  cursor: "pointer",
};

export const KanbanView: React.FC<KanbanViewProps> = ({
  transport,
  unavailable,
  connectionEpoch = 0,
}) => {
  const [board, setBoard] = useState<Board>({ status: "idle" });
  const [draft, setDraft] = useState("");
  const [notice, setNotice] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!transport?.watchBoard) {
      return;
    }
    setBoard({ status: "loading" });
    try {
      const view = await transport.watchBoard();
      setBoard({
        status: "loaded",
        repository: view.repository,
        scopeId: view.board_scope_id,
        cards: view.cards,
      });
    } catch (error) {
      // Never "no tasks": nobody looked. The two states are drawn differently
      // on purpose — one of them means the backlog is genuinely clear.
      setBoard({ status: "failed", detail: describe(error) });
    }
  }, [transport]);

  useLoadOnMount(load);

  // Re-establish the watch on a NEW connection (`useLoadOnMount` by design
  // ignores identity changes, so a reconnect never re-ran `load`).
  useEffect(() => {
    if (connectionEpoch > 0) {
      void load();
    }
    // Deliberately keyed on the epoch alone: "the connection changed".
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connectionEpoch]);

  // Live board deliveries. A post from an agent's `task.*` tool lands here the
  // same way an operator's does, so the board does not go stale behind a run.
  useEffect(() => {
    if (board.status !== "loaded") {
      return;
    }
    const scopeId = board.scopeId;
    return subscribeToFrames((frame) => {
      if (frame.kind !== "blackboard_posted") {
        return;
      }
      const item = frame.item;
      // The item carries its own board id, so a delivery for a workflow run's
      // board — or another repository's — is not folded into this one.
      if (item.workflow_run_id !== scopeId || item.kind !== "task") {
        return;
      }
      setBoard((current) =>
        current.status === "loaded" && current.scopeId === scopeId
          ? { ...current, cards: mergeCard(current.cards, item) }
          : current,
      );
    });
  }, [board.status, board.status === "loaded" ? board.scopeId : null]);

  const [creating, setCreating] = useState(false);
  const createCard = useCallback(async () => {
    if (!transport?.createBoardCard || creating) {
      return;
    }
    const title = draft.trim();
    if (title.length === 0) {
      setNotice("task title must not be empty");
      return;
    }
    setCreating(true);
    try {
      const card = await transport.createBoardCard(title);
      setDraft("");
      setNotice(null);
      setBoard((current) =>
        current.status === "loaded" ? { ...current, cards: mergeCard(current.cards, card) } : current,
      );
    } catch (error) {
      setNotice(describe(error));
    } finally {
      setCreating(false);
    }
  }, [transport, draft, creating]);

  const moveCard = useCallback(
    async (itemId: string, status: KanbanColumn) => {
      if (!transport?.moveBoardCard) {
        return;
      }
      try {
        const replacement = await transport.moveBoardCard(itemId, status);
        setBoard((current) =>
          current.status === "loaded"
            ? { ...current, cards: mergeCard(current.cards, replacement) }
            : current,
        );
      } catch (error) {
        // `blackboard.already-superseded` lands here when two moves raced. The
        // daemon's words, not a re-render of the move as if it had worked.
        setNotice(describe(error));
      }
    },
    [transport],
  );

  const columns = useMemo(
    () => (board.status === "loaded" ? groupByColumn(board.cards) : new Map<KanbanColumn, BlackboardItemView[]>()),
    [board],
  );

  return (
    <div
      role="region"
      aria-label="Task board"
      style={{ flex: 1, display: "flex", flexDirection: "column", height: "100vh", background: "var(--cody-canvas)", color: "var(--cody-text)", overflow: "hidden" }}
    >
      <div style={{ padding: "20px 24px 14px", borderBottom: "1px solid var(--cody-inset)" }}>
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", gap: 12, flexWrap: "wrap" }}>
          <div>
            <h1 style={{ margin: 0, fontSize: 20, fontWeight: 600 }}>Task Board</h1>
            {board.status === "loaded" ? (
              <p data-testid="board-anchor" style={{ margin: "4px 0 0", fontSize: 12, color: "var(--cody-text-muted)" }}>
                anchored to <code>{board.repository}</code>
              </p>
            ) : (
              <p style={{ margin: "4px 0 0", fontSize: 13, color: "var(--cody-text-muted)" }}>
                Cards the repository&rsquo;s agents and operators share
              </p>
            )}
          </div>
          <button style={button} onClick={() => void load()}>
            Refresh
          </button>
        </div>

        <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
          <input
            aria-label="New task title"
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                void createCard();
              }
            }}
            placeholder="New task title…"
            style={{
              flex: 1,
              padding: "7px 10px",
              background: "var(--cody-canvas)",
              border: "1px solid var(--cody-border-strong)",
              borderRadius: 6,
              color: "var(--cody-text)",
              fontSize: 13,
            }}
          />
          <button
            style={{ ...button, background: "var(--cody-success-strong)", color: "var(--cody-on-accent)" }}
            disabled={creating}
            onClick={() => void createCard()}
          >
            {creating ? "Adding…" : "Add task"}
          </button>
        </div>
        {notice && (
          <div role="status" style={{ marginTop: 8, fontSize: 12, color: "var(--cody-warning)" }}>
            {notice}
          </div>
        )}
      </div>

      {unavailable ? (
        <div data-testid="board-unavailable" role="status" style={note("var(--cody-warning)")}>
          Task board unavailable — {unavailable}
        </div>
      ) : board.status === "failed" ? (
        <div data-testid="board-failed" role="status" style={note("var(--cody-danger-soft)")}>
          The task board could not be read — {board.detail}
          <div style={{ marginTop: 8, fontSize: 12, color: "var(--cody-text-muted)" }}>
            This is not an empty backlog: the board was never read.
          </div>
        </div>
      ) : board.status !== "loaded" ? (
        <div role="status" style={note("var(--cody-text-muted)")}>
          Reading the board…
        </div>
      ) : (
        <div style={{ flex: 1, display: "flex", gap: 12, padding: "16px 24px", overflowX: "auto" }}>
          {KANBAN_COLUMNS.map((column) => {
            const cards = columns.get(column) ?? [];
            return (
              <div
                key={column}
                data-testid={`board-column-${column}`}
                // A Kanban board you can actually drag on. The per-card move
                // buttons remain — they are the keyboard/screen-reader path.
                onDragOver={(event) => event.preventDefault()}
                onDrop={(event) => {
                  event.preventDefault();
                  const cardId = event.dataTransfer.getData("text/plain");
                  // Dropping a card back on its own column is a no-op, not a
                  // redundant supersession on the daemon.
                  if (cardId && !cards.some((card) => card.id === cardId)) {
                    void moveCard(cardId, column);
                  }
                }}
                style={{
                  minWidth: 240,
                  flex: 1,
                  display: "flex",
                  flexDirection: "column",
                  gap: 8,
                  background: "var(--cody-canvas)",
                  border: "1px solid var(--cody-inset)",
                  borderRadius: 8,
                  padding: 10,
                }}
              >
                <div style={{ fontSize: 12, textTransform: "uppercase", color: "var(--cody-text-muted)", letterSpacing: 0.5 }}>
                  {column} · {cards.length}
                </div>
                {cards.length === 0 && (
                  <div style={{ fontSize: 12, color: "var(--cody-text-faint)", padding: "8px 2px" }}>No cards.</div>
                )}
                {cards.map((card) => (
                  <Card key={card.id} card={card} column={column} onMove={moveCard} />
                ))}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
};

const Card: React.FC<{
  card: BlackboardItemView;
  column: KanbanColumn;
  onMove: (itemId: string, status: KanbanColumn) => void | Promise<void>;
}> = ({ card, column, onMove }) => {
  const title = cardTitle(card);
  const misfiled = card.status !== undefined && card.status !== column;
  return (
    <div
      data-testid={`board-card-${card.id}`}
      draggable
      onDragStart={(event) => {
        event.dataTransfer.setData("text/plain", card.id);
        event.dataTransfer.effectAllowed = "move";
      }}
      style={{ padding: 10, background: "var(--cody-panel-raised)", border: "1px solid var(--cody-border-strong)", borderRadius: 6, display: "flex", flexDirection: "column", gap: 6, cursor: "grab" }}
    >
      <span style={{ fontSize: 13 }}>{title}</span>
      {misfiled && (
        // Shown, not hidden: the daemon put this card in a column this client
        // does not model, and the operator should know rather than wonder why
        // it is in "todo".
        <span style={{ fontSize: 11, color: "var(--cody-warning)" }}>column reported as “{card.status}”</span>
      )}
      {card.assignee && <span style={{ fontSize: 11, color: "var(--cody-text-muted)" }}>@{card.assignee}</span>}
      <div style={{ display: "flex", gap: 4, flexWrap: "wrap" }}>
        {KANBAN_COLUMNS.filter((target) => target !== column).map((target) => (
          <button
            key={target}
            style={{ ...button, padding: "2px 7px", fontSize: 11 }}
            aria-label={`Move ${title} to ${target}`}
            title={`Move this card to the ${target} column`}
            onClick={() => void onMove(card.id, target)}
          >
            → {target}
          </button>
        ))}
      </div>
    </div>
  );
};

/**
 * Group live cards into columns.
 *
 * A card whose status matches no modelled column lands in the **first** column
 * rather than being dropped: an unrecognized column must never hide work. A
 * superseded card is excluded — the read asks for the live board only, and a
 * live delivery of a replacement retires the row it supersedes.
 */
export function groupByColumn(cards: BlackboardItemView[]): Map<KanbanColumn, BlackboardItemView[]> {
  const grouped = new Map<KanbanColumn, BlackboardItemView[]>();
  for (const column of KANBAN_COLUMNS) {
    grouped.set(column, []);
  }
  const ordered = [...cards].sort((left, right) => (left.ordinal ?? 0) - (right.ordinal ?? 0));
  for (const card of ordered) {
    if (card.superseded_by) {
      continue;
    }
    const column = (KANBAN_COLUMNS as readonly string[]).includes(card.status ?? "")
      ? (card.status as KanbanColumn)
      : KANBAN_COLUMNS[0];
    grouped.get(column)?.push(card);
  }
  return grouped;
}

/**
 * Fold one delivery into the card list: replace the row with the same id, drop
 * the row this item supersedes, and append anything new.
 */
export function mergeCard(
  cards: BlackboardItemView[],
  incoming: BlackboardItemView,
): BlackboardItemView[] {
  const retired = new Set<string>([incoming.id]);
  // A supersession arrives as a NEW item naming its predecessor, so the
  // predecessor has to leave by id rather than by waiting for a delete.
  for (const card of cards) {
    if (card.superseded_by === incoming.id) {
      retired.add(card.id);
    }
  }
  const kept = cards.filter((card) => !retired.has(card.id));
  return incoming.superseded_by ? kept : [...kept, incoming];
}

/**
 * A task card's payload is opaque JSON by contract; the board convention is
 * `{ title, description }`. A payload without a usable title falls back to the
 * card id rather than rendering "undefined" or inventing a name.
 */
function cardTitle(card: BlackboardItemView): string {
  const payload = card.payload;
  if (payload !== null && typeof payload === "object" && !Array.isArray(payload)) {
    const title = (payload as Record<string, unknown>).title;
    if (typeof title === "string" && title.trim().length > 0) {
      return title;
    }
  }
  return card.id;
}

function note(color: string): React.CSSProperties {
  return { padding: "48px 24px", textAlign: "center", color, fontSize: 14 };
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
