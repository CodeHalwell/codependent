import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { RemoteUiRenderer } from "../src/components/RemoteUiRenderer.js";
import { UI_PROTOCOL_VERSION, type UiDocument, type UiEvent } from "@codypendent/ui";

describe("Desktop RemoteUiRenderer", () => {
  it("returns null when there are no active documents", () => {
    const { container } = render(<RemoteUiRenderer documents={new Map()} />);
    expect(container.firstChild).toBeNull();
  });

  it("desktop_renders_semantic_remote_ui", () => {
    const events: UiEvent[] = [];
    const docId = "desktop-test-doc";

    const semanticDocument: UiDocument = {
      protocolVersion: UI_PROTOCOL_VERSION,
      documentId: docId,
      revision: 1,
      metadata: {
        title: "Test Extension Tool",
        source: "acme.tools",
      },
      root: {
        kind: "element",
        id: "root-stack",
        type: "Stack",
        props: { accessibleLabel: "Semantic Tool Container" },
        children: [
          {
            kind: "element",
            id: "status-alert",
            type: "Alert",
            props: { title: "Build Status", message: "Compilation succeeded", tone: "success" },
            children: [],
          },
          {
            kind: "element",
            id: "metric-badge",
            type: "Badge",
            props: { title: "v1.2.0", tone: "accent" },
            children: [],
          },
          {
            kind: "element",
            id: "input-field",
            type: "TextInput",
            props: { name: "searchQuery", accessibleLabel: "Search Packages", placeholder: "e.g. lodash" },
            children: [],
          },
          {
            kind: "element",
            id: "submit-button",
            type: "Button",
            props: { label: "Run Package Search", action: "search.run" },
            children: [],
          },
        ],
      },
    };

    const documents = new Map<string, UiDocument>([[docId, semanticDocument]]);

    const { container } = render(
      <RemoteUiRenderer
        documents={documents}
        onEvent={(event) => events.push(event)}
        showTerminalFallback
      />,
    );

    // 1. Assert host chrome and document container render
    expect(screen.getByText(/Live Extensions \(1\)/i)).toBeDefined();
    expect(screen.getByLabelText("Extension identity")).toBeDefined();
    expect(container.querySelector('[data-ui-document-id="desktop-test-doc"]')).not.toBeNull();

    // 2. Assert semantic DOM elements are rendered instead of raw metadata strings
    // Alerts and feedback
    const alert = container.querySelector('[role="alert"]');
    expect(alert).not.toBeNull();
    expect(alert?.textContent).toContain("Build Status");
    expect(alert?.textContent).toContain("Compilation succeeded");

    // Badges
    const badge = container.querySelector(".ui-badge");
    expect(badge).not.toBeNull();
    expect(badge?.textContent).toContain("v1.2.0");

    // Text inputs
    const input = container.querySelector<HTMLInputElement>('input[placeholder="e.g. lodash"]');
    expect(input).not.toBeNull();
    expect(input?.getAttribute("aria-label")).toBe("Search Packages");

    // Buttons
    const button = container.querySelector<HTMLButtonElement>('button.ui-button');
    expect(button).not.toBeNull();
    expect(button?.textContent).toBe("Run Package Search");

    // Terminal preview
    expect(container.querySelector(".ui-terminal-preview")).not.toBeNull();

    // 3. Test interactive event dispatching
    act(() => {
      button?.click();
    });

    expect(events.length).toBeGreaterThan(0);
    expect(events[0]).toMatchObject({
      documentId: docId,
      revision: 1,
      targetId: "submit-button",
      type: "action",
    });

    // Test text input change
    act(() => {
      fireEvent.change(input!, { target: { value: "vitest" } });
    });

    expect(events.length).toBeGreaterThan(1);
    expect(events.at(-1)).toMatchObject({
      documentId: docId,
      revision: 1,
      targetId: "input-field",
      type: "change",
    });
  });

  it("handles multiple documents and document disposal", () => {
    const doc1: UiDocument = {
      protocolVersion: UI_PROTOCOL_VERSION,
      documentId: "doc-1",
      revision: 1,
      metadata: { title: "Doc 1", source: "ext.one" },
      root: {
        kind: "element",
        id: "root-1",
        type: "Text",
        props: { value: "Content of Doc 1" },
        children: [],
      },
    };

    const doc2: UiDocument = {
      protocolVersion: UI_PROTOCOL_VERSION,
      documentId: "doc-2",
      revision: 1,
      metadata: { title: "Doc 2", source: "ext.two" },
      root: {
        kind: "element",
        id: "root-2",
        type: "Text",
        props: { value: "Content of Doc 2" },
        children: [],
      },
    };

    const documents = new Map<string, UiDocument>([
      ["doc-1", doc1],
      ["doc-2", doc2],
    ]);

    const { container, rerender } = render(<RemoteUiRenderer documents={documents} />);

    expect(container.querySelectorAll('[data-ui-document-id]').length).toBe(2);
    expect(container.textContent).toContain("Content of Doc 1");
    expect(container.textContent).toContain("Content of Doc 2");

    // Remove doc-1
    const nextDocuments = new Map<string, UiDocument>([["doc-2", doc2]]);
    rerender(<RemoteUiRenderer documents={nextDocuments} />);

    expect(container.querySelectorAll('[data-ui-document-id]').length).toBe(1);
    expect(container.textContent).not.toContain("Content of Doc 1");
    expect(container.textContent).toContain("Content of Doc 2");
  });
});
