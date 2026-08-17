export interface DecodedCursor {
  timestamp: string;
  id: string;
  queryHash?: string;
}

/**
 * Encodes keyset cursor into a base64url string.
 */
export function encodeCursor(cursor: DecodedCursor): string {
  const json = JSON.stringify(cursor);
  if (typeof btoa === "function") {
    return btoa(json).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }
  return Buffer.from(json).toString("base64url");
}

/**
 * Decodes base64url keyset cursor.
 */
export function decodeCursor(cursorStr: string): DecodedCursor | null {
  try {
    let base64 = cursorStr.replace(/-/g, "+").replace(/_/g, "/");
    while (base64.length % 4) {
      base64 += "=";
    }
    const json = typeof atob === "function" ? atob(base64) : Buffer.from(base64, "base64").toString("utf-8");
    return JSON.parse(json) as DecodedCursor;
  } catch {
    return null;
  }
}
