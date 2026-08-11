/** @jsxImportSource react */
import { useState } from "react";
import { describe, expect, it } from "vitest";
import { DEFAULT_UI_HARD_LIMITS, type UiWireMessage } from "../src/protocol.js";
import { TextInput } from "../src/react/index.js";
import { defaultWorkerCapabilities } from "../src/worker/bridge.js";
import { createReactUiSurface } from "../src/worker/react.js";
import { UiWorkerRuntime, type UiWorkerTransport } from "../src/worker/runtime.js";

class MemoryTransport implements UiWorkerTransport {
  #input: unknown[] = [];
  #waiting: ((value: IteratorResult<unknown>) => void) | undefined;
  #closed = false;
  readonly output: UiWireMessage[] = [];
  readonly incoming: AsyncIterable<unknown> = { [Symbol.asyncIterator]: () => ({ next: () => this.#next() }) };
  push(message: unknown): void {
    if (this.#waiting !== undefined) {
      const waiting = this.#waiting;
      this.#waiting = undefined;
      waiting({ value: message, done: false });
    } else this.#input.push(message);
  }
  async send(message: UiWireMessage): Promise<void> { this.output.push(structuredClone(message)); }
  async close(): Promise<void> { this.#closed = true; this.#waiting?.({ value: undefined, done: true }); this.#waiting = undefined; }
  async #next(): Promise<IteratorResult<unknown>> {
    const next = this.#input.shift();
    if (next !== undefined) return { value: next, done: false };
    if (this.#closed) return { value: undefined, done: true };
    return new Promise((resolve) => { this.#waiting = resolve; });
  }
}

async function waitFor(transport: MemoryTransport, type: string): Promise<UiWireMessage> {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    const message = [...transport.output].reverse().find((candidate) => candidate.type === type);
    if (message !== undefined) return message;
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
  throw new Error(`timed out waiting for ${type}`);
}

function StatefulInput() {
  const [value, setValue] = useState("");
  return <TextInput
    id="query"
    name="query"
    value={value}
    onChange={(event) => setValue(typeof event.payload === "string" ? event.payload : "")}
  />;
}

describe("React worker event mediation", () => {
  it("round-trips a declared handler-only change event into a state patch", async () => {
    const transport = new MemoryTransport();
    const offer = defaultWorkerCapabilities({ capabilities: [], contributionPoints: [] });
    const runtime = new UiWorkerRuntime(transport, {
      capabilityOffer: offer,
      surfaces: [createReactUiSurface({ documentId: "input", render: () => <StatefulInput /> })],
    });
    const running = runtime.run();
    transport.push({ type: "capabilities", messageId: "host", capabilities: offer });
    await waitFor(transport, "capabilities");
    transport.push({ type: "capabilitySelection", messageId: "selection", selection: {
      protocolVersion: { major: 1, minor: 0 }, primitives: ["*"], capabilities: [], contributionPoints: [],
      imageProtocols: [], colorDepth: 1, unicode: false, mouse: false, screenReader: false,
      viewport: offer.viewport, limits: DEFAULT_UI_HARD_LIMITS,
    } });
    const snapshot = await waitFor(transport, "snapshot");
    expect(snapshot).toMatchObject({ snapshot: { document: { root: { id: "query", props: { value: "", eventHandlers: ["change"] } } } } });
    transport.push({
      type: "event", messageId: "change-1",
      event: { protocolVersion: { major: 1, minor: 0 }, eventId: "change-1", documentId: "input", revision: 0, targetId: "query", type: "change", payload: "hello" },
    });
    const patch = await waitFor(transport, "patchBatch");
    expect(patch).toMatchObject({ patchBatch: { baseRevision: 0, revision: 1, patches: [{ op: "updateProps", nodeId: "query", set: { value: "hello" } }] } });
    const patchCount = transport.output.filter((message) => message.type === "patchBatch").length;
    transport.push({ type: "host.dispose", messageId: "dispose", extensions: { control: {} } });
    await waitFor(transport, "worker.disposed");
    await expect(running).resolves.toBeUndefined();
    expect(transport.output.filter((message) => message.type === "patchBatch")).toHaveLength(patchCount);
    expect(transport.output.some((message) => message.type === "dispose" && message.dispose.documentId === "input")).toBe(true);
  });
});
