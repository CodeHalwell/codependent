import { describe, expect, it } from "vitest";
import { PassThrough } from "node:stream";
import {
  DEFAULT_UI_HARD_LIMITS,
  MINIMAL_TERMINAL_CAPABILITIES,
  UI_WORKER_MESSAGE_BURST,
  UI_WORKER_MESSAGE_RATE_PER_SECOND,
  type UiWireMessage,
} from "../src/protocol.js";
import { Button, Stack, Text } from "../src/primitives.js";
import { HotReloadStateStore } from "../src/hot-reload.js";
import { createPureUiSurface, UiWorkerRuntime, type UiWorkerTransport } from "../src/worker/runtime.js";
import { decodeUiFrames, UiFrameWriter } from "../src/worker/framing.js";
import { createNodeStreamUiTransport } from "../src/worker/stdio.js";

class MemoryTransport implements UiWorkerTransport {
  #input: unknown[] = [];
  #waiting: ((value: IteratorResult<unknown>) => void) | undefined;
  #closed = false;
  readonly output: UiWireMessage[] = [];
  readonly incoming: AsyncIterable<unknown> = { [Symbol.asyncIterator]: () => ({ next: () => this.#next() }) };
  push(message: unknown): void { const waiting = this.#waiting; if (waiting !== undefined) { this.#waiting = undefined; waiting({ value: message, done: false }); } else this.#input.push(message); }
  async send(message: UiWireMessage): Promise<void> { this.output.push(structuredClone(message)); }
  async close(): Promise<void> { this.#closed = true; this.#waiting?.({ value: undefined, done: true }); this.#waiting = undefined; }
  async #next(): Promise<IteratorResult<unknown>> {
    const value = this.#input.shift();
    if (value !== undefined) return { value, done: false };
    if (this.#closed) return { value: undefined, done: true };
    return new Promise((resolve) => { this.#waiting = resolve; });
  }
}

async function waitFor(transport: MemoryTransport, type: string, count = 1): Promise<UiWireMessage> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const found = transport.output.filter((message) => message.type === type);
    if (found.length >= count) return found[count - 1] as UiWireMessage;
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
  throw new Error(`timed out waiting for ${type}`);
}

describe("worker runtime", () => {
  it("completes a real length-framed Node stream handshake", async () => {
    const workerInput = new PassThrough();
    const workerOutput = new PassThrough();
    const runtime = new UiWorkerRuntime(createNodeStreamUiTransport(workerInput, workerOutput), { capabilityOffer: MINIMAL_TERMINAL_CAPABILITIES, surfaces: [] });
    const running = runtime.run();
    const hostWriter = new UiFrameWriter(async (frame) => await new Promise<void>((resolvePromise, reject) => workerInput.write(frame, (error) => error === null || error === undefined ? resolvePromise() : reject(error))));
    const hostReader = decodeUiFrames(workerOutput)[Symbol.asyncIterator]();
    await hostWriter.write({ type: "capabilities", messageId: "host", capabilities: MINIMAL_TERMINAL_CAPABILITIES });
    expect((await hostReader.next()).value).toMatchObject({ type: "capabilities" });
    await hostWriter.write({ type: "capabilitySelection", messageId: "selection", selection: {
      protocolVersion: { major: 1, minor: 0 }, primitives: MINIMAL_TERMINAL_CAPABILITIES.primitives,
      capabilities: [], contributionPoints: [], imageProtocols: [], colorDepth: 1, unicode: false, mouse: false, screenReader: false,
      viewport: MINIMAL_TERMINAL_CAPABILITIES.viewport, limits: DEFAULT_UI_HARD_LIMITS,
    } });
    expect((await hostReader.next()).value).toMatchObject({ type: "worker.ready" });
    await hostWriter.write({ type: "host.dispose", messageId: "dispose", extensions: { control: {} } });
    expect((await hostReader.next()).value).toMatchObject({ type: "worker.disposed" });
    await expect(running).resolves.toBeUndefined();
    workerInput.end(); workerOutput.end(); await hostWriter.close();
  });

  it("handshakes, snapshots, handles lifecycle controls, and shuts down", async () => {
    const transport = new MemoryTransport();
    const offer = {
      ...MINIMAL_TERMINAL_CAPABILITIES, client: "test" as const, primitives: "*" as const,
      capabilities: ["context-read", "command-invoke"] as const, contributionPoints: ["panel"] as const,
      limits: DEFAULT_UI_HARD_LIMITS,
    };
    const hotReloadState = new HotReloadStateStore({ count: 3 }, 1);
    const runtime = new UiWorkerRuntime(transport, {
      capabilityOffer: offer,
      hotReloadState,
      surfaces: [createPureUiSurface({ documentId: "main", render: () => Text({ id: "root", children: "Hello" }) })],
    });
    const running = runtime.run();
    transport.push({ type: "capabilities", messageId: "host-capabilities", capabilities: offer });
    await waitFor(transport, "capabilities");
    transport.push({
      type: "capabilitySelection", messageId: "selection",
      selection: {
        protocolVersion: { major: 1, minor: 0 }, primitives: ["*"], capabilities: ["context-read", "command-invoke"],
        contributionPoints: ["panel"], imageProtocols: [], colorDepth: 1, unicode: false, mouse: false, screenReader: false,
        viewport: offer.viewport, limits: DEFAULT_UI_HARD_LIMITS,
      },
    });
    await waitFor(transport, "worker.ready");
    const first = await waitFor(transport, "snapshot");
    expect(first.type === "snapshot" && first.snapshot.document.root).toMatchObject({ id: "root", type: "Text" });

    transport.push({ type: "host.ping", messageId: "ping", extensions: { control: {} } });
    await waitFor(transport, "worker.pong");
    transport.push({ type: "resync", messageId: "resync", resync: { documentId: "main", knownRevision: 0 } });
    const resync = await waitFor(transport, "snapshot", 2);
    expect(resync.type === "snapshot" && resync.snapshot.reason).toBe("host-request");
    transport.push({ type: "hotReload", messageId: "reload", hotReload: { generation: 2, changedModules: ["component.js"] } });
    const reloaded = await waitFor(transport, "worker.reloaded");
    expect(reloaded.extensions).toMatchObject({ control: { generation: 2, states: { count: 3 } } });
    await waitFor(transport, "snapshot", 3);
    transport.push({ type: "host.dispose", messageId: "dispose", extensions: { control: {} } });
    await waitFor(transport, "worker.disposed");
    await expect(running).resolves.toBeUndefined();
    expect(runtime.state).toBe("disposed");
  });

  it("rejects selections that escalate host/worker offers", async () => {
    const transport = new MemoryTransport();
    const runtime = new UiWorkerRuntime(transport, { capabilityOffer: MINIMAL_TERMINAL_CAPABILITIES, surfaces: [] });
    const running = runtime.run();
    transport.push({ type: "capabilities", messageId: "host", capabilities: MINIMAL_TERMINAL_CAPABILITIES });
    await waitFor(transport, "capabilities");
    transport.push({ type: "capabilitySelection", messageId: "bad", selection: {
      protocolVersion: { major: 1, minor: 0 }, primitives: ["Image"], capabilities: ["command-invoke"], contributionPoints: [],
      imageProtocols: ["kitty"], colorDepth: 24, unicode: true, mouse: true, screenReader: true, limits: DEFAULT_UI_HARD_LIMITS,
    } });
    await expect(running).rejects.toThrow(/not offered|unoffered|unsupported/u);
  });

  it("atomically registers negotiated manifest contributions and clears them on shutdown", async () => {
    const transport = new MemoryTransport();
    const offer = {
      ...MINIMAL_TERMINAL_CAPABILITIES,
      contributionPoints: ["panel"] as const,
      limits: DEFAULT_UI_HARD_LIMITS,
    };
    const runtime = new UiWorkerRuntime(transport, {
      pluginId: "acme.component",
      capabilityOffer: offer,
      surfaces: [createPureUiSurface({ documentId: "main", render: () => Text({ id: "root", value: "Panel" }) })],
      contributions: [{ id: "acme.component.panel", point: "panel", renderer: "acme.Panel", documentId: "main" }],
    });
    const running = runtime.run();
    transport.push({ type: "capabilities", messageId: "host", capabilities: offer });
    await waitFor(transport, "capabilities");
    transport.push({ type: "capabilitySelection", messageId: "selection", selection: {
      protocolVersion: { major: 1, minor: 0 }, primitives: offer.primitives,
      capabilities: [], contributionPoints: ["panel"], imageProtocols: [], colorDepth: 1,
      unicode: false, mouse: false, screenReader: false, viewport: offer.viewport, limits: DEFAULT_UI_HARD_LIMITS,
    } });
    const registration = await waitFor(transport, "contributions");
    expect(registration).toMatchObject({
      extensions: { contributionOwner: "acme.component" },
      contributions: [{
        id: "acme.component.panel", extensionId: "acme.component", point: "panel", slot: "panel",
        documentId: "main", metadata: { renderer: "acme.Panel" },
      }],
    });
    transport.push({ type: "host.dispose", messageId: "dispose", extensions: { control: {} } });
    const cleared = await waitFor(transport, "contributions", 2);
    expect(cleared.type === "contributions" && cleared.contributions).toEqual([]);
    await waitFor(transport, "worker.disposed");
    await expect(running).resolves.toBeUndefined();
  });

  it("rerenders pure surfaces after declared worker-local events", async () => {
    const transport = new MemoryTransport();
    let count = 0;
    const runtime = new UiWorkerRuntime(transport, {
      capabilityOffer: MINIMAL_TERMINAL_CAPABILITIES,
      surfaces: [createPureUiSurface({
        documentId: "main",
        render: () => Stack({ id: "root", children: [
          Text({ id: "count", value: String(count) }),
          Button({ id: "local", label: "Increment", localEvents: ["press"] }),
        ] }),
        onEvent(event) {
          if (event.targetId !== "local" || event.type !== "press") return false;
          count += 1;
          return true;
        },
      })],
    });
    const running = runtime.run();
    transport.push({ type: "capabilities", messageId: "host", capabilities: MINIMAL_TERMINAL_CAPABILITIES });
    await waitFor(transport, "capabilities");
    transport.push({ type: "capabilitySelection", messageId: "selection", selection: {
      protocolVersion: { major: 1, minor: 0 }, primitives: MINIMAL_TERMINAL_CAPABILITIES.primitives,
      capabilities: [], contributionPoints: [], imageProtocols: [], colorDepth: 1,
      unicode: false, mouse: false, screenReader: false, viewport: MINIMAL_TERMINAL_CAPABILITIES.viewport,
      limits: DEFAULT_UI_HARD_LIMITS,
    } });
    const snapshot = await waitFor(transport, "snapshot");
    if (snapshot.type !== "snapshot" || snapshot.snapshot.document.root.kind !== "element") throw new Error("expected element snapshot");
    const local = snapshot.snapshot.document.root.children[1];
    if (local?.kind !== "element") throw new Error("expected local action element");
    expect(local).toMatchObject({ id: "local", props: { eventHandlers: ["press"] } });
    transport.push({
      type: "event", messageId: "press-1", event: {
        protocolVersion: { major: 1, minor: 0 }, eventId: "press-1", documentId: "main", revision: 0,
        targetId: "local", type: "press", payload: null,
      },
    });
    const patch = await waitFor(transport, "patchBatch");
    expect(patch.type === "patchBatch" && JSON.stringify(patch.patchBatch)).toContain("1");
    transport.push({ type: "host.dispose", messageId: "dispose", extensions: { control: {} } });
    await waitFor(transport, "worker.disposed");
    await expect(running).resolves.toBeUndefined();
  });
  it("defaults its message budget to the host's own ceiling", async () => {
    // crates/ui-host/src/runtime.rs kills a worker that exceeds
    // UI_WORKER_MESSAGE_RATE_PER_SECOND + UI_WORKER_MESSAGE_BURST, so the
    // worker's self-imposed budget must be the same numbers: it then fails
    // locally (recoverable) instead of being killed.
    expect(UI_WORKER_MESSAGE_RATE_PER_SECOND).toBe(240);
    expect(UI_WORKER_MESSAGE_BURST).toBe(120);
    const transport = new MemoryTransport();
    let count = 0;
    const runtime = new UiWorkerRuntime(transport, {
      capabilityOffer: MINIMAL_TERMINAL_CAPABILITIES,
      // Only the sustained rate is overridden; the burst stays at its default
      // so the thrown budget is the one a real worker is held to.
      messagesPerSecond: 0,
      surfaces: [createPureUiSurface({
        documentId: "main",
        render: () => Stack({ id: "root", children: [
          Text({ id: "count", value: String(count) }),
          Button({ id: "local", label: "Increment", localEvents: ["press"] }),
        ] }),
        onEvent(event) {
          if (event.targetId !== "local" || event.type !== "press") return false;
          count += 1;
          return true;
        },
      })],
    });
    const running = runtime.run();
    transport.push({ type: "capabilities", messageId: "host", capabilities: MINIMAL_TERMINAL_CAPABILITIES });
    await waitFor(transport, "capabilities");
    transport.push({ type: "capabilitySelection", messageId: "selection", selection: {
      protocolVersion: { major: 1, minor: 0 }, primitives: MINIMAL_TERMINAL_CAPABILITIES.primitives,
      capabilities: [], contributionPoints: [], imageProtocols: [], colorDepth: 1,
      unicode: false, mouse: false, screenReader: false, viewport: MINIMAL_TERMINAL_CAPABILITIES.viewport,
      limits: DEFAULT_UI_HARD_LIMITS,
    } });
    await waitFor(transport, "snapshot");
    for (let index = 0; index < UI_WORKER_MESSAGE_BURST + 8; index += 1) {
      transport.push({
        type: "event", messageId: `press-${index}`, event: {
          protocolVersion: { major: 1, minor: 0 }, eventId: `press-${index}`, documentId: "main", revision: index,
          targetId: "local", type: "press", payload: null,
        },
      });
    }
    await expect(running).rejects.toThrow(`0/s + ${UI_WORKER_MESSAGE_BURST} burst message budget`);
  });
});
