import { describe, expect, it } from "vitest";

import type { Envelope, Payload } from "../src/envelope.js";
import { encodeEnvelope, FrameDecoder, FrameError, MAX_FRAME_BYTES } from "../src/framing.js";

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

function concatenate(...arrays: Uint8Array[]): Uint8Array {
  const result = new Uint8Array(arrays.reduce((length, array) => length + array.length, 0));
  let offset = 0;
  for (const array of arrays) {
    result.set(array, offset);
    offset += array.length;
  }
  return result;
}

function lengthPrefix(length: number): Uint8Array {
  const prefix = new Uint8Array(4);
  new DataView(prefix.buffer).setUint32(0, length, false);
  return prefix;
}

function envelope(payload: Payload, sequence?: number): Envelope {
  return {
    protocol_version: { major: 1, minor: 0 },
    message_id: "11111111-1111-1111-1111-111111111111",
    client_id: "22222222-2222-2222-2222-222222222222",
    ...(sequence === undefined ? {} : { sequence }),
    payload,
  };
}

describe("daemon framing", () => {
  it("writes the big-endian byte length and round-trips an envelope", () => {
    const original = envelope({ type: "Ping" });
    const frame = encodeEnvelope(original);

    expect(new DataView(frame.buffer, frame.byteOffset, 4).getUint32(0, false)).toBe(frame.length - 4);
    expect(textDecoder.decode(frame.subarray(4))).toBe(JSON.stringify(original));
    expect(new FrameDecoder().push(frame)).toEqual([original]);
  });

  it("reassembles a frame fragmented at every byte boundary", () => {
    const original = envelope({ type: "Pong" }, 42);
    const frame = encodeEnvelope(original);
    const decoder = new FrameDecoder();
    const decoded: Envelope[] = [];

    for (const byte of frame) decoded.push(...decoder.push(new Uint8Array([byte])));

    expect(decoded).toEqual([original]);
    expect(decoder.pendingBytes).toBe(0);
  });

  it("decodes multiple frames and retains a trailing partial frame", () => {
    const first = encodeEnvelope(envelope({ type: "Ping" }, 1));
    const second = encodeEnvelope(envelope({ type: "Pong" }, 2));
    const third = encodeEnvelope(envelope({ type: "Ping" }, 3));
    const split = Math.floor(third.length / 2);
    const decoder = new FrameDecoder();

    expect(decoder.push(concatenate(first, second, third.subarray(0, split)))).toEqual([
      envelope({ type: "Ping" }, 1),
      envelope({ type: "Pong" }, 2),
    ]);
    expect(decoder.pendingBytes).toBe(split);
    expect(decoder.push(third.subarray(split))).toEqual([envelope({ type: "Ping" }, 3)]);
    expect(decoder.pendingBytes).toBe(0);
  });

  it("rejects an oversized declared length as soon as its prefix is complete", () => {
    const prefix = lengthPrefix(MAX_FRAME_BYTES + 1);
    const decoder = new FrameDecoder();

    expect(() => decoder.push(prefix.subarray(0, 3))).not.toThrow();
    expect(() => decoder.push(prefix.subarray(3))).toThrow(FrameError);
  });

  it("allows the maximum declared length while waiting for its body", () => {
    const prefix = lengthPrefix(MAX_FRAME_BYTES);

    expect(new FrameDecoder().push(prefix)).toEqual([]);
  });

  it("rejects an encoded payload larger than the frame limit", () => {
    const oversized = envelope({
      type: "Error",
      code: "oversized",
      message: "x".repeat(MAX_FRAME_BYTES),
      retryable: false,
    });

    expect(() => encodeEnvelope(oversized)).toThrow(FrameError);
  });

  it("rejects malformed JSON in a completed frame", () => {
    const body = textEncoder.encode("{not json");
    const prefix = lengthPrefix(body.length);

    expect(() => new FrameDecoder().push(concatenate(prefix, body))).toThrow(FrameError);
  });
});
