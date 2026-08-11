import { stdin, stdout, exit } from "node:process";

let buffered = Buffer.alloc(0);
let projected = false;

function send(message) {
  const payload = Buffer.from(JSON.stringify(message), "utf8");
  const header = Buffer.alloc(4);
  header.writeUInt32BE(payload.length, 0);
  stdout.write(Buffer.concat([header, payload]));
}

function document(revision, message) {
  return {
    protocolVersion: { major: 1, minor: 0 },
    documentId: "lifecycle-document",
    revision,
    root: {
      kind: "element",
      id: "root",
      type: "Stack",
      props: {},
      children: [
        {
          kind: "element",
          id: "gesture",
          type: "Text",
          props: { value: message },
          children: [],
        },
      ],
    },
    metadata: { title: "Lifecycle worker" },
  };
}

function handle(message) {
  if (message.type === "capabilities") {
    send({
      type: "capabilities",
      messageId: "worker-capabilities",
      capabilities: message.capabilities,
    });
    return;
  }
  if (message.type === "capabilitySelection") {
    send({
      type: "worker.ready",
      messageId: "worker-ready",
      extensions: { control: { fixture: "lifecycle" } },
    });
    send({
      type: "snapshot",
      messageId: "lifecycle-snapshot",
      snapshot: { document: document(0, "ready") },
    });
    send({
      type: "contributions",
      messageId: "lifecycle-contribution",
      extensions: { contributionOwner: "lifecycle-ui" },
      contributions: [{
        id: "lifecycle-ui.panel",
        extensionId: "lifecycle-ui",
        point: "panel",
        slot: "panel",
        documentId: "lifecycle-document",
        requires: ["command-invoke"],
        metadata: { renderer: "lifecycle.Panel" },
      }],
    });
    send({
      type: "subscription",
      messageId: "lifecycle-command-subscription",
      subscription: {
        subscriptionId: "command-core-refresh",
        kind: "command",
        resourceId: "core.refresh",
      },
    });
    send({
      type: "subscription",
      messageId: "lifecycle-context-subscription",
      subscription: {
        subscriptionId: "context-session",
        kind: "context",
      },
    });
    send({
      type: "subscription",
      messageId: "lifecycle-workflow-subscription",
      subscription: {
        subscriptionId: "workflow-lifecycle",
        kind: "workflow",
        resourceId: "lifecycle-workflow",
      },
    });
    send({
      type: "subscription",
      messageId: "lifecycle-artifact-subscription",
      subscription: {
        subscriptionId: "artifact-lifecycle",
        kind: "artifact",
        resourceId: "lifecycle-artifact",
        parameters: {
          includeContent: true,
          maxBytes: 64,
          page: 1,
          pageSize: 16,
        },
      },
    });
    return;
  }
  if (message.type === "projection") {
    projected = message.projection?.subscriptionId === "command-core-refresh"
      && message.projection?.value?.enabled === true;
    return;
  }
  if (message.type === "event" && projected) {
    send({
      type: "action",
      messageId: "lifecycle-action",
      action: {
        invocationId: "lifecycle-invocation",
        documentId: message.event.documentId,
        revision: message.event.revision,
        sourceNodeId: message.event.targetId,
        actionId: "core.refresh",
        payload: { from: "real-worker" },
        interactionToken: message.event.interactionToken,
        interactionEventType: message.event.type,
      },
    });
    return;
  }
  if (message.type === "actionResult") {
    send({
      type: "snapshot",
      messageId: "lifecycle-result-snapshot",
      snapshot: { document: document(1, "action-result-received") },
    });
    return;
  }
  if (message.type === "host.ping") {
    send({
      type: "worker.pong",
      messageId: "lifecycle-pong",
      extensions: { control: {} },
    });
    return;
  }
  if (message.type === "host.dispose") {
    send({
      type: "worker.disposed",
      messageId: "lifecycle-disposed",
      extensions: { control: {} },
    });
    exit(0);
  }
}

stdin.on("data", (chunk) => {
  buffered = Buffer.concat([buffered, chunk]);
  while (buffered.length >= 4) {
    const length = buffered.readUInt32BE(0);
    if (buffered.length < 4 + length) return;
    const payload = buffered.subarray(4, 4 + length);
    buffered = buffered.subarray(4 + length);
    handle(JSON.parse(payload.toString("utf8")));
  }
});
