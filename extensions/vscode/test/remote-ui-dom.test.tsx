// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { UI_CONTRIBUTION_POINTS, UI_PROTOCOL_VERSION, type UiDocument, type UiElementNode, type UiEvent, type UiPrimitive } from "@codypendent/ui";
import { UI_CONFORMANCE_STORY } from "@codypendent/ui/tooling";

import { createWebviewCapabilities, WEB_PRIMITIVES } from "../src/webview/remote-ui/capabilities.js";
import { RemoteUiRenderer, type RemoteUiRecoveryRequest } from "../src/webview/remote-ui/renderer.js";
import { WEB_CONTRIBUTION_POINTS, WEB_SLOT_DEFINITIONS } from "../src/webview/remote-ui/slot-registry.js";
import { RemoteUiStore } from "../src/webview/remote-ui/store.js";
import { applyThemeTokens, applyWireTheme } from "../src/webview/remote-ui/theme.js";

const ATTESTED_PLACEMENT = { point: "panel", extensionId: "test.plugin" } as const;

declare global {
  // React uses this opt-in to make test scheduling deterministic.
  // eslint-disable-next-line no-var
  var IS_REACT_ACT_ENVIRONMENT: boolean;
}

function primitive(type: UiPrimitive, index: number) {
  const base = { kind: "element" as const, id: `node-${index}`, type, children: [] };
  switch (type) {
    case "Button": return { ...base, props: { action: "test.action", label: "Run", shortcut: "Ctrl+K" } };
    case "Form": return { ...base, props: { submitAction: "test.submit", accessibleLabel: "Test form" } };
    case "TextInput": case "TextArea": return { ...base, props: { name: `input-${index}`, accessibleLabel: "Input" } };
    case "Select": case "MultiSelect": case "Checkbox": case "Radio": return { ...base, props: { name: `choice-${index}`, accessibleLabel: "Choice", options: [{ value: "yes", label: "Yes" }] } };
    case "Table": return { ...base, props: { columns: ["name"], rows: [{ name: "Ada" }] } };
    case "Graph": return { ...base, props: { accessibleLabel: "Graph", nodes: [], edges: [] } };
    case "Chart": return { ...base, props: { accessibleLabel: "Chart", data: [1, 2, 3] } };
    case "Sparkline": return { ...base, props: { accessibleLabel: "Trend", values: [1, 2, 3] } };
    case "Markdown": return { ...base, props: { source: "**safe** <script>never executes</script>" } };
    case "Code": return { ...base, props: { value: "const x = 1" } };
    case "Diff": return { ...base, props: { patch: "-old\n+new" } };
    case "Image": return { ...base, props: { src: "https://tracker.invalid/image.png", alt: "Blocked image" } };
    case "Audio": return { ...base, props: { src: "https://tracker.invalid/audio.mp3", alt: "Blocked audio", transcript: "Transcript" } };
    case "JsonTree": return { ...base, props: { value: { ok: true } } };
    case "LogViewer": return { ...base, props: { lines: ["hello"] } };
    case "KeyValue": return { ...base, props: { entries: { key: "value" } } };
    case "Progress": return { ...base, props: { value: 50, maximum: 100, label: "Half" } };
    case "Tabs": return { ...base, props: { tabs: [{ id: "one", label: "One" }], activeId: "one" } };
    case "Pagination": return { ...base, props: { current: 1, total: 2, accessibleLabel: "Pages" } };
    case "Link": return { ...base, props: { label: "Unsafe", href: "javascript:alert(1)", action: "link.open" } };
    default: return { ...base, props: { accessibleLabel: type, label: type, title: type, items: [] } };
  }
}

function documentWith(root: UiDocument["root"], revision = 1): UiDocument {
  return { protocolVersion: UI_PROTOCOL_VERSION, documentId: "dom-document", revision, root };
}

