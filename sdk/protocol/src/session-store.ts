import type { Catchup, SessionProjection } from "./catchup.js";
import type { SessionEvent } from "./generated/events.js";

export interface SessionStoreSnapshot {
  readonly ready: boolean;
  readonly cursor: number;
  readonly projection?: SessionProjection;
  readonly events: readonly SessionEvent[];
}

export type SessionStoreListener = () => void;

export class SessionSequenceGapError extends Error {
  readonly expected: number;
  readonly received: number;

  constructor(expected: number, received: number) {
    super(`session event gap: expected sequence ${expected}, received ${received}`);
    this.name = "SessionSequenceGapError";
    this.expected = expected;
    this.received = received;
  }
}

const EMPTY_SNAPSHOT: SessionStoreSnapshot = {
  ready: false,
  cursor: 0,
  events: [],
};

/**
 * Host-neutral attach/catch-up/live event store.
 *
 * Live events received before the attach catch-up are held back so consumers
 * never observe a live-ready state with missing history.
 */
export class SessionStore {
  private snapshot: SessionStoreSnapshot = EMPTY_SNAPSHOT;
  private buffered = new Map<number, SessionEvent>();
  private readonly listeners = new Set<SessionStoreListener>();

  getSnapshot = (): SessionStoreSnapshot => this.snapshot;

  subscribe = (listener: SessionStoreListener): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  applyEvent(event: SessionEvent): void {
    if (!this.snapshot.ready) {
      this.buffered.set(event.sequence, event);
      return;
    }

    if (event.sequence <= this.snapshot.cursor) return;
    this.assertNext(this.snapshot.cursor, event.sequence);
    this.snapshot = {
      ...this.snapshot,
      cursor: event.sequence,
      events: [...this.snapshot.events, event],
    };
    this.emit();
  }

  applyCatchup(catchup: Catchup): void {
    if (catchup.type === "Unknown") {
      throw new Error("cannot initialize a session store from an unknown catch-up");
    }

    let cursor: number;
    let projection: SessionProjection | undefined;
    let events: SessionEvent[];

    if (catchup.type === "Snapshot") {
      if (catchup.projection.last_sequence !== catchup.through) {
        throw new Error(
          `snapshot watermark mismatch: projection is at ${catchup.projection.last_sequence}, catch-up is through ${catchup.through}`,
        );
      }
      cursor = catchup.through;
      projection = catchup.projection;
      events = [];
    } else {
      const expectedBaseline = catchup.from - 1;
      if (this.snapshot.ready && this.snapshot.cursor < expectedBaseline) {
        throw new SessionSequenceGapError(this.snapshot.cursor + 1, catchup.from);
      }
      cursor = this.snapshot.ready ? this.snapshot.cursor : expectedBaseline;
      projection = this.snapshot.projection;
      events = this.snapshot.ready ? [...this.snapshot.events] : [];
      for (const event of catchup.events) {
        if (event.sequence <= cursor) continue;
        this.assertNext(cursor, event.sequence);
        events.push(event);
        cursor = event.sequence;
      }
      if (cursor !== catchup.through) {
        throw new Error(`catch-up watermark mismatch: events end at ${cursor}, catch-up is through ${catchup.through}`);
      }
    }

    for (const event of [...this.buffered.values()].sort((a, b) => a.sequence - b.sequence)) {
      if (event.sequence <= cursor) continue;
      this.assertNext(cursor, event.sequence);
      events.push(event);
      cursor = event.sequence;
    }

    this.buffered.clear();
    this.snapshot = {
      ready: true,
      cursor,
      ...(projection === undefined ? {} : { projection }),
      events,
    };
    this.emit();
  }

  reset(): void {
    this.buffered.clear();
    this.snapshot = EMPTY_SNAPSHOT;
    this.emit();
  }

  private assertNext(cursor: number, sequence: number): void {
    const expected = cursor + 1;
    if (sequence > expected) throw new SessionSequenceGapError(expected, sequence);
  }

  private emit(): void {
    for (const listener of this.listeners) listener();
  }
}
