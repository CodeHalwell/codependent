import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import {
  DEFAULT_UI_HARD_LIMITS,
  MINIMAL_TERMINAL_CAPABILITIES,
  UI_CONTRIBUTION_POINTS,
  UI_EVENT_TYPES,
  UI_HOST_CAPABILITIES,
  UI_PRIMITIVES,
  UI_PROTOCOL_VERSION,
  type ColorDepth,
  type UiCapabilities,
  type UiContributionRegistration,
  type UiDocument,
  type UiJsonValue,
  type UiNode,
  type UiTheme,
  type UiWireMessage,
} from "../protocol.js";
import type { DevelopmentDiagnostic, WorkbenchTarget } from "./diagnostics.js";
import { jsonRecord } from "./diagnostics.js";
import type { UiWorkbenchStory, WorkbenchActionFixture, WorkbenchEventFixture, WorkbenchProjectionFixture } from "./stories.js";

export interface WorkbenchOptions {
  target: WorkbenchTarget;
  point: string;
  viewport: { width: number; height: number };
  colorDepth: ColorDepth;
  theme: UiTheme;
  fixture?: UiWorkbenchStory;
}

export interface WorkbenchTraceEntry {
  direction: "host→worker" | "worker→host";
  type: string;
  detail: string;
}

export interface WorkbenchReport {
  target: WorkbenchTarget;
  point: string;
  documents: UiDocument[];
  contributions: UiContributionRegistration[];
  diagnostics: DevelopmentDiagnostic[];
  trace: WorkbenchTraceEntry[];
  subscriptions: string[];
  actions: string[];
  patches: string[];
  events: string[];
  hotReloadState: Readonly<Record<string, UiJsonValue>>;
}

export const DEFAULT_WORKBENCH_OPTIONS: WorkbenchOptions = {
  target: "terminal",
  point: "panel",
  viewport: { width: 80, height: 24 },
  colorDepth: "trueColor",
  theme: {
    id: "workbench.dark",
    name: "Workbench Dark",
    revision: 1,
    colorScheme: "dark",
    highContrast: false,
    reducedMotion: false,
    tokens: {
      "surface.background": "#0b0d12",
      "surface.panel": "#11141b",
      "surface.border": "#2a2f3a",
      "text.primary": "#e8ecf4",
      "text.muted": "#707889",
      "status.error": "#fb7185",
      "spacing.sm": 4,
      "spacing.md": 8,
    },
  },
};

export function createWorkbenchCapabilities(options: WorkbenchOptions): UiCapabilities {
  const terminal = options.target === "terminal";
  const graphical = options.target === "vscode" || options.target === "web" || options.target === "test";
  return {
    ...MINIMAL_TERMINAL_CAPABILITIES,
    client: options.target === "web" ? "web" : options.target,
    protocolVersions: [UI_PROTOCOL_VERSION],
    primitives: UI_PRIMITIVES.filter((primitive) => !["ApprovalCard", "PermissionDiff"].includes(primitive)),
    media: graphical ? ["image", "audio", "video"] : [],
    colorDepth: options.colorDepth,
    keyboard: true,
    screenReader: graphical,
    reducedMotion: options.theme.reducedMotion ?? false,
    clipboard: true,
    ...(terminal ? { terminalGraphics: [] } : {}),
    capabilities: UI_HOST_CAPABILITIES,
    contributionPoints: (UI_CONTRIBUTION_POINTS as readonly string[]).includes(options.point)
      ? [options.point]
      : [],
    viewport: { ...options.viewport, ...(graphical ? { pixelWidth: options.viewport.width, pixelHeight: options.viewport.height, density: 1 } : {}) },
    limits: DEFAULT_UI_HARD_LIMITS,
    daemon: {
      rich_text: true,
      image_display: graphical,
      audio_capture: false,
      editor_mutations: options.target === "vscode",
      diff_view: true,
      mouse: true,
      unicode: true,
      true_color: options.colorDepth === "trueColor",
    },
  };
}

function record(value: unknown, name: string): Readonly<Record<string, unknown>> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error(`${name} must be an object`);
  return value as Readonly<Record<string, unknown>>;
}