describe("RemoteUiRenderer DOM", () => {
  let container: HTMLDivElement;
  let root: Root;
  let store: RemoteUiStore;
  let events: UiEvent[];
  let recoveries: RemoteUiRecoveryRequest[];
  const capabilities = createWebviewCapabilities({ width: 1024, height: 768 });

  beforeEach(() => {
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    store = new RemoteUiStore(capabilities);
    events = [];
    recoveries = [];
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  function render(): void {
    act(() => root.render(<RemoteUiRenderer store={store} capabilities={capabilities} dispatch={(event) => events.push(event)} recover={(request) => recoveries.push(request)} showTerminalFallback />));
  }

  it("renders every built-in primitive without falling into unknown handling", () => {
    const children = WEB_PRIMITIVES.map(primitive);
    store.apply({ type: "snapshot", document: documentWith({ kind: "element", id: "root", type: "Stack", props: {}, children }) }, ATTESTED_PLACEMENT);
    render();

    for (const type of WEB_PRIMITIVES.filter((candidate) => candidate !== "TerminalOnly" && candidate !== "WebOnly")) {
      expect(container.querySelector(`[data-ui-primitive="${type}"]`), type).not.toBeNull();
    }
    expect(container.textContent).toContain("TerminalOnly");
    expect(container.querySelector('[data-ui-primitive="TerminalOnly"]')).toBeNull();
    expect(container.querySelector('[data-ui-primitive="WebOnly"]')).toBeNull();
    expect(container.querySelector(".ui-unsupported")).toBeNull();
    expect(container.querySelector('img[src^="https:"]')).toBeNull();
    expect(container.querySelector('a[href^="javascript:"]')).toBeNull();
    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector(".ui-terminal-preview")?.textContent).toContain("Terminal fallback");
  });

  it("emits revision-bound action events and keyboard shortcuts", () => {
    store.apply(
      { type: "snapshot", document: documentWith(primitive("Button", 1)) },
      { point: "panel", extensionId: "acme.verified" },
    );
    render();
    expect(container.querySelector(".ui-extension-chrome")?.textContent).toContain("acme.verified");
    const button = container.querySelector<HTMLButtonElement>("button");
    expect(button).not.toBeNull();
    act(() => button?.click());
    expect(events[0]).toMatchObject({ documentId: "dom-document", revision: 1, targetId: "node-1", type: "action" });
    expect(events[0].payload).toBeUndefined();

    act(() => button?.dispatchEvent(new KeyboardEvent("keydown", { key: "k", ctrlKey: true, bubbles: true })));
    expect(events).toHaveLength(2);
    expect(events[1].targetId).toBe("node-1");
  });

  it("dispatches React handler-only onPress with the declared event name", () => {
    const button: UiElementNode = {
      kind: "element", id: "local-button", type: "Button",
      props: { label: "Increment", eventHandlers: ["press"] }, children: [],
    };
    store.apply({ type: "snapshot", document: documentWith(button) }, ATTESTED_PLACEMENT);
    render();
    act(() => container.querySelector<HTMLButtonElement>("button")?.click());
    expect(events).toHaveLength(1);
    expect(events[0]).toMatchObject({ targetId: "local-button", type: "press" });
    expect(events[0].payload).toBeUndefined();
  });

  it("mounts every advertised host contribution point through its semantic adapter", () => {
    const points = capabilities.contributionPoints ?? [];
    for (const [index, point] of points.entries()) {
      store.apply({
        type: "snapshot",
        document: {
          protocolVersion: UI_PROTOCOL_VERSION,
          documentId: `slot-${index}`,
          revision: 1,
          root: { kind: "element", id: `root-${index}`, type: "Text", props: { value: point }, children: [] },
        },
      }, { point, extensionId: "acme.slots" });
    }
    render();
    for (const point of points) {
      const adapter = container.querySelector(`[data-ui-slot-adapter="${point}"]`);
      expect(adapter, point).not.toBeNull();
      expect(adapter?.textContent).toContain(point);
    }
    expect(container.querySelector('[data-ui-slot-adapter="status-item"]')?.getAttribute("role")).toBe("status");
    expect(container.querySelector('[data-ui-slot-adapter="notification"]')?.getAttribute("aria-live")).toBe("polite");
    expect(container.querySelector('[data-ui-slot-adapter="message-renderer"]')?.getAttribute("role")).toBe("feed");
    expect(container.querySelector('[data-ui-slot-adapter="sidebar"]')?.tagName).toBe("ASIDE");
    expect(container.querySelector('[data-ui-slot-adapter="form"]')?.getAttribute("role")).toBe("form");
    expect(container.querySelector('[data-ui-slot-adapter="wizard"]')?.getAttribute("role")).toBe("dialog");
    expect(container.querySelector('[data-ui-slot-adapter="context-menu"]')?.getAttribute("role")).toBe("menu");
    expect(container.querySelector('[data-ui-slot-adapter="command-palette"]')?.hasAttribute("inert")).toBe(true);
    expect(container.querySelector('[data-ui-slot-adapter="context-menu"]')?.getAttribute("aria-hidden")).toBe("true");
    expect(container.querySelector('[data-ui-slot-adapter="wizard"]')?.hasAttribute("inert")).toBe(false);
    expect(container.querySelector('[data-ui-slot-adapter="notification"]')?.hasAttribute("inert")).toBe(false);
    expect(WEB_CONTRIBUTION_POINTS).toEqual(UI_CONTRIBUTION_POINTS);
    expect(WEB_SLOT_DEFINITIONS).toHaveLength(UI_CONTRIBUTION_POINTS.length);
    expect(new Set(WEB_SLOT_DEFINITIONS.map((definition) => definition.region))).toEqual(new Set([
      "sidebar", "navigation", "primary", "transcript", "composer", "setup", "status", "overlay",
    ]));
    for (const definition of WEB_SLOT_DEFINITIONS) {
      const adapter = container.querySelector(`[data-ui-slot-adapter="${definition.point}"]`);
      expect(adapter?.closest(`[data-ui-host-region="${definition.region}"]`), definition.point).not.toBeNull();
      expect(adapter?.getAttribute("aria-label"), definition.point).toBe(definition.label);
    }
  });

  it("renders the shared story across host structure, long content, and overlays", () => {
    if (UI_CONFORMANCE_STORY.document === undefined) throw new Error("conformance story must include a document");
    store.apply({ type: "snapshot", document: UI_CONFORMANCE_STORY.document }, { point: "panel", extensionId: "conformance.story" });
    render();
    expect(container.querySelector('[data-ui-host-region="primary"] [data-ui-slot-adapter="panel"]')).not.toBeNull();
    expect(container.querySelector('[data-ui-document-id="conformance-surface-states"]')?.textContent).toContain("A-very-long-unbroken-extension-value");
    expect(container.querySelector('[data-ui-node-id="long"]')).not.toBeNull();
    for (const theme of UI_CONFORMANCE_STORY.themes ?? []) {
      applyWireTheme(container, {
        id: theme.id, name: theme.id, revision: 1,
        colorScheme: theme.mode === "light" ? "light" : "dark",
        highContrast: theme.mode === "highContrast",
        reducedMotion: false,
        tokens: theme.colors,
      });
      expect(container.dataset.uiTheme).toBe(theme.id);
    }
    for (const viewport of UI_CONFORMANCE_STORY.viewports ?? []) {
      container.style.width = `${viewport.width}ch`;
      container.style.height = `${viewport.height}rem`;
      expect(container.scrollWidth).toBeGreaterThanOrEqual(0);
    }

    store.apply({ type: "snapshot", document: documentWith(primitive("Button", 999), 1) }, { point: "command-palette", extensionId: "overlay.story" });
    render();
    expect(container.querySelector('[data-ui-host-region="overlay"] [role="dialog"]')).not.toBeNull();
    expect(document.activeElement?.tagName).toBe("BUTTON");
  });

  it("offers document-scoped retry, disable, and report recovery actions", () => {
    store.apply({ type: "snapshot", document: documentWith(primitive("Button", 1)) }, { point: "panel", extensionId: "acme.recovery" });
    store.apply({ type: "error", documentId: "dom-document", code: "render.failed", message: "Could not render this document" });
    render();
    const card = container.querySelector('[data-ui-error-document="dom-document"]');
    expect(card?.getAttribute("role")).toBe("alert");
    expect(card?.getAttribute("aria-live")).toBe("polite");
    const buttons = [...(card?.querySelectorAll("button") ?? [])];
    expect(buttons.map((button) => button.textContent)).toEqual(["Retry", "Disable extension surface", "Report details"]);
    buttons.forEach((button) => act(() => button.click()));
    expect(recoveries.map((request) => request.action)).toEqual(["retry", "disable", "report"]);
    expect(recoveries.every((request) => request.documentId === "dom-document" && request.extensionId === "acme.recovery")).toBe(true);
  });

  it("submits normalized form data and applies patches without remounting the host", () => {
    const form = primitive("Form", 0) as UiElementNode;
    form.children = [
      { kind: "element", id: "name", type: "TextInput", props: { name: "name", accessibleLabel: "Name", value: "Ada" }, children: [] },
      { kind: "element", id: "submit", type: "Button", props: { action: "test.submit", label: "Submit", submit: true }, children: [] },
    ];
    store.apply({ type: "snapshot", document: documentWith(form) }, ATTESTED_PLACEMENT);
    render();
    const formElement = container.querySelector("form");
    expect(formElement).not.toBeNull();
    act(() => container.querySelector<HTMLButtonElement>('button[type="submit"]')?.click());
    expect(events).toHaveLength(1);
    expect(events.at(-1)).toMatchObject({ type: "submit", payload: { formData: { name: "Ada" } } });

    act(() => {
      store.apply({
        type: "patch",
        batch: {
          protocolVersion: UI_PROTOCOL_VERSION,
          documentId: "dom-document",
          baseRevision: 1,
          revision: 2,
          patches: [{ op: "updateProps", nodeId: "name", set: { value: "Grace" } }],
          atomic: true,
        },
      });
    });
    expect(container.querySelector<HTMLInputElement>('input[name="name"]')?.value).toBe("Grace");
  });

  it("reports renderer output using semantic roles and labels", () => {
    store.apply({ type: "snapshot", document: documentWith({
      kind: "element", id: "root", type: "Stack", props: { accessibleLabel: "Controls" }, children: [
        { kind: "element", id: "alert", type: "Alert", props: { title: "Problem", message: "Try again", tone: "critical" }, children: [] },
        { kind: "element", id: "progress", type: "Progress", props: { label: "Build", value: 4, maximum: 10 }, children: [] },
      ],
    }) }, ATTESTED_PLACEMENT);
    render();
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("Problem");
    expect(container.querySelector("progress")?.getAttribute("max")).toBe("10");
    expect(container.querySelector('[aria-label="Controls"]')).not.toBeNull();
  });

  it("accepts inert theme scalars and rejects CSS injection", () => {
    applyWireTheme(container, {
      id: "safe-theme",
      name: "Safe",
      revision: 1,
      tokens: {
        accent: "#4ea1ff",
        unsafe: "url(https://tracker.invalid/token)",
        escape: "red; background: black",
      },
    });
    expect(container.dataset.uiTheme).toBe("safe-theme");
    expect(container.style.getPropertyValue("--cody-ui-accent")).toBe("#4ea1ff");
    expect(container.style.getPropertyValue("--cody-ui-unsafe")).toBe("");
    expect(container.style.getPropertyValue("--cody-ui-escape")).toBe("");
    applyThemeTokens(container, {
      id: "semantic",
      mode: "dark",
      colors: { "text.primary": "#eeeeee", "surface.background": "#111111" },
      spacing: { md: 8 },
    });
    expect(container.style.getPropertyValue("--cody-ui-text-primary")).toBe("#eeeeee");
    expect(container.style.getPropertyValue("--cody-ui-surface-background")).toBe("#111111");
    expect(container.style.getPropertyValue("--cody-ui-spacing-md")).toBe("8px");
  });

  it("windows large virtual lists while preserving list position metadata", () => {
    store.apply({ type: "snapshot", document: documentWith({
      kind: "element",
      id: "virtual",
      type: "VirtualList",
      props: { accessibleLabel: "Many items", items: Array.from({ length: 1_000 }, (_, index) => ({ id: `item-${index}`, label: `Item ${index}` })), rowHeight: 24, height: 240 },
      children: [],
    }) }, ATTESTED_PLACEMENT);
    render();
    const visible = container.querySelectorAll('[role="listitem"]');
    expect(visible.length).toBeGreaterThan(0);
    expect(visible.length).toBeLessThan(50);
    expect(visible[0]?.getAttribute("aria-setsize")).toBe("1000");
  });
});
