import type { UiNode } from "./protocol.js";

export interface AccessibilityIssue {
  nodeId?: string;
  severity: "error" | "warning";
  code: "missingLabel" | "mouseOnly" | "colorOnly" | "focusOrder" | "missingTranscript" | "missingAlt";
  message: string;
}

const INTERACTIVE = new Set(["Button", "Link", "TextInput", "TextArea", "Select", "MultiSelect", "Checkbox", "Radio", "Menu", "Tabs"]);

export function accessibleLabel(label: string, description?: string): { accessibleLabel: string; description?: string } {
  return { accessibleLabel: label, ...(description === undefined ? {} : { description }) };
}

export function keyboardAction(action: string, shortcut?: string): { action: string; shortcut?: string } {
  return { action, ...(shortcut === undefined ? {} : { shortcut }) };
}

export function auditAccessibility(root: UiNode): AccessibilityIssue[] {
  const issues: AccessibilityIssue[] = [];
  const focusOrders = new Set<number>();
  const visit = (node: UiNode): void => {
    if (node.kind === "text") return;
    const label = node.props.accessibleLabel ?? node.props.label ?? node.props.title ?? node.props.alt;
    if (INTERACTIVE.has(node.type) && (typeof label !== "string" || label.length === 0)) {
      issues.push({ ...(node.id === undefined ? {} : { nodeId: node.id }), severity: "error", code: "missingLabel", message: `${node.type} needs an accessible label` });
    }
    if (node.type === "Image" && typeof node.props.alt !== "string") {
      issues.push({ ...(node.id === undefined ? {} : { nodeId: node.id }), severity: "error", code: "missingAlt", message: "Image needs alt text" });
    }
    if (node.type === "Audio" && typeof node.props.transcript !== "string" && node.fallback === undefined) {
      issues.push({ ...(node.id === undefined ? {} : { nodeId: node.id }), severity: "warning", code: "missingTranscript", message: "Audio should provide a transcript or fallback" });
    }
    if (node.props.mouseAction !== undefined && node.props.action === undefined && node.props.shortcut === undefined) {
      issues.push({ ...(node.id === undefined ? {} : { nodeId: node.id }), severity: "error", code: "mouseOnly", message: "Mouse actions need a keyboard action" });
    }
    if (node.props.tone !== undefined && node.props.status === undefined && label === undefined && node.children.length === 0) {
      issues.push({ ...(node.id === undefined ? {} : { nodeId: node.id }), severity: "warning", code: "colorOnly", message: "Tone must not be the only meaning" });
    }
    const order = node.props.focusOrder;
    if (typeof order === "number") {
      if (focusOrders.has(order)) issues.push({ ...(node.id === undefined ? {} : { nodeId: node.id }), severity: "warning", code: "focusOrder", message: `Duplicate focus order ${order}` });
      focusOrders.add(order);
    }
    node.children.forEach(visit);
    if (node.fallback !== undefined) visit(node.fallback);
  };
  visit(root);
  return issues;
}

/** Deterministic plain-text representation for screen readers and logs. */
export function toAccessibleText(root: UiNode): string {
  const lines: string[] = [];
  const visit = (node: UiNode, depth: number): void => {
    if (node.kind === "text") {
      if (node.text.trim()) lines.push(`${"  ".repeat(depth)}${node.text}`);
      return;
    }
    const label = node.props.accessibleLabel ?? node.props.label ?? node.props.title ?? node.props.alt ?? node.props.value;
    if (typeof label === "string" && label.trim()) lines.push(`${"  ".repeat(depth)}${label}`);
    node.children.forEach((child) => visit(child, depth + (label === undefined ? 0 : 1)));
  };
  visit(root, 0);
  return lines.join("\n");
}
