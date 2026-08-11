import { auditAccessibility } from "../accessibility.js";
import { UI_PRIMITIVES, type UiDocument, type UiJsonValue, type UiNode } from "../protocol.js";
import { validateDocument } from "../validation.js";

export type WorkbenchTarget = "terminal" | "vscode" | "web" | "test";
export type DevelopmentDiagnosticKind =
  | "validation" | "accessibility" | "layout" | "theme" | "fallback" | "primitive";

export interface DevelopmentDiagnostic {
  kind: DevelopmentDiagnosticKind;
  severity: "error" | "warning" | "info";
  code: string;
  path: string;
  message: string;
  suggestion?: string;
}

const SIZE_TOKENS = new Set(["xs", "sm", "md", "lg", "xl"]);
const THEME_TOKENS = new Set([
  "surface.background", "surface.panel", "surface.border", "surface.overlay", "surface.user",
  "text.primary", "text.secondary", "text.muted", "text.heading",
  "status.info", "status.success", "status.warning", "status.error", "status.running", "status.idle",
  "syntax.keyword", "syntax.literal", "syntax.string", "syntax.comment", "syntax.type", "syntax.function",
  "syntax.operator", "syntax.constant", "syntax.punctuation",
  "diff.added", "diff.removed", "diff.context", "diff.header",
  "agent.modelText", "agent.tool", "agent.thinking", "focus.active", "focus.inactive",
  "selection.foreground", "selection.background",
]);
const WEB_DIMENSION = /^(?:auto|0|[0-9]+(?:\.[0-9]+)?(?:px|%|rem|em|ch|vh|vw))$/u;
const TERMINAL_DIMENSION = /^(?:auto|full|0|[0-9]+(?:\.[0-9]+)?(?:%|fr)?)$/u;

function elementChildren(node: UiNode): readonly UiNode[] {
  return node.kind === "element"
    ? [...node.children, ...(node.fallback === undefined ? [] : [node.fallback])]
    : [];
}

function pathFor(node: UiNode, fallback: string): string {
  return node.id === undefined ? fallback : `#${node.id}`;
}

function stringProp(node: UiNode, key: string): string | undefined {
  if (node.kind !== "element") return undefined;
  const value = node.props[key];
  return typeof value === "string" ? value : undefined;
}

function diagnoseDimension(
  diagnostics: DevelopmentDiagnostic[],
  node: UiNode,
  key: "width" | "height",
  target: WorkbenchTarget,
  path: string,
): void {
  const value = stringProp(node, key);
  if (value === undefined) return;
  const accepted = target === "terminal" ? TERMINAL_DIMENSION.test(value)
    : target === "vscode" || target === "web" ? WEB_DIMENSION.test(value)
      : TERMINAL_DIMENSION.test(value) && WEB_DIMENSION.test(value);
  if (!accepted) {
    diagnostics.push({
      kind: "layout",
      severity: "warning",
      code: "ignored-dimension",
      path: `${path}.props.${key}`,
      message: `${target} ignores ${key}=${JSON.stringify(value)}`,
      suggestion: target === "terminal"
        ? "Use cells, %, fr, full, or auto; prefer semantic width variants for cross-host stories."
        : "Use px, %, rem, em, ch, vh, vw, or auto.",
    });
  }
}

function diagnoseNode(
  node: UiNode,
  target: WorkbenchTarget,
  diagnostics: DevelopmentDiagnostic[],
  path: string,
): void {
  if (node.kind === "text") return;
  const current = pathFor(node, path);
  if (!(UI_PRIMITIVES as readonly string[]).includes(node.type) && node.fallback === undefined) {
    diagnostics.push({
      kind: "primitive", severity: "warning", code: "custom-primitive-without-fallback", path: current,
      message: `${node.type} has no semantic fallback`, suggestion: "Provide fallback content for hosts that do not implement this primitive.",
    });
  }
  diagnoseDimension(diagnostics, node, "width", target, current);
  diagnoseDimension(diagnostics, node, "height", target, current);
  for (const key of ["gap", "padding"] as const) {
    const value = node.props[key];
    if (typeof value === "string" && !SIZE_TOKENS.has(value)) {
      diagnostics.push({
        kind: "layout", severity: "warning", code: "ignored-spacing", path: `${current}.props.${key}`,
        message: `${target} may ignore spacing token ${JSON.stringify(value)}`,
        suggestion: "Use xs, sm, md, lg, xl, or a bounded numeric value.",
      });
    }
  }
  for (const key of ["foreground", "background", "borderColor"] as const) {
    const value = stringProp(node, key)?.replace(/^theme\./u, "");
    if (value !== undefined && !THEME_TOKENS.has(value)) {
      diagnostics.push({
        kind: "theme", severity: "warning", code: "unknown-theme-token", path: `${current}.props.${key}`,
        message: `Unknown semantic theme token ${JSON.stringify(value)}`,
        suggestion: "Use a governed semantic token; unknown tokens intentionally fall back to host defaults.",
      });
    }
  }
  if ((node.type === "WebOnly" || node.requires?.some((requirement) => requirement.feature === "web"))
    && node.fallback === undefined) {
    diagnostics.push({
      kind: "fallback", severity: "error", code: "missing-terminal-fallback", path: current,
      message: "Web-specific content has no semantic fallback", suggestion: "Add a terminal-safe fallback node.",
    });
  }
  elementChildren(node).forEach((child, index) => diagnoseNode(child, target, diagnostics, `${current}.children[${index}]`));
}

/** Combined schema, accessibility, fallback, theme, and host-layout diagnostics. */
export function diagnoseDocument(document: UiDocument, target: WorkbenchTarget): DevelopmentDiagnostic[] {
  const diagnostics: DevelopmentDiagnostic[] = [];
  const validation = validateDocument(document);
  diagnostics.push(...validation.issues.map((issue) => ({
    kind: "validation" as const,
    severity: "error" as const,
    code: issue.code,
    path: issue.path,
    message: issue.message,
  })));
  diagnostics.push(...auditAccessibility(document.root).map((issue) => ({
    kind: "accessibility" as const,
    severity: issue.severity,
    code: issue.code,
    path: issue.nodeId === undefined ? "root" : `#${issue.nodeId}`,
    message: issue.message,
  })));
  diagnoseNode(document.root, target, diagnostics, "root");
  return diagnostics;
}

export function formatDevelopmentDiagnostic(diagnostic: DevelopmentDiagnostic): string {
  const suggestion = diagnostic.suggestion === undefined ? "" : ` Fix: ${diagnostic.suggestion}`;
  return `${diagnostic.severity.toUpperCase()} ${diagnostic.kind}/${diagnostic.code} ${diagnostic.path}: ${diagnostic.message}${suggestion}`;
}

export function jsonRecord(value: unknown): Readonly<Record<string, UiJsonValue>> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Readonly<Record<string, UiJsonValue>>
    : undefined;
}
