/**
 * `@codypendent/protocol` — TypeScript bindings for the Codypendent daemon wire
 * protocol defined in `crates/protocol/src/`.
 *
 * One module here per module there, same names, same field names. See the
 * package README for how these are kept in sync (they are hand-maintained and
 * pinned to the committed golden vectors by a conformance suite, not generated).
 */

export * from "./artifact.js";
export * from "./blackboard.js";
export * from "./capabilities.js";
export * from "./catchup.js";
export * from "./codegraph.js";
export * from "./commands.js";
export * from "./document.js";
export * from "./envelope.js";
export * from "./error.js";
export * from "./events.js";
export * from "./handshake.js";
export * from "./ide.js";
export * from "./ids.js";
export * from "./input.js";
export * from "./memory.js";
export * from "./question.js";
export * from "./run.js";
export * from "./tags.js";
export * from "./version.js";
export * from "./workflow.js";
