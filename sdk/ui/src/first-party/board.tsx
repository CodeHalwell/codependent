/** @jsxImportSource react */
import type { ReactNode } from "react";
import {
  ActionMenu,
  Box,
  Button,
  Row,
  Stack,
  Text,
} from "../react/primitives.js";
import { StatusBadge, SurfaceFrame, VirtualizedCollection } from "./foundation.js";
import { toUiJson, type SemanticIntent, type SurfaceOptions } from "./types.js";

export interface BoardCard {
  id: string;
  title: string;
  /** The column this card currently sits in. */
  columnId: string;
  status?: string;
  assignee?: string;
  /** Free-form card kind (`task`, `finding`, `decision`, …). */
  kind?: string;
  summary?: string;
  /** Anything a producer wants a renderer to carry, e.g. a board item payload. */
  data?: unknown;
}

export interface BoardColumn {
  id: string;
  label: string;
  description?: string;
  /** Shown instead of cards when the column is empty. */
  emptyMessage?: string;
  /** Advisory WIP ceiling; an exceeded column is called out, never blocked. */
  limit?: number;
}

export interface KanbanBoardProps extends SurfaceOptions {
  columns: readonly BoardColumn[];
  cards: readonly BoardCard[];
  /** Host-mediated action for opening a card; receives `{ cardId }`. */
  selectCardAction: string;
  selectedCardId?: string;
  /**
   * Host-mediated action for moving a card to another column. Every column the
   * card is not already in becomes one menu entry carrying `cardId`,
   * `fromColumnId`, and `toColumnId` — a keyboard-reachable stand-in for
   * pointer drag-and-drop, which the event vocabulary does not carry.
   */
  moveCardAction?: string;
  /** Extra per-card intents (assign, archive, …); each receives `{ cardId }`. */
  cardIntents?: readonly SemanticIntent<{ cardId: string }>[];
}

/** A column descriptor for cards whose `columnId` the board does not define. */
const UNPLACED: BoardColumn = {
  id: "core.board.unplaced",
  label: "Unplaced",
  description: "These cards name a column this board does not define.",
  emptyMessage: "No unplaced cards",
};

function cardCaption(card: BoardCard): string {
  return [card.kind, card.assignee === undefined ? undefined : `@${card.assignee}`]
    .filter((part): part is string => part !== undefined && part.length > 0)
    .join(" · ");
}

function cardLabel(card: BoardCard, column: BoardColumn): string {
  const caption = cardCaption(card);
  return [`${card.title}, in ${column.label}`, card.status, caption]
    .filter((part): part is string => part !== undefined && part.length > 0)
    .join(", ");
}

function BoardCardView({
  card,
  column,
  destinations,
  selectCardAction,
  selectedCardId,
  moveCardAction,
  cardIntents,
}: {
  card: BoardCard;
  column: BoardColumn;
  destinations: readonly BoardColumn[];
  selectCardAction: string;
  selectedCardId: string | undefined;
  moveCardAction: string | undefined;
  cardIntents: readonly SemanticIntent<{ cardId: string }>[];
}): ReactNode {
  // Each entry carries its own action id and flat payload fields, so both the
  // menu's own action and a per-entry override resolve the same way in every
  // host.
  const menuItems = [
    ...cardIntents.map((intent) => toUiJson({
      id: `${intent.action}:${card.id}`,
      label: intent.label,
      action: intent.action,
      cardId: card.id,
      ...(intent.shortcut === undefined ? {} : { shortcut: intent.shortcut }),
    })),
    ...(moveCardAction === undefined ? [] : destinations
      .filter((destination) => destination.id !== card.columnId)
      .map((destination) => toUiJson({
        id: `move:${card.id}:${destination.id}`,
        label: `Move to ${destination.label}`,
        action: moveCardAction,
        cardId: card.id,
        fromColumnId: card.columnId,
        toColumnId: destination.id,
      }))),
  ];
  return (
    <Box
      id={`card-${card.id}`}
      border="rounded"
      padding="xs"
      accessibleLabel={cardLabel(card, column)}
    >
      <Stack gap="xs">
        <Row align="spaceBetween" gap="xs">
          <Text value={card.title} weight={card.id === selectedCardId ? "bold" : "medium"} />
          {card.status === undefined ? null : <StatusBadge status={card.status} />}
        </Row>
        {cardCaption(card).length === 0 ? null : <Text value={cardCaption(card)} role="caption" tone="muted" />}
        {card.summary === undefined ? null : <Text value={card.summary} role="caption" tone="muted" truncate />}
        <Row gap="xs">
          <Button
            action={selectCardAction}
            label="Open"
            payload={toUiJson({ cardId: card.id })}
            accessibleLabel={`Open ${card.title}`}
          />
          {menuItems.length === 0 ? null : (
            <ActionMenu
              items={menuItems}
              accessibleLabel={`${card.title} actions`}
              {...(moveCardAction === undefined ? {} : { action: moveCardAction, description: "Move this card with the keyboard" })}
            />
          )}
        </Row>
      </Stack>
    </Box>
  );
}

