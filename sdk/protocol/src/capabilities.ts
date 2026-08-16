/** Mirrors `crates/protocol/src/capabilities.rs`. */

/**
 * `#[serde(default)]` on the struct, with no per-field skip: every field is
 * always present on the wire.
 */
export interface ClientCapabilities {
  rich_text: boolean;
  image_display: boolean;
  audio_capture: boolean;
  editor_mutations: boolean;
  diff_view: boolean;
  mouse: boolean;
  unicode: boolean;
  true_color: boolean;
}
