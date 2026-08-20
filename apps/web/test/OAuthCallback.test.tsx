import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act } from "@testing-library/react";
import { App } from "../src/App.js";
import { ControlPlaneClient } from "@codypendent/control-plane";

/**
 * The OAuth redirect had no consumer.
 *
 * `LoginView`'s buttons sent the browser to GitHub/OIDC, and nothing in the
 * app ever read the code back: `handleCallback` had zero callers, so no token
 * was installed and no session existed. Pasting a bearer token by hand was the
 * only path that worked — and only until the page reloaded, because nothing
 * persisted it either.
 */

const STORAGE_KEY = "codypendent.controlPlane.accessToken";

const USER = {
  id: "u-1",
  displayName: "Alice Tester",
  primaryEmail: "alice@example.com",
  state: "active" as const,
  createdAt: "",
  updatedAt: "",
};

function stubDataCalls(): void {
  // One organization, as the console needs to render its overview at all.
  vi.spyOn(ControlPlaneClient.prototype, "listOrganizations").mockResolvedValue([
    {
      id: "org-1",
      slug: "acme",
      displayName: "Acme Corp",
      maxPublicationClass: "metadata-shared",
      maxClassification: "internal",
      policyVersion: 1,
      createdAt: "",
      dataResidency: null,
      retentionDays: null,
    },
  ]);
  vi.spyOn(ControlPlaneClient.prototype, "listUsers").mockResolvedValue([]);
  vi.spyOn(ControlPlaneClient.prototype, "listRoleGrants").mockResolvedValue([]);
  vi.spyOn(ControlPlaneClient.prototype, "listApiKeys").mockResolvedValue([]);
  vi.spyOn(ControlPlaneClient.prototype, "listDaemons").mockResolvedValue([]);
  vi.spyOn(ControlPlaneClient.prototype, "listRepositories").mockResolvedValue([]);
  vi.spyOn(ControlPlaneClient.prototype, "listTeams").mockResolvedValue([]);
  vi.spyOn(ControlPlaneClient.prototype, "listSharedSessions").mockResolvedValue({
    items: [],
    cursor: null,
    hasMore: false,
  });
  vi.spyOn(ControlPlaneClient.prototype, "listPendingApprovals").mockResolvedValue([]);
  vi.spyOn(ControlPlaneClient.prototype, "listInbox").mockResolvedValue({
    items: [],
    cursor: null,
    hasMore: false,
  });
  vi.spyOn(ControlPlaneClient.prototype, "listAuditRecords").mockResolvedValue({
    items: [],
    cursor: null,
    hasMore: false,
  });
}

function setUrl(search: string, hash = ""): void {
  window.history.replaceState({}, "", `/${search}${hash}`);
}

async function settle(ms = 40): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, ms));
  });
}

describe("OAuth redirect handling", () => {
  beforeEach(() => {
    window.sessionStorage.clear();
    setUrl("");
  });

  afterEach(() => {
    vi.restoreAllMocks();
    window.sessionStorage.clear();
    setUrl("");
  });

  it("exchanges the authorization code and signs the user in", async () => {
    stubDataCalls();
    const exchange = vi
      .spyOn(ControlPlaneClient.prototype, "handleOAuthCallback")
      .mockResolvedValue({
        user: USER,
        tokens: { accessToken: "minted-token", refreshToken: "r", expiresIn: 3600 },
      } as never);
    vi.spyOn(ControlPlaneClient.prototype, "getCurrentUser").mockResolvedValue(USER);

    setUrl("?code=the-code&state=the-state");
    render(<App />);
    await settle();

    expect(exchange).toHaveBeenCalledWith({ code: "the-code", state: "the-state" });
    expect(screen.getByTestId("overview-view")).toBeDefined();
  });

  it("persists the minted token so a reload keeps the session", async () => {
    stubDataCalls();
    vi.spyOn(ControlPlaneClient.prototype, "handleOAuthCallback").mockResolvedValue({
      user: USER,
      tokens: { accessToken: "minted-token", refreshToken: "r", expiresIn: 3600 },
    } as never);
    vi.spyOn(ControlPlaneClient.prototype, "getCurrentUser").mockResolvedValue(USER);

    setUrl("?code=the-code&state=the-state");
    render(<App />);
    await settle();

    expect(window.sessionStorage.getItem(STORAGE_KEY)).toBe("minted-token");
  });

  it("strips the single-use code from the address bar before exchanging it", async () => {
    stubDataCalls();
    vi.spyOn(ControlPlaneClient.prototype, "handleOAuthCallback").mockResolvedValue({
      user: USER,
      tokens: { accessToken: "minted-token", refreshToken: "r", expiresIn: 3600 },
    } as never);
    vi.spyOn(ControlPlaneClient.prototype, "getCurrentUser").mockResolvedValue(USER);

    setUrl("?code=the-code&state=the-state");
    render(<App />);
    await settle();

    expect(window.location.search).toBe("");
  });

  it("exchanges a code exactly once, even under StrictMode's double invoke", async () => {
    stubDataCalls();
    const exchange = vi
      .spyOn(ControlPlaneClient.prototype, "handleOAuthCallback")
      .mockResolvedValue({
        user: USER,
        tokens: { accessToken: "minted-token", refreshToken: "r", expiresIn: 3600 },
      } as never);
    vi.spyOn(ControlPlaneClient.prototype, "getCurrentUser").mockResolvedValue(USER);

    setUrl("?code=the-code&state=the-state");
    const { StrictMode } = await import("react");
    render(
      <StrictMode>
        <App />
      </StrictMode>
    );
    await settle();

    // An authorization code is single-use: a second exchange fails and would
    // turn a successful sign-in into a failed one.
    expect(exchange).toHaveBeenCalledTimes(1);
  });

  it("reports a refusal from the identity provider instead of failing silently", async () => {
    stubDataCalls();
    vi.spyOn(ControlPlaneClient.prototype, "getCurrentUser").mockResolvedValue(USER);

    setUrl("?error=access_denied&error_description=The+user+declined");
    render(<App />);
    await settle();

    expect(screen.getByTestId("callback-error").textContent).toContain("The user declined");
  });

  it("reports a failed exchange rather than dropping the user on a blank console", async () => {
    stubDataCalls();
    vi.spyOn(ControlPlaneClient.prototype, "handleOAuthCallback").mockRejectedValue(
      new Error("state mismatch")
    );
    vi.spyOn(ControlPlaneClient.prototype, "getCurrentUser").mockResolvedValue(USER);

    setUrl("?code=the-code&state=stale");
    render(<App />);
    await settle();

    expect(screen.getByTestId("callback-error").textContent).toContain("state mismatch");
  });

  it("restores a stored token on load, so a reload is not a sign-out", async () => {
    stubDataCalls();
    const getUser = vi
      .spyOn(ControlPlaneClient.prototype, "getCurrentUser")
      .mockResolvedValue(USER);
    window.sessionStorage.setItem(STORAGE_KEY, "stored-token");

    render(<App />);
    await settle();

    // A token was present, so the session was actually looked up rather than
    // short-circuited to signed-out.
    expect(getUser).toHaveBeenCalled();
    expect(screen.getByTestId("overview-view")).toBeDefined();
  });
});
