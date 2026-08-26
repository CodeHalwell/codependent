/**
 * One formatter for every moment on screen.
 *
 * Sidebar rows used to read `open · 2026-08-19T14:03:11.482Z` — the raw wire
 * string. `relativeTime` is what those sites render now; the unparseable
 * pass-through is the honesty property (a raw string beats a fabricated
 * date).
 */
import { describe, expect, it } from "vitest";

import { clock, relativeTime } from "../src/time.js";

const NOW = Date.parse("2026-08-26T12:00:00Z");

describe("relativeTime", () => {
  it("renders recent moments as ages", () => {
    expect(relativeTime("2026-08-26T11:59:40Z", NOW)).toBe("just now");
    expect(relativeTime("2026-08-26T11:45:00Z", NOW)).toBe("15m ago");
    expect(relativeTime("2026-08-26T09:00:00Z", NOW)).toBe("3h ago");
    expect(relativeTime("2026-08-24T12:00:00Z", NOW)).toBe("2d ago");
  });

  it("renders old and future moments absolutely, never as nonsense ages", () => {
    // A week or more out: a date, not "9d ago" arithmetic drift.
    expect(relativeTime("2026-08-01T12:00:00Z", NOW)).not.toContain("ago");
    // Clock skew: a future timestamp must not be a negative age.
    expect(relativeTime("2026-08-27T12:00:00Z", NOW)).not.toContain("ago");
  });

  it("passes an unparseable string through untouched", () => {
    expect(relativeTime("not a timestamp", NOW)).toBe("not a timestamp");
    expect(clock("not a timestamp")).toBe("not a timestamp");
  });
});
