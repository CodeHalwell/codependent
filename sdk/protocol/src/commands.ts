/** Stable public facade for schema-generated command contracts. */

export type { Command, CommandBody, PromotionAction } from "./generated/commands.js";
export type { FileMatchWire, UiPluginLifecycleStatus } from "./generated/payload.js";

import type { CommandBody } from "./generated/commands.js";

/** Every `CommandBody` tag this build knows. Kept exhaustive by `tags.ts`. */
export type CommandBodyTag = CommandBody["type"];
