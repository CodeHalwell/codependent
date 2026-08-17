export * from "./types/index.js";
export * from "./errors.js";
export * from "./client.js";
export * from "./stream.js";
export * from "./utils/index.js";

/**
 * The authoritative wire contract, generated from `crates/control-plane-protocol` — see
 * `src/generated/`, and `@codypendent/control-plane/wire` for a direct import.
 *
 * It is namespaced rather than re-exported flat because the legacy hand-written types in
 * `./types` still occupy the same names at the top level (`Organization`, `Repository`,
 * `AuditRecord`, …) with camelCase fields, while the server and the Rust protocol both
 * speak snake_case. Where the two disagree, `wire` is the one that matches the bytes.
 */
export type * as wire from "./generated/index.js";
