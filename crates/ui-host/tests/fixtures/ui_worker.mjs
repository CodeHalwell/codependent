import { stdin, stdout, stderr, argv, exit } from "node:process";

const mode = argv[2] ?? "normal";
let buffered = Buffer.alloc(0);

function send(message) {
  const payload = Buffer.from(JSON.stringify(message), "utf8");
  const header = Buffer.alloc(4);
  header.writeUInt32BE(payload.length, 0);
  stdout.write(Buffer.concat([header, payload]));
}

function handle(message) {
  if (message.type === "capabilities") {
    if (mode === "crash-handshake") exit(17);
    send({
      type: "capabilities",
      messageId: "worker-capabilities",
      capabilities: message.capabilities,
    });
    return;
  }
  if (message.type === "capabilitySelection") {
    if (mode === "silent-handshake") return;
    send({
      type: "worker.ready",
      messageId: "worker-ready",
      extensions: { control: { fixture: true } },
    });
    if (mode === "stderr") {
      stderr.write("\u001b[31mtoken=do-not-leak\u001b[0m\nordinary diagnostic\n");
    }
    if (mode === "stderr-flood") {
      stderr.write(Buffer.alloc(2 * 1024 * 1024, 0x78));
    }
    if (mode === "flood") {
      for (let index = 0; index < 100; index += 1) {
        send({
          type: "worker.pong",
          messageId: `flood-${index}`,
          extensions: { control: {} },
        });
      }
    }
    if (mode === "bad-direction") {
      send({
        type: "event",
        messageId: "spoofed-event",
        event: {
          protocolVersion: { major: 1, minor: 0 },
          eventId: "evil",
          documentId: "document",
          revision: 1,
          targetId: "root",
          type: "press",
        },
      });
    }
    if (mode === "bad-subscription") {
      send({
        type: "subscription",
        messageId: "unauthorized-subscription",
        subscription: {
          subscriptionId: "artifact-secret",
          kind: "artifact",
          resourceId: "secret",
        },
      });
    }
    return;
  }
  if (message.type === "host.ping" && mode !== "no-pong") {
    send({
      type: "worker.pong",
      messageId: "worker-pong",
      extensions: { control: {} },
    });
    return;
  }
  if (message.type === "hotReload") {
    send({
      type: "worker.reloaded",
      messageId: "worker-reloaded",
      extensions: { control: {} },
    });
    return;
  }
  if (message.type === "resync") {
    send({
      type: "resync",
      messageId: "worker-resync",
      resync: message.resync,
    });
    return;
  }
  if (message.type === "host.dispose") {
    send({
      type: "worker.disposed",
      messageId: "worker-disposed",
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
