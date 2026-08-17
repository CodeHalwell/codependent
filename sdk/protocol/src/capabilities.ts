/** Mirrors `crates/protocol/src/capabilities.rs`. */

/**
 * Legacy fields are always present. Additive platform fields default to false
 * and may be omitted by older peers.
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
  session_library?: boolean;
  editor_actions?: boolean;
  inbox?: boolean;
  analytics?: boolean;
  automation?: boolean;
  bundles?: boolean;
}
