/** Stable public facade for schema-generated envelope and payload contracts. */

export type { Envelope } from "./generated/envelope.js";
export type { Payload } from "./generated/payload.js";

import type { Payload } from "./generated/payload.js";

/** The status fields flattened beside `type: "DaemonStatusResponse"`. */
export type DaemonStatus = Omit<Extract<Payload, { type: "DaemonStatusResponse" }>, "type">;

/** Every `Payload` tag this build knows. Kept exhaustive by `tags.ts`. */
export type PayloadTag = Payload["type"];
