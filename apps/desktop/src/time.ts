//! Shared timestamp presentation. Every daemon timestamp is an RFC3339
//! string; rendering it verbatim put `open · 2026-08-19T14:03:11.482Z` on
//! every sidebar row. One formatter, used by every surface that shows a
//! moment in time, so the app never mixes raw wire strings with formatted
//! ones. Unparseable input passes through untouched — an honest raw string
//! beats a fabricated date.

/** A wall-clock time from a daemon timestamp, or the raw string if unparseable. */
export function clock(iso: string): string {
  const parsed = Date.parse(iso);
  return Number.isNaN(parsed) ? iso : new Date(parsed).toLocaleTimeString();
}

/**
 * A compact human moment: relative when recent ("3m ago", "2h ago"), a local
 * date once a week old (the hour no longer matters at that distance), a local
 * date + time when in the future (clock skew), the raw string when unparseable.
 */
export function relativeTime(iso: string, now: number = Date.now()): string {
  const parsed = Date.parse(iso);
  if (Number.isNaN(parsed)) {
    return iso;
  }
  const deltaSeconds = Math.round((now - parsed) / 1000);
  if (deltaSeconds < 0) {
    // A future timestamp (clock skew) renders absolutely rather than as a
    // nonsensical negative age.
    return new Date(parsed).toLocaleString();
  }
  if (deltaSeconds < 60) {
    return "just now";
  }
  if (deltaSeconds < 3600) {
    return `${Math.floor(deltaSeconds / 60)}m ago`;
  }
  if (deltaSeconds < 86_400) {
    return `${Math.floor(deltaSeconds / 3600)}h ago`;
  }
  if (deltaSeconds < 7 * 86_400) {
    return `${Math.floor(deltaSeconds / 86_400)}d ago`;
  }
  return new Date(parsed).toLocaleDateString();
}
