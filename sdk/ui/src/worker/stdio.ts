import { stdin, stdout } from "node:process";
import { once } from "node:events";
import type { Writable } from "node:stream";
import { decodeUiFrames, UiFrameWriter, type FrameLimits } from "./framing.js";
import { runUiWorker, type UiWorkerRuntimeOptions, type UiWorkerTransport } from "./runtime.js";
import type { UiWireMessage } from "../protocol.js";

function inputChunks(): AsyncIterable<Uint8Array> {
  return stdin as unknown as AsyncIterable<Uint8Array>;
}

/** Creates the only production transport available to component bundles. */
export function createNodeStreamUiTransport(
  input: AsyncIterable<Uint8Array>,
  output: Writable,
  frameLimits: Partial<FrameLimits> = {},
): UiWorkerTransport {
  const writer = new UiFrameWriter(async (frame) => {
    if (!output.write(frame)) await once(output, "drain");
  }, { maxFrameBytes: frameLimits.maxFrameBytes ?? 8 * 1024 * 1024, maxBufferedBytes: frameLimits.maxBufferedBytes ?? 16 * 1024 * 1024 });
  return {
    incoming: decodeUiFrames(input, frameLimits),
    send: (message: UiWireMessage) => writer.write(message),
    close: () => writer.close(),
  };
}

export function createStdioUiTransport(frameLimits: Partial<FrameLimits> = {}): UiWorkerTransport {
  return createNodeStreamUiTransport(inputChunks(), stdout, frameLimits);
}

/**
 * Starts a sandboxed stdio worker. Components receive only UiProvider's
 * projection/actions interface; this bootstrap does not pass process or stream
 * handles to application code.
 */
export async function runStdioUiWorker(options: UiWorkerRuntimeOptions): Promise<void> {
  await runUiWorker(createStdioUiTransport(), options);
}
