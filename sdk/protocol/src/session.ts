/** Stable public facade for schema-generated session platform contracts. */

export type * from "./generated/session.js";

/** Opaque server-issued cursor. Clients must store and replay it unchanged. */
export type PageCursor = string;
