/** Mirrors `crates/protocol/src/model.rs`. */

import type { CodypendentError } from "./error.js";
import type { ModelId } from "./ids.js";

/**
 * What the daemon can say about a model without running it.
 *
 * `#[serde(other)] Unknown` on the Rust side: a client built against an older
 * protocol folds a verdict it has never heard of into `Unknown` rather than
 * failing to parse the whole reply.
 */
export type ModelReadiness =
  | { state: "ready"; detail: string }
  | { state: "unverified"; detail: string }
  /**
   * `error` is the classified cause, when the daemon has one — the same
   * `CodypendentError` a failed run carries, with the same `user_action` to
   * turn into an affordance. `skip_serializing_if` — absent, never `null`.
   */
  | { state: "unavailable"; detail: string; error?: CodypendentError }
  | { state: "unknown" };

/** One configured model's readiness, as the daemon sees it. */
export interface ModelProbe {
  /** The `models.toml` id, not the provider's model name. */
  id: ModelId;
  readiness: ModelReadiness;
  /** Whether the network was actually used to reach this verdict. */
  probed: boolean;
}
