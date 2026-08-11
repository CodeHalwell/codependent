/** @jsxImportSource react */
import { describe, expect, it } from "vitest";
import { UI_PROTOCOL_VERSION, type UiEvent, type UiHostMessage } from "../src/index.js";
import { Button, Stack, Text, TextInput, createReactUiRoot } from "../src/react/index.js";

describe("React renderer", () => {
  it("emits a snapshot and incremental prop patch", () => {
    const messages: UiHostMessage[] = [];
    const root = createReactUiRoot({ documentId: "react-doc", onMessage: (message) => messages.push(message) });
    root.render(<Text id="greeting" value="Hello" />);
    root.render(<Text id="greeting" value="World" />);
    expect(messages[0]).toMatchObject({ type: "snapshot", document: { revision: 0 } });
    expect(messages[1]).toMatchObject({ type: "patch", batch: { baseRevision: 0, revision: 1, patches: [{ op: "updateProps", nodeId: "greeting", set: { value: "World" } }] } });
  });

  it("preserves host ids across keyed reorder and emits move patches", () => {
    const messages: UiHostMessage[] = [];
    const root = createReactUiRoot({ documentId: "keyed", onMessage: (message) => messages.push(message) });
    root.render(<Stack id="root">{["a", "b"].map((value) => <Text key={value} value={value} />)}</Stack>);
    const first = root.getDocument();
    root.render(<Stack id="root">{["b", "a"].map((value) => <Text key={value} value={value} />)}</Stack>);
    const second = root.getDocument();
    expect(first?.root.kind === "element" ? first.root.children.map((node) => node.id).sort() : []).toEqual(
      second?.root.kind === "element" ? second.root.children.map((node) => node.id).sort() : [],
    );
    const update = messages.at(-1);
    expect(update?.type).toBe("patch");
    if (update?.type === "patch") expect(update.batch.patches.some((patch) => patch.op === "move")).toBe(true);
  });

  it("rejects stale events and dispatches revision-current events", () => {
    let actions = 0;
    const root = createReactUiRoot({ documentId: "events" });
    root.render(<Button id="run" label="Run" onAction={() => { actions += 1; }} />);
    const current = root.getDocument();
    const event: UiEvent = {
      protocolVersion: UI_PROTOCOL_VERSION,
      eventId: "event-1",
      documentId: "events",
      revision: current?.revision ?? 0,
      targetId: "run",
      type: "action",
    };
    expect(root.dispatch({ ...event, revision: event.revision - 1 })).toBe(false);
    expect(root.dispatch(event)).toBe(true);
    expect(actions).toBe(1);
  });

  it("keeps handler-only commits local without emitting empty revision batches", () => {
    const messages: UiHostMessage[] = [];
    let result = "first";
    const root = createReactUiRoot({ documentId: "handlers", onMessage: (message) => messages.push(message) });
    root.render(<Button id="run" label="Run" onAction={() => { result = "old"; }} />);
    root.render(<Button id="run" label="Run" onAction={() => { result = "new"; }} />);
    expect(messages).toHaveLength(1);
    expect(root.getDocument()?.revision).toBe(0);
    expect(root.dispatch({
      protocolVersion: UI_PROTOCOL_VERSION,
      eventId: "event-handler-update",
      documentId: "handlers",
      revision: 0,
      targetId: "run",
      type: "action",
    })).toBe(true);
    expect(result).toBe("new");
  });

  it("serializes handler presence while keeping functions worker-local", () => {
    const root = createReactUiRoot({ documentId: "stateful-input" });
    root.render(<TextInput id="query" name="query" value="" onChange={() => undefined} />);
    const document = root.getDocument();
    expect(document?.root).toMatchObject({
      props: { value: "", eventHandlers: ["change"] },
    });
    expect(JSON.stringify(document)).not.toContain("onChange");
  });

  it("rejects ambiguous local and host-mediated bindings", () => {
    const errors: unknown[] = [];
    const root = createReactUiRoot({ documentId: "ambiguous", onError: (error) => errors.push(error) });
    root.render(<Button id="run" action="run" onAction={() => undefined} />);
    expect(errors.map(String).join("\n")).toContain("host-mediated action");
    expect(JSON.stringify(root.getDocument())).not.toContain("\"action\":\"run\"");
  });
});
