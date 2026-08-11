import { UI_PROTOCOL_VERSION, type UiDocument, type UiEventModifiers, type UiEventType, type UiJsonValue } from "../protocol.js";
import type { ThemeTokens } from "../session.js";
import type { WorkbenchTarget } from "./diagnostics.js";

export interface WorkbenchProjectionFixture {
  value?: UiJsonValue;
  removed?: boolean;
  revision?: number;
}

export interface WorkbenchActionFixture {
  status: "succeeded" | "failed" | "cancelled";
  value?: UiJsonValue;
  error?: { code: string; message: string; recovery?: string };
}

export interface WorkbenchEventFixture {
  documentId?: string;
  revision?: number;
  targetId: string;
  type: UiEventType;
  payload?: UiJsonValue;
  modifiers?: UiEventModifiers;
}

export interface UiWorkbenchStory {
  id: string;
  title: string;
  point: string;
  renderer: string;
  target: WorkbenchTarget;
  document?: UiDocument;
  projections?: Readonly<Record<string, WorkbenchProjectionFixture>>;
  actions?: Readonly<Record<string, WorkbenchActionFixture>>;
  events?: readonly WorkbenchEventFixture[];
  themes?: readonly ThemeTokens[];
  viewports?: readonly { width: number; height: number }[];
  hotReloadState?: Readonly<Record<string, UiJsonValue>>;
}

export const WORKBENCH_THEMES: readonly ThemeTokens[] = [
  { id: "dark", mode: "dark", colors: { "text.primary": "#e8ecf4", "surface.background": "#0b0d12", "status.error": "#fb7185" }, spacing: { sm: 1, md: 2 } },
  { id: "light", mode: "light", colors: { "text.primary": "#172033", "surface.background": "#ffffff", "status.error": "#b42318" }, spacing: { sm: 1, md: 2 } },
  { id: "high-contrast", mode: "highContrast", colors: { "text.primary": "#ffffff", "surface.background": "#000000", "status.error": "#ffff00" }, spacing: { sm: 1, md: 2 } },
  { id: "monochrome", mode: "monochrome", colors: {}, spacing: { sm: 1, md: 2 } },
];

/** Shared structural story consumed by SDK and graphical-host conformance tests. */
export const UI_CONFORMANCE_STORY: UiWorkbenchStory = {
  id: "conformance.surface-states",
  title: "Remote UI Surface States",
  point: "panel",
  renderer: "codypendent.conformance",
  target: "vscode",
  document: {
    protocolVersion: UI_PROTOCOL_VERSION,
    documentId: "conformance-surface-states",
    revision: 1,
    root: {
      kind: "element", id: "root", type: "Stack",
      props: { gap: "md", accessibleLabel: "Remote UI surface states" },
      children: [
        { kind: "element", id: "heading", type: "Text", props: { value: "Surface states", role: "heading", weight: "bold", accessibleLabel: "Surface states" }, children: [] },
        { kind: "element", id: "loading", type: "Spinner", props: { label: "Loading…", accessibleLabel: "Loading" }, children: [] },
        { kind: "element", id: "empty", type: "EmptyState", props: { title: "No Results", message: "Change the filters to see results.", accessibleLabel: "No results" }, children: [] },
        { kind: "element", id: "error", type: "Alert", props: { tone: "critical", title: "Could Not Load", message: "Retry the request or disable this surface.", accessibleLabel: "Could not load" }, children: [] },
        { kind: "element", id: "long", type: "Text", props: { value: "A-very-long-unbroken-extension-value-that-must-wrap-without-overflow-or-hiding-recovery-controls", accessibleLabel: "Long content" }, children: [] },
        { kind: "element", id: "retry", type: "Button", props: { action: "conformance.retry", label: "Retry Surface", accessibleLabel: "Retry surface" }, children: [] },
      ],
    },
    metadata: { title: "Conformance Story", source: "shared-story" },
  },
  actions: { "conformance.retry": { status: "succeeded", value: { retried: true } } },
  themes: WORKBENCH_THEMES,
  viewports: [{ width: 40, height: 12 }, { width: 80, height: 24 }, { width: 120, height: 40 }],
  hotReloadState: { count: 3 },
};
