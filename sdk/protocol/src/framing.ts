/**
 * The daemon's length-prefixed JSON framing.
 *
 * Each frame is a four-byte, big-endian payload length followed by the UTF-8
 * JSON bytes of one envelope. This mirrors `crates/protocol/src/framing.rs`.
 */
import type { Envelope } from "./envelope.js";

/** Frames larger than this are a protocol violation (16 MiB). */
export const MAX_FRAME_BYTES = 16 * 1024 * 1024;

const LENGTH_PREFIX_BYTES = 4;
const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

/** A framing-layer failure (an oversized frame or malformed JSON body). */
export class FrameError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "FrameError";
  }
}

/** Serialize one envelope and prepend its big-endian payload length. */
export function encodeEnvelope(envelope: Envelope): Uint8Array {
  const body = textEncoder.encode(JSON.stringify(envelope));
  if (body.length > MAX_FRAME_BYTES) {
    throw new FrameError(`frame of ${body.length} bytes exceeds MAX_FRAME_BYTES`);
  }

  const frame = new Uint8Array(LENGTH_PREFIX_BYTES + body.length);
  new DataView(frame.buffer).setUint32(0, body.length, false);
  frame.set(body, LENGTH_PREFIX_BYTES);
  return frame;
}

/** Incrementally decode complete envelopes from arbitrary stream chunks. */
export class FrameDecoder {
  private buffer = new Uint8Array(0);

  /**
   * Append a stream chunk and return all newly completed envelopes.
   * A trailing partial prefix or body remains buffered for the next call.
   */
  push(chunk: Uint8Array): Envelope[] {
    const buffered = new Uint8Array(this.buffer.length + chunk.length);
    buffered.set(this.buffer);
    buffered.set(chunk, this.buffer.length);
    this.buffer = buffered;

    const envelopes: Envelope[] = [];
    for (;;) {
      if (this.buffer.length < LENGTH_PREFIX_BYTES) return envelopes;

      const length = new DataView(
        this.buffer.buffer,
        this.buffer.byteOffset,
        LENGTH_PREFIX_BYTES,
      ).getUint32(0, false);
      if (length > MAX_FRAME_BYTES) {
        throw new FrameError(`frame of ${length} bytes exceeds MAX_FRAME_BYTES`);
      }

      const frameEnd = LENGTH_PREFIX_BYTES + length;
      if (this.buffer.length < frameEnd) return envelopes;

      const body = this.buffer.subarray(LENGTH_PREFIX_BYTES, frameEnd);
      let envelope: Envelope;
      try {
        envelope = JSON.parse(textDecoder.decode(body)) as Envelope;
      } catch (cause) {
        throw new FrameError(
          `frame body is not valid JSON: ${cause instanceof Error ? cause.message : String(cause)}`,
        );
      }

      envelopes.push(envelope);
      // Copy the tail so a drained frame does not retain the larger input slab.
      this.buffer = this.buffer.slice(frameEnd);
    }
  }

  /** Number of bytes currently held in an incomplete frame. */
  get pendingBytes(): number {
    return this.buffer.length;
  }

  /** Discard all unparsed buffer state. */
  clear(): void {
    this.buffer = new Uint8Array(0);
  }
}
