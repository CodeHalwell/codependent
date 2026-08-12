import { afterEach, describe, expect, it, vi } from "vitest";
import type { UiWireMessage } from "../src/protocol.js";
import { MediatedUiBridge } from "../src/worker/bridge.js";

describe("mediated worker bridge", () => {
  afterEach(() => vi.useRealTimers());

  it("deduplicates projections and ignores stale or unversioned regressions", async () => {
    const messages: UiWireMessage[] = [];
    const bridge = new MediatedUiBridge(async (message) => { messages.push(message); });
    const artifact = bridge.artifact("artifact-1");
    expect(bridge.artifact("artifact-1")).toBe(artifact);
    const unsubscribe = artifact.subscribe(() => undefined);
    expect(messages.filter((message) => message.type === "subscription")).toHaveLength(1);
    const subscription = messages.find((message) => message.type === "subscription");
    if (subscription?.type !== "subscription") throw new Error("missing subscription");
    expect(bridge.applyProjection({ subscriptionId: subscription.subscription.subscriptionId, revision: 3, value: { id: "artifact-1", mediaType: "application/json", revision: 3, value: { status: "new" } } })).toBe(true);
    expect(bridge.applyProjection({ subscriptionId: subscription.subscription.subscriptionId, revision: 2, value: { id: "artifact-1", mediaType: "application/json", revision: 2, value: { status: "old" } } })).toBe(false);
    expect(bridge.applyProjection({ subscriptionId: subscription.subscription.subscriptionId, value: { id: "artifact-1", mediaType: "application/json", revision: 1, value: { status: "unversioned" } } })).toBe(false);
    expect(artifact.getSnapshot()?.value).toEqual({ status: "new" });
    unsubscribe();
  });

  it("reference-counts projections, suppresses StrictMode churn, and releases bounded IDs", () => {
    vi.useFakeTimers();
    const messages: UiWireMessage[] = [];
    const bridge = new MediatedUiBridge(async (message) => { messages.push(message); });
    const projection = bridge.context("session");
    const first = projection.subscribe(() => undefined);
    first();
    const second = projection.subscribe(() => undefined);
    vi.advanceTimersByTime(100);
    expect(messages.filter((message) => message.type === "subscription")).toHaveLength(1);
    expect(messages.filter((message) => message.type === "unsubscribe")).toHaveLength(0);
    second();
    vi.advanceTimersByTime(50);
    expect(messages.filter((message) => message.type === "unsubscribe")).toHaveLength(1);

    const duplicateCallback = (): void => undefined;
    const duplicate = bridge.run("duplicate-callback");
    const duplicateFirst = duplicate.subscribe(duplicateCallback);
    const duplicateSecond = duplicate.subscribe(duplicateCallback);
    duplicateFirst();
    vi.advanceTimersByTime(100);
    expect(messages.filter((message) => message.type === "unsubscribe")).toHaveLength(1);
    duplicateSecond();
    vi.advanceTimersByTime(50);
    expect(messages.filter((message) => message.type === "unsubscribe")).toHaveLength(2);

    for (let index = 0; index < 100; index += 1) {
      const cell = bridge.artifact(`artifact-${index}`, { page: index, pageSize: 1 });
      cell.subscribe(() => undefined)();
    }
    vi.advanceTimersByTime(50);
    expect(messages.filter((message) => message.type === "unsubscribe")).toHaveLength(102);
    const ranged = bridge.artifact("artifact-0", { offset: 10, length: 20 });
    const stop = ranged.subscribe(() => undefined);
    const rangedMessage = [...messages].reverse().find((message) => message.type === "subscription");
    expect(rangedMessage?.type === "subscription" ? rangedMessage.subscription.parameters : undefined)
      .toEqual({ offset: 10, length: 20 });
    stop();
  });

  it("decodes canonical context and workflow projections", () => {
    const messages: UiWireMessage[] = [];
    const bridge = new MediatedUiBridge(async (message) => { messages.push(message); });
    const context = bridge.context("session");
    const workflow = bridge.workflow("workflow-1");
    const stopContext = context.subscribe(() => undefined);
    const stopWorkflow = workflow.subscribe(() => undefined);
    const subscriptions = messages.filter((message) => message.type === "subscription");
    const contextId = subscriptions.find((message) => message.type === "subscription" && message.subscription.kind === "context")?.subscription.subscriptionId;
    const workflowId = subscriptions.find((message) => message.type === "subscription" && message.subscription.kind === "workflow")?.subscription.subscriptionId;
    if (contextId === undefined || workflowId === undefined) throw new Error("missing subscriptions");
    expect(bridge.applyProjection({ subscriptionId: contextId, revision: 1, value: { activeFile: "src/main.ts", openFiles: ["src/main.ts"], dirtyBuffers: [], diagnosticsRevision: 4 } })).toBe(true);
    expect(bridge.applyProjection({ subscriptionId: workflowId, revision: 1, value: { workflowRunId: "workflow-1", phase: "running", nodes: [{ workflowRunId: "workflow-1", nodeId: "build", state: "running", attempt: 1, warnings: [] }] } })).toBe(true);
    expect(context.getSnapshot()?.activeFile).toBe("src/main.ts");
    expect(workflow.getSnapshot()?.nodes[0]?.nodeId).toBe("build");
    stopContext();
    stopWorkflow();
  });

  it("maps Rust themes and resolves only known, pending actions", async () => {
    const messages: UiWireMessage[] = [];
    const bridge = new MediatedUiBridge(async (message) => { messages.push(message); });
    bridge.updateTheme({ id: "theme", name: "Theme", revision: 2, colorScheme: "dark", tokens: { accent: "#fff", "space.sm": 2 } });
    expect(bridge.theme().getSnapshot()).toEqual({ id: "theme", mode: "dark", colors: { accent: "#fff" }, spacing: { "space.sm": 2 } });

    await expect(bridge.cancel("guessed-id")).rejects.toThrow("unknown UI action");
    expect(messages.some((message) => message.type === "cancelAction")).toBe(false);
    await expect(bridge.invoke("outside-gesture", null)).rejects.toThrow("active document/action context");
    const result = bridge.withActionContext(
      { documentId: "document", revision: 4, sourceNodeId: "root" },
      () => bridge.invoke<null, { ok: boolean }>("refresh", null),
    );
    const action = messages.find((message) => message.type === "action");
    if (action?.type !== "action") throw new Error("missing action");
    expect(action.action).toMatchObject({ documentId: "document", revision: 4, sourceNodeId: "root", actionId: "refresh" });
    expect(bridge.applyActionResult({ invocationId: action.action.invocationId, status: "succeeded", value: { ok: true } })).toBe(true);
    await expect(result).resolves.toEqual({ ok: true });
    expect(bridge.applyActionResult({ invocationId: action.action.invocationId, status: "succeeded", value: null })).toBe(false);
  });

  it("cancels a real pending invocation once", async () => {
    const messages: UiWireMessage[] = [];
    const bridge = new MediatedUiBridge(async (message) => { messages.push(message); });
    const result = bridge.withActionContext(
      { documentId: "d", revision: 0, sourceNodeId: "root" },
      () => bridge.invoke("long-running", null),
    );
    const action = messages.find((message) => message.type === "action");
    if (action?.type !== "action") throw new Error("missing action");
    await bridge.cancel(action.action.invocationId);
    await expect(result).rejects.toMatchObject({ name: "AbortError" });
    expect(messages.filter((message) => message.type === "cancelAction")).toHaveLength(1);
  });
  it("decodes a blackboard projection and preserves fields it does not know", () => {
    const messages: UiWireMessage[] = [];
    const bridge = new MediatedUiBridge(async (message) => { messages.push(message); });
    const board = bridge.blackboard("workflow-1", { kind: "finding" });
    // The same resource + parameters resolve to one shared subscription.
    expect(bridge.blackboard("workflow-1", { kind: "finding" })).toBe(board);
    expect(bridge.blackboard("workflow-1")).not.toBe(board);
    const stop = board.subscribe(() => undefined);
    const subscription = messages.find((message) => message.type === "subscription" && message.subscription.kind === "blackboard");
    if (subscription?.type !== "subscription") throw new Error("missing blackboard subscription");
    expect(subscription.subscription).toMatchObject({ kind: "blackboard", resourceId: "workflow-1", parameters: { kind: "finding" } });

    expect(bridge.applyProjection({
      subscriptionId: subscription.subscription.subscriptionId,
      revision: 1,
      value: {
        workflowRunId: "workflow-1",
        items: [
          {
            id: "item-1", workflowRunId: "workflow-1", kind: "finding", revision: 2,
            payload: { note: "flaky test" }, author: { role: "analyst" }, confidence: 0.8,
            evidence: [{ path: "src/lib.rs" }], supersededBy: "item-2",
            // Columns a later daemon adds must survive the decode.
            boardScope: "run", status: "in-progress",
          },
          { id: "malformed", kind: "finding" },
        ],
      },
    })).toBe(true);

    const snapshot = board.getSnapshot();
    expect(snapshot?.workflowRunId).toBe("workflow-1");
    expect(snapshot?.items).toHaveLength(1);
    expect(snapshot?.items[0]).toMatchObject({
      id: "item-1", kind: "finding", revision: 2, confidence: 0.8, supersededBy: "item-2",
      extra: { boardScope: "run", status: "in-progress" },
    });
    stop();
  });
});
