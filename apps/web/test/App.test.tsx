import { afterEach, describe, it, expect, vi } from "vitest";
import { render, screen, act } from "@testing-library/react";
import { App } from "../src/App.js";
import { ControlPlaneClient } from "@codypendent/control-plane";

describe("Web App", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    window.sessionStorage.clear();
    window.history.replaceState({}, "", "/");
  });

  it("renders layout, sidebar navigation, and navigates between views", async () => {
    window.sessionStorage.setItem("codypendent.controlPlane.accessToken", "test-token");
    vi.spyOn(ControlPlaneClient.prototype, "getCurrentUser").mockResolvedValue({
      id: "u-1",
      displayName: "Alice Tester",
      primaryEmail: "alice@example.com",
      state: "active",
      createdAt: "",
      updatedAt: "",
    });

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
    vi.spyOn(ControlPlaneClient.prototype, "listUsers").mockResolvedValue([]);
    vi.spyOn(ControlPlaneClient.prototype, "listRoleGrants").mockResolvedValue([]);
    vi.spyOn(ControlPlaneClient.prototype, "listApiKeys").mockResolvedValue([]);
    vi.spyOn(ControlPlaneClient.prototype, "listDaemons").mockResolvedValue([]);

    render(<App />);

    await act(async () => {
      await new Promise((r) => setTimeout(r, 30));
    });

    // Verify overview view is active
    expect(screen.getByTestId("overview-view")).toBeDefined();

    // Navigate to Sessions
    await act(async () => {
      screen.getByTestId("nav-sessions").click();
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(screen.getByTestId("sessions-view")).toBeDefined();

    // Navigate to Approvals
    await act(async () => {
      screen.getByTestId("nav-approvals").click();
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(screen.getByTestId("approvals-view")).toBeDefined();

    // Navigate to Audit Logs
    await act(async () => {
      screen.getByTestId("nav-audit").click();
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(screen.getByTestId("audit-view")).toBeDefined();

    // Navigate to Members
    await act(async () => {
      screen.getByTestId("nav-users").click();
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(screen.getByTestId("users-view")).toBeDefined();

    // Navigate to API Keys
    await act(async () => {
      screen.getByTestId("nav-apikeys").click();
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(screen.getByTestId("apikeys-view")).toBeDefined();

    // Navigate to Settings
    await act(async () => {
      screen.getByTestId("nav-settings").click();
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(screen.getByTestId("settings-view")).toBeDefined();
  });

  it("guards every protected hash when there is no authenticated session", async () => {
    window.sessionStorage.clear();
    window.history.replaceState({}, "", "/#sessions");

    render(<App />);

    expect(screen.getByTestId("login-view")).toBeDefined();
    expect(screen.queryByTestId("sessions-view")).toBeNull();
    expect(screen.queryByTestId("app-sidebar")).toBeNull();
  });

  it("keeps protected routes available to a stored bearer token without current-user lookup", async () => {
    window.sessionStorage.setItem("codypendent.controlPlane.accessToken", "stored-token");
    window.history.replaceState({}, "", "/#overview");
    const getCurrentUser = vi.spyOn(ControlPlaneClient.prototype, "getCurrentUser");
    vi.spyOn(ControlPlaneClient.prototype, "listOrganizations").mockResolvedValue([]);

    render(<App />);

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });

    expect(getCurrentUser).toHaveBeenCalled();
    expect(screen.getByRole("heading", { name: "Overview" })).toBeDefined();
    expect(screen.getByRole("complementary", { name: "Main Navigation" })).toBeDefined();
    expect(screen.queryByTestId("login-view")).toBeNull();
    expect(window.sessionStorage.getItem("codypendent.controlPlane.accessToken")).toBe(
      "stored-token",
    );

    await act(async () => {
      screen.getByTestId("user-menu-button").click();
    });
    expect(screen.getByText("Bearer credential configured")).toBeDefined();

    await act(async () => {
      screen.getByTestId("logout-button").click();
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    expect(screen.getByTestId("login-view")).toBeDefined();
    expect(window.sessionStorage.getItem("codypendent.controlPlane.accessToken")).toBeNull();
  });
});
