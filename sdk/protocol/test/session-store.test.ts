import { describe, expect, it, vi } from "vitest";

import type { Catchup, SessionEvent, SessionProjection } from "../src/index.js";
import { MAX_RETAINED_EVENTS, SessionSequenceGapError, SessionStore } from "../src/index.js";

function event(sequence: number): SessionEvent {
  return {
    sequence,
    occurred_at: "2026-08-17T00:00:00Z",
    actor: { type: "System" },
    body: { type: "NoteAppended", text: `event ${sequence}` },
  };
}

function projection(last_sequence: number): SessionProjection {
  return {
    session_id: "00000000-0000-0000-0000-000000000001",
    title: "Session",
    last_sequence,
    closed: false,
  };
}

describe("SessionStore", () => {
  it("publishes an event catch-up as one coherent ready update", () => {
    const store = new SessionStore();
    const listener = vi.fn();
    store.subscribe(listener);

    store.applyCatchup({ type: "Events", from: 1, through: 2, events: [event(1), event(2)] });

    expect(listener).toHaveBeenCalledTimes(1);
    expect(store.getSnapshot()).toMatchObject({ ready: true, cursor: 2, events: [event(1), event(2)] });
  });

  it("uses a projection snapshot as the catch-up baseline", () => {
    const store = new SessionStore();
    store.applyCatchup({ type: "Snapshot", through: 500, projection: projection(500) });

    expect(store.getSnapshot()).toEqual({
      ready: true,
      cursor: 500,
      projection: projection(500),
      events: [],
      droppedEvents: 0,
    });
  });

  it("buffers live events until catch-up and reconciles overlap in sequence order", () => {
    const store = new SessionStore();
    const listener = vi.fn();
    store.subscribe(listener);
    store.applyEvent(event(4));
    store.applyEvent(event(3));
    store.applyEvent(event(3));

    const catchup: Catchup = { type: "Events", from: 1, through: 3, events: [event(1), event(2), event(3)] };
    store.applyCatchup(catchup);

    expect(listener).toHaveBeenCalledTimes(1);
    expect(store.getSnapshot().events.map(({ sequence }) => sequence)).toEqual([1, 2, 3, 4]);
    expect(store.getSnapshot()).toMatchObject({ ready: true, cursor: 4 });
  });

  it("deduplicates live events at or behind the cursor without notifying", () => {
    const store = new SessionStore();
    store.applyCatchup({ type: "Events", from: 1, through: 1, events: [event(1)] });
    const listener = vi.fn();
    store.subscribe(listener);

    store.applyEvent(event(1));

    expect(listener).not.toHaveBeenCalled();
    expect(store.getSnapshot().events).toHaveLength(1);
  });

  it("retains accumulated history across a reconnect catch-up", () => {
    const store = new SessionStore();
    store.applyCatchup({ type: "Events", from: 1, through: 2, events: [event(1), event(2)] });
    store.applyEvent(event(3));

    store.applyCatchup({ type: "Events", from: 3, through: 4, events: [event(3), event(4)] });

    expect(store.getSnapshot().events.map(({ sequence }) => sequence)).toEqual([1, 2, 3, 4]);
    expect(store.getSnapshot().cursor).toBe(4);
  });

  it("rejects sequence gaps in catch-up and live delivery", () => {
    const catchingUp = new SessionStore();
    expect(() =>
      catchingUp.applyCatchup({ type: "Events", from: 1, through: 3, events: [event(1), event(3)] }),
    ).toThrow(SessionSequenceGapError);

    const live = new SessionStore();
    live.applyCatchup({ type: "Events", from: 1, through: 1, events: [event(1)] });
    expect(() => live.applyEvent(event(3))).toThrow("expected sequence 2, received 3");
  });

  it("resets state and discards buffered live events", () => {
    const store = new SessionStore();
    store.applyEvent(event(2));
    const listener = vi.fn();
    store.subscribe(listener);

    store.reset();
    store.applyCatchup({ type: "Events", from: 1, through: 1, events: [event(1)] });

    expect(listener).toHaveBeenCalledTimes(2);
    expect(store.getSnapshot()).toMatchObject({ ready: true, cursor: 1, events: [event(1)] });
  });
});

describe("SessionStore retention", () => {
  /**
   * The store appended every event forever, and did it by copying the whole
   * array each time — quadratic in the length of the session, on the hot path
   * of a live stream, for a history nothing was reading. A long run paid
   * steadily more per event and held all of it in memory.
   */
  it("retains a bounded tail rather than the whole session", () => {
    const store = new SessionStore();
    store.applyCatchup({ type: "Events", from: 1, through: 1, events: [event(1)] });

    const total = MAX_RETAINED_EVENTS + 500;
    for (let sequence = 2; sequence <= total; sequence += 1) {
      store.applyEvent(event(sequence));
    }

    const snapshot = store.getSnapshot();
    expect(snapshot.events).toHaveLength(MAX_RETAINED_EVENTS);
    expect(snapshot.droppedEvents).toBe(total - MAX_RETAINED_EVENTS);
    // The cursor stays the real watermark, and the tail is the newest end.
    expect(snapshot.cursor).toBe(total);
    expect(snapshot.events[snapshot.events.length - 1]?.sequence).toBe(total);
    expect(snapshot.events[0]?.sequence).toBe(total - MAX_RETAINED_EVENTS + 1);
  });

  it("drops nothing, and says so, for a session inside the bound", () => {
    const store = new SessionStore();
    store.applyCatchup({ type: "Events", from: 1, through: 2, events: [event(1), event(2)] });
    store.applyEvent(event(3));

    expect(store.getSnapshot().events).toHaveLength(3);
    expect(store.getSnapshot().droppedEvents).toBe(0);
  });

  it("truncates an oversized catch-up too, and counts what it cut", () => {
    const store = new SessionStore();
    const total = MAX_RETAINED_EVENTS + 10;
    const events = Array.from({ length: total }, (_, index) => event(index + 1));

    store.applyCatchup({ type: "Events", from: 1, through: total, events });

    const snapshot = store.getSnapshot();
    expect(snapshot.events).toHaveLength(MAX_RETAINED_EVENTS);
    expect(snapshot.droppedEvents).toBe(10);
    expect(snapshot.cursor).toBe(total);
  });
});
