/**
 * LEGACY hand-written mirror. Do not add types here.
 *
 * The authoritative contract is generated from `crates/control-plane-protocol` into
 * `../generated/` (`npm run generate`, gated by `npm run check:generated`). These modules
 * predate that pipeline and disagree with it in a way that matters: they name fields in
 * camelCase while the Rust protocol and the running server both serialize snake_case, so a
 * response typed by this module does not describe the bytes that arrive.
 *
 * They survive only because `sdk/control-plane-react` compiles against these names and is
 * owned elsewhere. Anything new belongs in the generated modules; anything here should be
 * deleted in the same change that migrates the React bindings.
 */
export * from "./common.js";
export * from "./auth.js";
export * from "./organization.js";
export * from "./team.js";
export * from "./repository.js";
export * from "./rbac.js";
export * from "./daemon.js";
export * from "./session.js";
export * from "./inbox.js";
export * from "./approval.js";
export * from "./audit.js";
export * from "./storage.js";
export * from "./stream.js";
