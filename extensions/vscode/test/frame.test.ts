import { describe, expect, it } from "vitest";

import {
  encodeEnvelope,
  FrameDecoder,
  FrameError,
  MAX_FRAME_BYTES,
  type Envelope as GeneratedEnvelope,
} from "@codypendent/protocol";
import { PROTOCOL_V1, type Envelope, type Payload } from "../src/protocol/types.js";

/**
 * `src/protocol/types.ts` mirrors the same wire the generated bindings
 * describe, narrowed to the shapes the extension consumes (the generated view
 * widens every optional to `T | null | undefined`). The serialized JSON is
 * identical, so handing a mirrored envelope to the generated codec is a
 * re-view, not a conversion.
 */
function generated(value: Envelope): GeneratedEnvelope {
  return value as unknown as GeneratedEnvelope;
}

function envelope(payload: Payload, overrides: Partial<Envelope> = {}): Envelope {
  return {
    protocol_version: PROTOCOL_V1,
    message_id: "11111111-1111-1111-1111-111111111111",
    client_id: "22222222-2222-2222-2222-222222222222",
    payload,
    ...overrides,
  };
}

function ping(): Envelope {
  return envelope({ type: "Ping" });
}

describe("frame codec", () => {
  it("round-trips an envelope through encode -> decode", () => {
    const original = envelope({ type: "Pong" }, { sequence: 7 });
    const decoder = new FrameDecoder();
    const out = decoder.push(encodeEnvelope(generated(original))) as Envelope[];
    expect(out).toHaveLength(1);
    expect(out[0]).toEqual(original);
    expect(decoder.pendingBytes).toBe(0);
  });

  it("writes a 4-byte big-endian length prefix", () => {
    const frame = encodeEnvelope(generated(ping()));
    const declaredLen = Buffer.from(frame).readUInt32BE(0);
    expect(declaredLen).toBe(frame.length - 4);
    // The body is exactly the JSON bytes.
    expect(Buffer.from(frame).subarray(4).toString("utf8")).toBe(JSON.stringify(ping()));
  });

  it("reassembles a frame delivered one byte at a time across chunk boundaries", () => {
    const frame = encodeEnvelope(generated(envelope({ type: "Ping" }, { sequence: 42 })));
    const decoder = new FrameDecoder();
    const collected: Envelope[] = [];
    for (let i = 0; i < frame.length; i += 1) {
      const produced = decoder.push(frame.subarray(i, i + 1)) as Envelope[];
      collected.push(...produced);
      // Nothing emitted until the very last byte completes the frame.
      if (i < frame.length - 1) {
        expect(produced).toHaveLength(0);
      }
    }
    expect(collected).toHaveLength(1);
    expect(collected[0].sequence).toBe(42);
    expect(decoder.pendingBytes).toBe(0);
  });

  it("splits at an arbitrary boundary in the middle of the length prefix", () => {
    const frame = encodeEnvelope(generated(ping()));
    const decoder = new FrameDecoder();
    expect(decoder.push(frame.subarray(0, 2))).toHaveLength(0); // partial prefix
    expect(decoder.push(frame.subarray(2, 5))).toHaveLength(0); // rest of prefix + 1 body byte
    const rest = decoder.push(frame.subarray(5));
    expect(rest).toHaveLength(1);
    expect(rest[0]).toEqual(ping());
    expect(decoder.pendingBytes).toBe(0);
  });

  it("decodes multiple complete frames packed into a single chunk", () => {
    const f1 = encodeEnvelope(generated(envelope({ type: "Pong" }, { sequence: 1 })));
    const f2 = encodeEnvelope(generated(envelope({ type: "Pong" }, { sequence: 2 })));
    const combined = Buffer.concat([f1, f2]);

    const decoder = new FrameDecoder();
    const out = decoder.push(combined);
    expect(out).toHaveLength(2);
    expect(out.map((e) => e.sequence)).toEqual([1, 2]);
    expect(decoder.pendingBytes).toBe(0);
  });

  it("rejects an envelope declared larger than MAX_FRAME_BYTES", () => {
    // Manufacture a prefix declaring MAX_FRAME_BYTES + 1.
    const prefix = Buffer.alloc(4);
    prefix.writeUInt32BE(MAX_FRAME_BYTES + 1, 0);

    const decoder = new FrameDecoder();
    expect(() => decoder.push(prefix)).toThrow(FrameError);
  });

  it("rejects non-JSON payload bytes when completing a frame", () => {
    const garbage = Buffer.from("this is definitely not valid json {{{");
    const prefix = Buffer.alloc(4);
    prefix.writeUInt32BE(garbage.length, 0);

    const decoder = new FrameDecoder();
    expect(() => decoder.push(Buffer.concat([prefix, garbage]))).toThrow(FrameError);
  });

  it("resets internal state on clear()", () => {
    const frame = encodeEnvelope(generated(envelope({ type: "Pong" })));
    const decoder = new FrameDecoder();
    decoder.push(frame.subarray(0, 10)); // partially ingest
    expect(decoder.pendingBytes).toBeGreaterThan(0);

    decoder.clear();
    expect(decoder.pendingBytes).toBe(0);

    // After clear, decoding a fresh frame succeeds cleanly.
    const fresh = decoder.push(frame);
    expect(fresh).toHaveLength(1);
  });
});
