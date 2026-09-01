/**
 * The words the run-feedback strip and the transcript's working row use, and
 * the usage chip — ported from the TUI so both clients describe a run the same
 * way (`crates/tui/src/render.rs`: `activity_status_line`, `usage_label`,
 * `format_cost_micros`, `thousands`).
 */
import type { RunActivity, RunUsage } from "./types.js";

/** `1234` → `1,234`. Magnitude is the message for a token count. */
export function thousands(value: number): string {
  return Math.trunc(value).toLocaleString("en-US");
}

/**
 * A MEASURED cost, in USD millionths. Four decimals: a run against a cheap
 * model costs a fraction of a cent, and two decimals would report almost every
 * one of them as `$0.00`.
 */
export function formatCostMicros(micros: number): string {
  const whole = Math.trunc(micros / 1_000_000);
  const fraction = Math.trunc((micros % 1_000_000) / 100);
  return `$${whole}.${fraction.toString().padStart(4, "0")}`;
}

/**
 * The measured-usage chip: `1,234 in · 567 out · $0.0034`.
 *
 * Only measured dimensions appear. An absent one means the provider did not
 * report it, never zero — a run with tokens and no price (every unpriced local
 * model) shows its tokens and no money rather than a `$0.00` that would read
 * as "this run was free". `null` when nothing at all was measured.
 */
export function usageLabel(usage: RunUsage | null): string | null {
  if (!usage) {
    return null;
  }
  const parts: string[] = [];
  if (usage.promptTokens !== null) {
    parts.push(`${thousands(usage.promptTokens)} in`);
  }
  if (usage.completionTokens !== null) {
    parts.push(`${thousands(usage.completionTokens)} out`);
  }
  if (usage.costMicros !== null) {
    parts.push(formatCostMicros(usage.costMicros));
  }
  return parts.length > 0 ? parts.join(" · ") : null;
}

/**
 * The one-line description of what the run is doing, or `null` when there is
 * nothing to say (`idle`, and `streaming` — the growing reply is the signal).
 */
export function describeActivity(activity: RunActivity): string | null {
  switch (activity.kind) {
    case "idle":
    case "streaming":
      return null;
    case "thinking":
      return "working…";
    case "tool":
      return `running ${activity.tool}…`;
    case "waiting":
      return activity.on === "approval" ? "waiting for your approval" : "waiting for your answer";
    case "retrying": {
      const seconds = Math.max(1, Math.round(activity.delayMs / 1000));
      return `retrying (${activity.attempt}/${activity.maxAttempts}) · ${activity.message} · next attempt in ${seconds}s`;
    }
  }
}
