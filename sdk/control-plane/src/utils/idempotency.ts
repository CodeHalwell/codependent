/**
 * Generates a unique idempotency key (UUID v4 or random hex string).
 */
export function generateIdempotencyKey(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  // Fallback for older environments
  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, (c) => {
    const r = (Math.random() * 16) | 0;
    const v = c === "x" ? r : (r & 0x3) | 0x8;
    return v.toString(16);
  });
}

/**
 * Computes SHA-256 digest of arbitrary UTF-8 string or ArrayBuffer.
 * Works in both Browser and Node.js environments.
 */
export async function sha256Hex(data: string | Uint8Array | ArrayBuffer): Promise<string> {
  let bytes: Uint8Array;
  if (typeof data === "string") {
    bytes = new TextEncoder().encode(data);
  } else if (data instanceof Uint8Array) {
    bytes = data;
  } else {
    bytes = new Uint8Array(data);
  }

  if (typeof crypto !== "undefined" && crypto.subtle && typeof crypto.subtle.digest === "function") {
    const buffer = await crypto.subtle.digest("SHA-256", bytes.buffer as ArrayBuffer);
    const hashArray = Array.from(new Uint8Array(buffer));
    return hashArray.map((b) => b.toString(16).padStart(2, "0")).join("");
  }

  // Node.js crypto fallback
  try {
    const nodeCryptoModuleName = "node:crypto";
    const nodeCrypto = await import(/* @vite-ignore */ nodeCryptoModuleName);
    return nodeCrypto.createHash("sha256").update(bytes).digest("hex");
  } catch {
    throw new Error("No cryptographic digest provider available in this runtime");
  }
}
