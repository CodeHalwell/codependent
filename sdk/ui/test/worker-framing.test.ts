import { describe, expect, it } from "vitest";
import { decodeUiFrames, encodeUiFrame, UiFrameError, UiFrameWriter } from "../src/worker/framing.js";

async function* chunks(values: Uint8Array[]): AsyncGenerator<Uint8Array> { for (const value of values) yield value; }

describe("worker framing", () => {
  it("decodes split headers/payloads and coalesced frames", async () => {
    const first = encodeUiFrame({ type: "one" });
    const second = encodeUiFrame({ type: "two" });
    const joined = new Uint8Array(first.length + second.length);
    joined.set(first); joined.set(second, first.length);
    const pieces = [joined.slice(0, 2), joined.slice(2, 9), joined.slice(9)];
    const decoded: unknown[] = [];
    for await (const value of decodeUiFrames(chunks(pieces))) decoded.push(value);
    expect(decoded).toEqual([{ type: "one" }, { type: "two" }]);
  });

  it("rejects oversized, invalid, and truncated frames", async () => {
    expect(() => encodeUiFrame({ long: "x".repeat(100) }, 10)).toThrow(UiFrameError);
    const truncated = encodeUiFrame({ ok: true }).slice(0, 7);
    await expect(async () => { for await (const _ of decodeUiFrames(chunks([truncated]))) { /* drain */ } }).rejects.toMatchObject({ code: "truncated-frame" });
  });

  it("serializes writes and bounds producer backpressure", async () => {
    let release: (() => void) | undefined;
    const writes: Uint8Array[] = [];
    const writer = new UiFrameWriter(async (frame) => {
      writes.push(frame);
      await new Promise<void>((resolve) => { release = resolve; });
    }, { maxFrameBytes: 1024, maxBufferedBytes: 80 });
    const first = writer.write({ value: "a".repeat(20) });
    await Promise.resolve();
    await expect(writer.write({ value: "b".repeat(60) })).rejects.toMatchObject({ code: "buffer-overflow" });
    release?.(); await first;
    expect(writes).toHaveLength(1);
  });
});
