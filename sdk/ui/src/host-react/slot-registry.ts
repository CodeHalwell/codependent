import { UI_CONTRIBUTION_POINTS, type UiContributionPoint } from "../protocol.js";

export type WebHostRegion = "sidebar" | "navigation" | "primary" | "transcript" | "composer" | "setup" | "status" | "overlay";
export type WebSlotElement = "aside" | "section" | "footer" | "nav" | "article";

export interface WebSlotDefinition {
  point: UiContributionPoint;
  region: WebHostRegion;
  order: number;
  element: WebSlotElement;
  label: string;
  role?: "status" | "dialog" | "listbox" | "feed" | "form" | "menu";
  ariaLive?: "polite";
  overlay?: boolean;
  focusManaged?: boolean;
}

/**
 * The concrete React host Remote UI slot contract. A point is advertised only when
 * it has a stable region, element, label, focus lifecycle, and ordering here.
 */
export const WEB_SLOT_DEFINITIONS = [
  { point: "sidebar", region: "sidebar", order: 10, element: "aside", label: "Extension sidebar" },
  { point: "command", region: "navigation", order: 20, element: "nav", label: "Extension commands" },
  { point: "panel", region: "primary", order: 30, element: "section", label: "Extension panels" },
  { point: "dashboard-card", region: "primary", order: 40, element: "section", label: "Extension dashboard cards" },
  { point: "workflow-inspector", region: "primary", order: 50, element: "section", label: "Extension workflow nodes" },
  { point: "blackboard-renderer", region: "primary", order: 60, element: "section", label: "Extension blackboard" },
  { point: "code-graph-node", region: "primary", order: 70, element: "section", label: "Extension code graph nodes" },
  { point: "message-renderer", region: "transcript", order: 80, element: "section", label: "Extension transcript entries", role: "feed" },
  { point: "tool-renderer", region: "transcript", order: 90, element: "article", label: "Extension tool results" },
  { point: "artifact-renderer", region: "transcript", order: 100, element: "article", label: "Extension artifact renderers" },
  { point: "document-block", region: "transcript", order: 110, element: "article", label: "Extension document blocks" },
  { point: "trace-span-renderer", region: "transcript", order: 120, element: "article", label: "Extension trace spans" },
  { point: "composer-accessory", region: "composer", order: 130, element: "section", label: "Extension composer accessories" },
  { point: "form", region: "composer", order: 140, element: "section", label: "Extension forms", role: "form" },
  { point: "settings-section", region: "setup", order: 150, element: "section", label: "Extension settings" },
  { point: "setup-step", region: "setup", order: 160, element: "section", label: "Extension setup steps" },
  { point: "status-item", region: "status", order: 170, element: "footer", label: "Extension status items", role: "status", ariaLive: "polite" },
  { point: "command-palette", region: "overlay", order: 180, element: "section", label: "Extension command palette", role: "dialog", overlay: true, focusManaged: true },
  { point: "quick-pick", region: "overlay", order: 190, element: "section", label: "Extension quick picks", role: "listbox", overlay: true, focusManaged: true },
  { point: "context-menu", region: "overlay", order: 200, element: "section", label: "Extension context menu", role: "menu", overlay: true, focusManaged: true },
  { point: "wizard", region: "overlay", order: 210, element: "section", label: "Extension wizards", role: "dialog", overlay: true, focusManaged: true },
  { point: "notification", region: "overlay", order: 220, element: "aside", label: "Extension notifications", ariaLive: "polite", overlay: true },
] as const satisfies readonly WebSlotDefinition[];

const REGISTRY = new Map<string, WebSlotDefinition>(WEB_SLOT_DEFINITIONS.map((slot) => [slot.point, slot]));

if (WEB_SLOT_DEFINITIONS.length !== UI_CONTRIBUTION_POINTS.length
  || UI_CONTRIBUTION_POINTS.some((point) => !REGISTRY.has(point))) {
  throw new Error("Remote UI slot registry is out of sync with the public contract");
}

export const WEB_CONTRIBUTION_POINTS = UI_CONTRIBUTION_POINTS.filter((point) => REGISTRY.has(point));

export function webSlotDefinition(point: unknown): WebSlotDefinition | undefined {
  return typeof point === "string" ? REGISTRY.get(point) : undefined;
}
