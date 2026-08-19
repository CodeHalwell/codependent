/**
 * Where the control-plane access token lives between page loads.
 *
 * `sessionStorage`, not `localStorage`: a reload or a follow-up tab navigation
 * keeps you signed in, but the token does not outlive the browsing session and
 * never reaches disk. For a console that fronts audit logs and approvals, the
 * shorter window is the right default; a deployment that wants
 * remember-me can widen it deliberately rather than inheriting it.
 */
const TOKEN_STORAGE_KEY = "codypendent.controlPlane.accessToken";

/**
 * Storage access throws rather than returning null in a sandboxed iframe and
 * in some privacy modes. A console that cannot persist its token must still
 * load — it just signs the user out on reload — so every access is guarded.
 */
export function readStoredToken(): string | null {
  try {
    return window.sessionStorage.getItem(TOKEN_STORAGE_KEY);
  } catch {
    return null;
  }
}

export function writeStoredToken(token: string | null): void {
  try {
    if (token) {
      window.sessionStorage.setItem(TOKEN_STORAGE_KEY, token);
    } else {
      window.sessionStorage.removeItem(TOKEN_STORAGE_KEY);
    }
  } catch {
    // Persisting is best-effort; the in-memory session still works.
  }
}
