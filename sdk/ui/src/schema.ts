/** Stable identifier for the checked-in canonical JSON Schema. */
export const REMOTE_UI_SCHEMA_ID = "https://codypendent.dev/schema/remote-ui/v1.json";

/** Resolves in Node, browsers, and bundlers without reading the filesystem. */
export const REMOTE_UI_SCHEMA_URL = new URL("../schema/remote-ui.schema.json", import.meta.url);
