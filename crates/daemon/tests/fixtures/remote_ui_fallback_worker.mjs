import { stdin, stdout, exit } from "node:process";

let buffered = Buffer.alloc(0);
let target = "terminal";

function send(message) {
  const payload = Buffer.from(JSON.stringify(message), "utf8");
  const header = Buffer.alloc(4);
  header.writeUInt32BE(payload.length, 0);
  stdout.write(Buffer.concat([header, payload]));
}

function handle(message) {
  if (message.type === "capabilities") {
    target = message.capabilities?.client === "web" ? "web" : "terminal";
    send({
      type: "capabilities",
      messageId: `fallback-${target}-capabilities`,
      capabilities: message.capabilities,
    });
    return;
  }
  if (message.type === "capabilitySelection") {
    const terminal = target === "terminal";
    const id = terminal ? "fallback-ui.terminal-panel" : "fallback-ui.web-panel";
    const renderer = terminal ? "fallback.TerminalPanel" : "fallback.WebPanel";
    const documentId = terminal ? "fallback-terminal-document" : "fallback-web-document";
    send({
      type: "worker.ready",
      messageId: `fallback-${target}-ready`,
      extensions: { control: { fixture: "fallback" } },
    });
    send({
      type: "snapshot",
      messageId: `fallback-${target}-snapshot`,
      snapshot: {
        document: {
          protocolVersion: { major: 1, minor: 0 },
          documentId,
          revision: 0,
          root: {
            kind: "element",
            id: "root",
            type: "Text",
            props: { value: terminal ? "terminal fallback" : "web surface" },
            children: [],
          },
          metadata: { title: terminal ? "Fallback UI" : "Web UI" },
        },
      },
    });
    send({
      type: "contributions",
      messageId: `fallback-${target}-contribution`,
      extensions: { contributionOwner: "fallback-ui" },
      contributions: [{
        id,
        extensionId: "fallback-ui",
        point: "panel",
        slot: "panel",
        documentId,
        requires: [],
        metadata: { renderer },
      }],
    });
    return;
  }
  if (message.type === "host.ping") {
    send({
      type: "worker.pong",
      messageId: `fallback-${target}-pong`,
      extensions: { control: {} },
    });
    return;
  }
  if (message.type === "host.dispose") {
    send({
      type: "worker.disposed",
      messageId: `fallback-${target}-disposed`,
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
