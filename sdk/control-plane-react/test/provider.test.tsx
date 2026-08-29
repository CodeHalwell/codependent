import { describe, it, expect, vi } from "vitest";
import { render, screen, act } from "@testing-library/react";
import { ControlPlaneProvider } from "../src/context.js";
import { useOrganizations } from "../src/hooks/useOrganizations.js";
import { useRepositories } from "../src/hooks/useRepositories.js";
import { useAuth } from "../src/hooks/useAuth.js";
import { ControlPlaneClient } from "@codypendent/control-plane";

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
};

function deferred<T>(): Deferred<T> {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function TestConsumer() {
  const { organizations, activeOrganization, setActiveOrganizationId } = useOrganizations();
  const { repositories, activeRepository } = useRepositories();
  const { currentUser, isAuthenticated, logout } = useAuth();

  return (
    <div>
      <div data-testid="auth-status">{isAuthenticated ? "authenticated" : "anonymous"}</div>
      <div data-testid="user-name">{currentUser?.displayName ?? "none"}</div>
      <div data-testid="active-org">{activeOrganization?.slug ?? "no-org"}</div>
      <div data-testid="org-count">{organizations.length}</div>
      <div data-testid="active-repo">{activeRepository?.displayName ?? "no-repo"}</div>
      <div data-testid="repo-count">{repositories.length}</div>
      <button
        data-testid="switch-org-btn"
        onClick={() => setActiveOrganizationId("org-2")}
      >
        Switch Org
      </button>
      <button data-testid="logout-btn" onClick={() => void logout()}>
        Log out
      </button>
    </div>
  );
}

describe("ControlPlaneProvider", () => {
  it("keeps a bearer-token session authenticated when current-user lookup is unsupported", async () => {
    const mockClient = new ControlPlaneClient({ baseUrl: "https://control-plane.example.com" });
    vi.spyOn(mockClient, "listOrganizations").mockResolvedValue([]);

    render(
      <ControlPlaneProvider client={mockClient} token="bearer-token">
        <TestConsumer />
      </ControlPlaneProvider>,
    );

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(screen.getByTestId("auth-status").textContent).toBe("authenticated");
    expect(screen.getByTestId("user-name").textContent).toBe("none");
  });

  it("clears an API-key session from provider, HTTP, and stream auth on logout", async () => {
    const mockClient = new ControlPlaneClient({
      baseUrl: "https://control-plane.example.com",
      apiKey: "api-key",
    });
    vi.spyOn(mockClient, "listOrganizations").mockResolvedValue([]);

    render(
      <ControlPlaneProvider client={mockClient} apiKey="api-key">
        <TestConsumer />
      </ControlPlaneProvider>,
    );

    expect(screen.getByTestId("auth-status").textContent).toBe("authenticated");
    await act(async () => {
      screen.getByTestId("logout-btn").click();
      await new Promise((resolve) => setTimeout(resolve, 10));
    });
    expect(screen.getByTestId("auth-status").textContent).toBe("anonymous");
  });

  it("initializes context, loads orgs and repos, and allows switching active organization", async () => {
    const mockClient = new ControlPlaneClient({ baseUrl: "https://control-plane.example.com" });

    vi.spyOn(mockClient, "getCurrentUser").mockResolvedValue({
      id: "u-1",
      displayName: "Alice",
      primaryEmail: "alice@example.com",
      state: "active",
      createdAt: "",
      updatedAt: "",
    });

    vi.spyOn(mockClient, "listOrganizations").mockResolvedValue([
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
      {
        id: "org-2",
        slug: "beta",
        displayName: "Beta Inc",
        maxPublicationClass: "content-shared",
        maxClassification: "internal",
        policyVersion: 1,
        createdAt: "",
        dataResidency: null,
        retentionDays: null,
      },
    ]);

    vi.spyOn(mockClient, "listRepositories").mockImplementation(async (orgId) => {
      if (orgId === "org-1") {
        return [
          {
            id: "repo-1",
            organizationId: "org-1",
            federatedId: "f".repeat(64),
            displayName: "Frontend App",
            maxPublicationClass: "metadata-shared",
            maxClassification: "internal",
            policyVersion: 1,
            createdAt: "",
          },
        ];
      }
      return [
        {
          id: "repo-2",
          organizationId: "org-2",
          federatedId: "b".repeat(64),
          displayName: "Backend API",
          maxPublicationClass: "content-shared",
          maxClassification: "internal",
          policyVersion: 1,
          createdAt: "",
        },
      ];
    });

    vi.spyOn(mockClient, "listTeams").mockResolvedValue([]);

    render(
      <ControlPlaneProvider client={mockClient} token="test-token">
        <TestConsumer />
      </ControlPlaneProvider>
    );

    // Initial render
    expect(screen.getByTestId("auth-status")).toBeDefined();

    // Wait for async load
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(screen.getByTestId("auth-status").textContent).toBe("authenticated");
    expect(screen.getByTestId("user-name").textContent).toBe("Alice");
    expect(screen.getByTestId("active-org").textContent).toBe("acme");
    expect(screen.getByTestId("org-count").textContent).toBe("2");
    expect(screen.getByTestId("active-repo").textContent).toBe("Frontend App");

    // Switch organization
    await act(async () => {
      screen.getByTestId("switch-org-btn").click();
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(screen.getByTestId("active-org").textContent).toBe("beta");
    expect(screen.getByTestId("active-repo").textContent).toBe("Backend API");
  });

  it("drops late team and repository replies from the previous organization", async () => {
    const mockClient = new ControlPlaneClient({ baseUrl: "https://control-plane.example.com" });
    vi.spyOn(mockClient, "getCurrentUser").mockResolvedValue({
      id: "u-1",
      displayName: "Alice",
      primaryEmail: "alice@example.com",
      state: "active",
      createdAt: "",
      updatedAt: "",
    });
    vi.spyOn(mockClient, "listOrganizations").mockResolvedValue([
      {
        id: "org-1", slug: "acme", displayName: "Acme", maxPublicationClass: "metadata-shared",
        maxClassification: "internal", policyVersion: 1, createdAt: "", dataResidency: null,
        retentionDays: null,
      },
      {
        id: "org-2", slug: "beta", displayName: "Beta", maxPublicationClass: "metadata-shared",
        maxClassification: "internal", policyVersion: 1, createdAt: "", dataResidency: null,
        retentionDays: null,
      },
    ]);
    const oldRepositories = deferred<Awaited<ReturnType<ControlPlaneClient["listRepositories"]>>>();
    const oldTeams = deferred<Awaited<ReturnType<ControlPlaneClient["listTeams"]>>>();
    vi.spyOn(mockClient, "listRepositories").mockImplementation((organizationId) =>
      organizationId === "org-1"
        ? oldRepositories.promise
        : Promise.resolve([{
            id: "repo-2", organizationId: "org-2", federatedId: "b".repeat(64),
            displayName: "Beta Repo", maxPublicationClass: "metadata-shared",
            maxClassification: "internal", policyVersion: 1, createdAt: "",
          }]),
    );
    vi.spyOn(mockClient, "listTeams").mockImplementation((organizationId) =>
      organizationId === "org-1" ? oldTeams.promise : Promise.resolve([]),
    );

    render(
      <ControlPlaneProvider client={mockClient} token="test-token">
        <TestConsumer />
      </ControlPlaneProvider>,
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });
    await act(async () => {
      screen.getByTestId("switch-org-btn").click();
      await new Promise((resolve) => setTimeout(resolve, 10));
    });
    expect(screen.getByTestId("active-repo").textContent).toBe("Beta Repo");

    await act(async () => {
      oldRepositories.resolve([{
        id: "repo-1", organizationId: "org-1", federatedId: "a".repeat(64),
        displayName: "Stale Acme Repo", maxPublicationClass: "metadata-shared",
        maxClassification: "internal", policyVersion: 1, createdAt: "",
      }]);
      oldTeams.resolve([]);
      await Promise.all([oldRepositories.promise, oldTeams.promise]);
    });

    expect(screen.getByTestId("active-org").textContent).toBe("beta");
    expect(screen.getByTestId("active-repo").textContent).toBe("Beta Repo");
    expect(screen.getByTestId("repo-count").textContent).toBe("1");
  });
});
