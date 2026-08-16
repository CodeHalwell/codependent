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
    if (!output.write(frame)) {
      // Every listener and timer registered for this race must be torn down
      // whichever branch wins. A surviving `close` handler would later reject a
      // promise nobody awaits (an `unhandledRejection` kills the worker), and
      // surviving `once` handlers accumulate one pair per backpressured write
      // until Node warns about a listener leak.
      let timer: ReturnType<typeof setTimeout> | undefined;
      let onError: ((err: unknown) => void) | undefined;
      let onClose: (() => void) | undefined;
      const drained = new AbortController();
      try {
        await Promise.race([
          once(output, "drain", { signal: drained.signal }),
          new Promise<void>((_, reject) => {
            timer = setTimeout(() => reject(new Error("Timeout waiting for stream drain")), 10000);
            onError = (err: unknown) => reject(err);
            onClose = () => reject(new Error("Stream closed before drain"));
            output.once("error", onError);
            output.once("close", onClose);
          }),
        ]);
      } finally {
        if (timer !== undefined) clearTimeout(timer);
        if (onError !== undefined) output.removeListener("error", onError);
        if (onClose !== undefined) output.removeListener("close", onClose);
        // Losing branch: `once` keeps its own `drain` listener until settled.
        drained.abort();
      }
    }
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