/** Load author-controlled projection/action data without executing fixture code. */
export async function loadWorkbenchFixture(path: string): Promise<UiWorkbenchStory> {
  const source = await readFile(resolve(path), "utf8");
  if (Buffer.byteLength(source, "utf8") > 4 * 1024 * 1024) throw new Error("workbench fixture exceeds 4 MiB");
  const value = record(JSON.parse(source) as unknown, "workbench fixture");
  const id = typeof value.id === "string" ? value.id : "workbench.fixture";
  const title = typeof value.title === "string" ? value.title : id;
  const point = typeof value.point === "string" ? value.point : "panel";
  const renderer = typeof value.renderer === "string" ? value.renderer : id;
  const target = typeof value.target === "string" && ["terminal", "vscode", "web", "test"].includes(value.target)
    ? value.target as WorkbenchTarget
    : "terminal";
  const projections = jsonRecord(value.projections) as Readonly<Record<string, WorkbenchProjectionFixture>> | undefined;
  const actions = jsonRecord(value.actions) as Readonly<Record<string, WorkbenchActionFixture>> | undefined;
  const hotReloadState = jsonRecord(value.hotReloadState);
  const events = value.events === undefined
    ? undefined
    : Array.isArray(value.events)
      ? value.events.map((raw, index): WorkbenchEventFixture => {
          const event = record(raw, `workbench fixture events[${index}]`);
          if (typeof event.targetId !== "string" || event.targetId.length === 0) throw new Error(`workbench fixture events[${index}].targetId must be a non-empty string`);
          if (typeof event.type !== "string" || !(UI_EVENT_TYPES as readonly string[]).includes(event.type)) throw new Error(`workbench fixture events[${index}].type is unknown`);
          if (event.documentId !== undefined && typeof event.documentId !== "string") throw new Error(`workbench fixture events[${index}].documentId must be a string`);
          if (event.revision !== undefined && (!Number.isSafeInteger(event.revision) || (event.revision as number) < 0)) throw new Error(`workbench fixture events[${index}].revision must be a non-negative integer`);
          return {
            targetId: event.targetId,
            type: event.type as WorkbenchEventFixture["type"],
            ...(typeof event.documentId === "string" ? { documentId: event.documentId } : {}),
            ...(typeof event.revision === "number" ? { revision: event.revision } : {}),
            ...(event.payload === undefined ? {} : { payload: event.payload as UiJsonValue }),
            ...(event.modifiers === undefined ? {} : { modifiers: record(event.modifiers, `workbench fixture events[${index}].modifiers`) as NonNullable<WorkbenchEventFixture["modifiers"]> }),
          };
        })
      : (() => { throw new Error("workbench fixture events must be an array"); })();
  return {
    id, title, point, renderer, target,
    ...(projections === undefined ? {} : { projections }),
    ...(actions === undefined ? {} : { actions }),
    ...(events === undefined ? {} : { events }),
    ...(hotReloadState === undefined ? {} : { hotReloadState }),
  };
}

export function projectionFixture(
  story: UiWorkbenchStory | undefined,
  message: Extract<UiWireMessage, { type: "subscription" }>,
): WorkbenchProjectionFixture | undefined {
  const resource = message.subscription.resourceId ?? "";
  return story?.projections?.[`${message.subscription.kind}:${resource}`]
    ?? story?.projections?.[message.subscription.kind];
}

export function actionFixture(
  story: UiWorkbenchStory | undefined,
  message: Extract<UiWireMessage, { type: "action" }>,
): WorkbenchActionFixture | undefined {
  return story?.actions?.[message.action.actionId];
}

export function formatNodeTree(node: UiNode, indent = ""): string[] {
  if (node.kind === "text") return [`${indent}text#${node.id ?? "?"} ${JSON.stringify(node.text)}`];
  const props = Object.keys(node.props).length === 0 ? "" : ` props=${JSON.stringify(node.props)}`;
  const requires = node.requires === undefined || node.requires.length === 0 ? "" : ` requires=${JSON.stringify(node.requires)}`;
  const lines = [`${indent}${node.type}#${node.id ?? "?"}${props}${requires}`];
  node.children.forEach((child) => lines.push(...formatNodeTree(child, `${indent}  `)));
  if (node.fallback !== undefined) {
    lines.push(`${indent}  fallback:`);
    lines.push(...formatNodeTree(node.fallback, `${indent}    `));
  }
  return lines;
}

export function colorDepth(value: string | undefined): ColorDepth {
  if (value === undefined) return DEFAULT_WORKBENCH_OPTIONS.colorDepth;
  if (["monochrome", "ansi16", "ansi256", "trueColor"].includes(value)) return value as ColorDepth;
  throw new Error("--color-depth must be monochrome, ansi16, ansi256, or trueColor");
}

export function viewport(value: string | undefined): { width: number; height: number } {
  if (value === undefined) return DEFAULT_WORKBENCH_OPTIONS.viewport;
  const match = /^(\d+)x(\d+)$/u.exec(value);
  if (match === null) throw new Error("--viewport must use WIDTHxHEIGHT, for example 80x24");
  const width = Number.parseInt(match[1] as string, 10);
  const height = Number.parseInt(match[2] as string, 10);
  if (width < 12 || height < 5 || width > 16_384 || height > 16_384) throw new Error("--viewport is outside the supported workbench range");
  return { width, height };
}

export function workbenchTheme(name: string | undefined): UiTheme {
  const selected = name ?? "dark";
  if (!["dark", "light", "highContrast", "monochrome"].includes(selected)) {
    throw new Error("--theme must be dark, light, highContrast, or monochrome");
  }
  return {
    ...DEFAULT_WORKBENCH_OPTIONS.theme,
    id: `workbench.${selected}`,
    name: `Workbench ${selected}`,
    revision: 1,
    ...(selected === "dark" || selected === "light" ? { colorScheme: selected } : { colorScheme: "dark" }),
    highContrast: selected === "highContrast",
    ...(selected === "monochrome" ? { tokens: {} } : DEFAULT_WORKBENCH_OPTIONS.theme.tokens === undefined
      ? {}
      : { tokens: DEFAULT_WORKBENCH_OPTIONS.theme.tokens }),
  };
}