function BoardColumnView({
  surfaceId,
  column,
  cards,
  destinations,
  selectCardAction,
  selectedCardId,
  moveCardAction,
  cardIntents,
}: {
  surfaceId: string;
  column: BoardColumn;
  cards: readonly BoardCard[];
  destinations: readonly BoardColumn[];
  selectCardAction: string;
  selectedCardId: string | undefined;
  moveCardAction: string | undefined;
  cardIntents: readonly SemanticIntent<{ cardId: string }>[];
}): ReactNode {
  const overLimit = column.limit !== undefined && cards.length > column.limit;
  return (
    <Stack gap="xs" grow={1} accessibleLabel={`${column.label}, ${cards.length} cards`}>
      <Row align="spaceBetween" gap="xs">
        <Text value={column.label} role="heading" weight="bold" />
        <Text
          value={column.limit === undefined ? String(cards.length) : `${cards.length}/${column.limit}`}
          role="status"
          tone={overLimit ? "warning" : "muted"}
          accessibleLabel={overLimit
            ? `${cards.length} cards, over the limit of ${column.limit}`
            : `${cards.length} cards`}
        />
      </Row>
      {column.description === undefined ? null : <Text value={column.description} role="caption" tone="muted" />}
      <VirtualizedCollection
        id={`${surfaceId}-column-${column.id}`}
        label={`${column.label} cards`}
        items={cards}
        selectedKey={cards.some((card) => card.id === selectedCardId) ? selectedCardId : undefined}
        emptyMessage={column.emptyMessage ?? `No cards in ${column.label}`}
        itemKey={(card) => card.id}
      >
        {cards.map((card) => (
          <BoardCardView
            key={card.id}
            card={card}
            column={column}
            destinations={destinations}
            selectCardAction={selectCardAction}
            selectedCardId={selectedCardId}
            moveCardAction={moveCardAction}
            cardIntents={cardIntents}
          />
        ))}
      </VirtualizedCollection>
    </Stack>
  );
}

/**
 * A keyboard-first kanban board: a Row of virtualized columns, each holding
 * cards that carry status, assignee, and kind.
 *
 * Card movement is expressed as `ActionMenu` intents rather than pointer
 * drag-and-drop — the event vocabulary has no `drop`, and an intent is
 * reachable from the keyboard, replayable, and mediated by the host like any
 * other command. The whole board degrades to one flat grouped `List` through
 * the Row's `fallback`, so a narrow terminal or a host without `Row` still
 * shows every card and the column it is in.
 */
export function KanbanBoard({
  columns,
  cards,
  selectCardAction,
  selectedCardId,
  moveCardAction,
  cardIntents = [],
  ...surface
}: KanbanBoardProps): ReactNode {
  const byColumn = new Map(columns.map((column) => [column.id, [] as BoardCard[]]));
  const unplaced: BoardCard[] = [];
  for (const card of cards) {
    const bucket = byColumn.get(card.columnId);
    if (bucket === undefined) unplaced.push(card);
    else bucket.push(card);
  }
  const rendered = [
    ...columns.map((column) => [column, byColumn.get(column.id) ?? []] as const),
    ...(unplaced.length === 0 ? [] : [[UNPLACED, unplaced] as const]),
  ];
  const flat = rendered.flatMap(([column, columnCards]) =>
    columnCards.map((card) => toUiJson({
      id: card.id,
      label: `${column.label}: ${card.title}`,
      columnId: column.id,
      ...(card.status === undefined ? {} : { status: card.status }),
      ...(card.assignee === undefined ? {} : { assignee: card.assignee }),
      ...(card.kind === undefined ? {} : { kind: card.kind }),
    })),
  );
  return (
    <SurfaceFrame {...surface} width={surface.width ?? "full"}>
      <Row
        gap="sm"
        align="start"
        accessibleLabel={`${surface.title}: ${columns.length} columns, ${cards.length} cards`}
        fallback={{
          kind: "element",
          type: "List",
          props: {
            items: flat,
            virtualized: true,
            selectAction: selectCardAction,
            accessibleLabel: `${surface.title} cards, grouped by column`,
            emptyMessage: "This board has no cards",
          },
          children: [],
        }}
      >
        {rendered.map(([column, columnCards]) => (
          <BoardColumnView
            key={column.id}
            surfaceId={surface.id}
            column={column}
            cards={columnCards}
            destinations={columns}
            selectCardAction={selectCardAction}
            selectedCardId={selectedCardId}
            moveCardAction={moveCardAction}
            cardIntents={cardIntents}
          />
        ))}
      </Row>
    </SurfaceFrame>
  );
}
