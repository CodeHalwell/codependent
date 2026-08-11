import {
  DEFAULT_UI_HARD_LIMITS,
  UI_PRIMITIVES,
  UI_HOST_CAPABILITIES,
  UI_PROTOCOL_VERSION,
  type UiCapabilities,
  type UiContributionPoint,
  type UiPrimitive,
  type UiViewport,
} from "@codypendent/ui";
import { WEB_CONTRIBUTION_POINTS, webSlotDefinition } from "./slot-registry.js";

/** Every semantic primitive implemented by the VS Code webview renderer. */
export const WEB_PRIMITIVES: readonly UiPrimitive[] = [
  ...UI_PRIMITIVES.filter((primitive) => primitive !== "ApprovalCard" && primitive !== "PermissionDiff"),
];

/** Validate untrusted extension→webview placement ingress against exactly the
 * points this renderer advertises. */
export function supportedContributionPoint(value: unknown): UiContributionPoint | undefined {
  return webSlotDefinition(value)?.point;
}

function mediaMatches(query: string, fallback = false): boolean {
  return typeof window === "undefined" || typeof window.matchMedia !== "function"
    ? fallback
    : window.matchMedia(query).matches;
}

export function viewportFromWindow(): UiViewport {
  if (typeof window === "undefined") return { width: 1024, height: 768, density: 1 };
  return {
    width: Math.max(1, Math.round(window.innerWidth)),
    height: Math.max(1, Math.round(window.innerHeight)),
    pixelWidth: Math.max(1, Math.round(window.innerWidth * window.devicePixelRatio)),
    pixelHeight: Math.max(1, Math.round(window.innerHeight * window.devicePixelRatio)),
    density: window.devicePixelRatio,
  };
}

/** Advertised at startup and whenever the webview viewport changes. */
export function createWebviewCapabilities(viewport = viewportFromWindow()): UiCapabilities {
  const monochrome = mediaMatches("(forced-colors: active)");
  return {
    client: "vscode",
    protocolVersions: [UI_PROTOCOL_VERSION],
    daemon: {
      rich_text: true,
      image_display: true,
      audio_capture: false,
      editor_mutations: true,
      diff_view: true,
      mouse: true,
      unicode: true,
      true_color: !monochrome,
    },
    primitives: WEB_PRIMITIVES,
    media: ["image", "audio"],
    colorDepth: monochrome ? "monochrome" : "trueColor",
    keyboard: true,
    // The DOM is fully labelled and screen-reader compatible. Browser APIs do
    // not expose whether assistive technology is currently running.
    screenReader: true,
    reducedMotion: mediaMatches("(prefers-reduced-motion: reduce)"),
    clipboard: typeof navigator === "undefined" || navigator.clipboard !== undefined,
    capabilities: UI_HOST_CAPABILITIES,
    contributionPoints: [...WEB_CONTRIBUTION_POINTS],
    limits: {
      ...DEFAULT_UI_HARD_LIMITS,
      maxNodes: Math.min(DEFAULT_UI_HARD_LIMITS.maxNodes, 10_000),
      maxPatchesPerBatch: Math.min(DEFAULT_UI_HARD_LIMITS.maxPatchesPerBatch, 256),
      maxPatchBytes: Math.min(DEFAULT_UI_HARD_LIMITS.maxPatchBytes, 2 * 1024 * 1024),
    },
    viewport,
  };
}
