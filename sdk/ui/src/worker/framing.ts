const HEADER_BYTES = 4;

export interface FrameLimits {
  maxFrameBytes: number;
  maxBufferedBytes: number;
}

export const DEFAULT_FRAME_LIMITS: FrameLimits = {
  maxFrameBytes: 8 * 1024 * 1024,
  maxBufferedBytes: 16 * 1024 * 1024,
};

export class UiFrameError extends Error {
  constructor(readonly code: "frame-too-large" | "buffer-overflow" | "invalid-json" | "truncated-frame", message: string) {
    super(message);
    this.name = "UiFrameError";
  }
}

function append(left: Uint8Array, right: Uint8Array): Uint8Array {
  if (left.byteLength === 0) return right.slice();
  const joined = new Uint8Array(left.byteLength + right.byteLength);
  joined.set(left);
  joined.set(right, left.byteLength);
  return joined;
}

function uint32BigEndian(bytes: Uint8Array, offset = 0): number {
  return (((bytes[offset] ?? 0) * 0x1000000)
    + ((bytes[offset + 1] ?? 0) << 16)
    + ((bytes[offset + 2] ?? 0) << 8)
    + (bytes[offset + 3] ?? 0)) >>> 0;
}

/** Incrementally decodes the host's u32-BE + UTF-8 JSON stdio transport. */
export async function* decodeUiFrames(
  chunks: AsyncIterable<Uint8Array>,
  overrides: Partial<FrameLimits> = {},
): AsyncGenerator<unknown> {
  const limits = { ...DEFAULT_FRAME_LIMITS, ...overrides };
  let buffered = new Uint8Array();
  for await (const chunk of chunks) {
    if (chunk.byteLength === 0) continue;
    if (buffered.byteLength + chunk.byteLength > limits.maxBufferedBytes) {
      throw new UiFrameError("buffer-overflow", `worker input exceeds ${limits.maxBufferedBytes} buffered bytes`);
    }
    buffered = append(buffered, chunk);
    while (buffered.byteLength >= HEADER_BYTES) {
      const length = uint32BigEndian(buffered);
      if (length === 0 || length > limits.maxFrameBytes) {
        throw new UiFrameError("frame-too-large", `invalid UI frame length ${length}; maximum is ${limits.maxFrameBytes}`);
      }
      if (buffered.byteLength < HEADER_BYTES + length) break;
      const payload = buffered.slice(HEADER_BYTES, HEADER_BYTES + length);
      buffered = buffered.slice(HEADER_BYTES + length);
      try {
        yield JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(payload)) as unknown;
      } catch (cause) {
        throw new UiFrameError("invalid-json", `invalid UTF-8 JSON frame: ${cause instanceof Error ? cause.message : String(cause)}`);
      }
    }
  }
  if (buffered.byteLength !== 0) {
    throw new UiFrameError("truncated-frame", `transport ended with ${buffered.byteLength} unconsumed bytes`);
  }
}

export function encodeUiFrame(value: unknown, maximumBytes = DEFAULT_FRAME_LIMITS.maxFrameBytes): Uint8Array {
  const payload = new TextEncoder().encode(JSON.stringify(value));
  if (payload.byteLength === 0 || payload.byteLength > maximumBytes) {
    throw new UiFrameError("frame-too-large", `encoded UI frame is ${payload.byteLength} bytes; maximum is ${maximumBytes}`);
  }
  const framed = new Uint8Array(HEADER_BYTES + payload.byteLength);
  const view = new DataView(framed.buffer);
  view.setUint32(0, payload.byteLength, false);
  framed.set(payload, HEADER_BYTES);
  return framed;
}

export type FrameWrite = (frame: Uint8Array) => Promise<void>;

/** Serializes writes and rejects producer-side queue growth before allocation runs away. */
export class UiFrameWriter {
  #tail: Promise<void> = Promise.resolve();
  #pendingBytes = 0;
  #closed = false;

  constructor(
    private readonly writeFrame: FrameWrite,
    private readonly limits: FrameLimits = DEFAULT_FRAME_LIMITS,
  ) {}

  get pendingBytes(): number { return this.#pendingBytes; }

  write(value: unknown): Promise<void> {
    if (this.#closed) return Promise.reject(new Error("UI frame writer is closed"));
    const frame = encodeUiFrame(value, this.limits.maxFrameBytes);
    if (this.#pendingBytes + frame.byteLength > this.limits.maxBufferedBytes) {
      return Promise.reject(new UiFrameError("buffer-overflow", `worker output queue exceeds ${this.limits.maxBufferedBytes} bytes`));
    }
    this.#pendingBytes += frame.byteLength;
    const operation = this.#tail.then(() => this.writeFrame(frame));
    this.#tail = operation.catch(() => undefined).finally(() => { this.#pendingBytes -= frame.byteLength; });
    return operation;
  }

  async close(): Promise<void> {
    this.#closed = true;
    await this.#tail;
  }
}
