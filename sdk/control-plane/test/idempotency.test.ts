import { describe, it, expect } from "vitest";
import { generateIdempotencyKey, sha256Hex } from "../src/utils/idempotency.js";
import { encodeCursor, decodeCursor } from "../src/utils/cursor.js";

describe("idempotency and cursor utilities", () => {
  it("generates unique idempotency keys", () => {
    const key1 = generateIdempotencyKey();
    const key2 = generateIdempotencyKey();
    expect(key1).not.toBe(key2);
    expect(key1.length).toBeGreaterThan(10);
  });

  it("computes reproducible sha256 hex digests", async () => {
    const hash1 = await sha256Hex("hello world");
    const hash2 = await sha256Hex("hello world");
    expect(hash1).toBe(hash2);
    expect(hash1).toBe("b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9");
  });

  it("encodes and decodes keyset pagination cursors", () => {
    const cursor = {
      timestamp: "2026-08-17T10:00:00Z",
      id: "item-123",
      queryHash: "hash-456",
    };

    const encoded = encodeCursor(cursor);
    expect(typeof encoded).toBe("string");
    expect(encoded).not.toContain("+");
    expect(encoded).not.toContain("/");

    const decoded = decodeCursor(encoded);
    expect(decoded).toEqual(cursor);
  });

  it("gracefully returns null for malformed cursor strings", () => {
    expect(decodeCursor("not-a-valid-base64-json")).toBeNull();
  });
});
