import { describe, it, expect, vi } from "vitest";
import { render, screen, act } from "@testing-library/react";
import { ControlPlaneProvider } from "@codypendent/control-plane-react";
import { SessionsView } from "../src/views/SessionsView.js";
import { ControlPlaneClient } from "@codypendent/control-plane";

describe("SessionsView Component", () => {
  it("renders sessions table, filters by state, and opens session details drawer", async () => {
    const mockClient = new ControlPlaneClient({ baseUrl: "https://control-plane.example.com" });

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
    ]);
    vi.spyOn(mockClient, "listRepositories").mockResolvedValue([
      {
        id: "repo-1",
        organizationId: "org-1",
        federatedId: "a".repeat(64),
        displayName: "Core App",
        maxPublicationClass: "metadata-shared",
        maxClassification: "internal",
        policyVersion: 1,
        createdAt: "",
      },
    ]);
    vi.spyOn(mockClient, "listTeams").mockResolvedValue([]);

    vi.spyOn(mockClient, "listSharedSessions").mockResolvedValue({
      items: [
        {
          id: "session-1",
          organizationId: "org-1",
          repositoryId: "repo-1",
          daemonId: "d-1",
          remoteSessionKey: "sess-deploy-fix",
          class: "content-shared",
          title: "Fix Kubernetes deployment manifest",
          state: "running",
          startedAt: "2026-08-17T10:00:00Z",
          lastActivityAt: "2026-08-17T10:12:00Z",
          tombstonedAt: null,
          updatedAt: "2026-08-17T10:12:00Z",
          repositoryDisplayName: "Core App",
        },
      ],
      cursor: null,
      hasMore: false,
    });

    vi.spyOn(mockClient, "getSharedSession").mockResolvedValue({
      id: "session-1",
      organizationId: "org-1",
      repositoryId: "repo-1",
      daemonId: "d-1",
      remoteSessionKey: "sess-deploy-fix",
      class: "content-shared",
      title: "Fix Kubernetes deployment manifest",
      state: "running",
      startedAt: "2026-08-17T10:00:00Z",
      lastActivityAt: "2026-08-17T10:12:00Z",
      tombstonedAt: null,
      updatedAt: "2026-08-17T10:12:00Z",
      repositoryDisplayName: "Core App",
      steps: [
        {
          id: "step-1",
          stepIndex: 0,
          kind: "planning",
          title: "Analyze deployment errors",
          status: "completed",
          startedAt: "2026-08-17T10:00:05Z",
          summary: "Identified incorrect port in service definition",
        },
      ],
    });

    render(
      <ControlPlaneProvider client={mockClient} initialOrganizationId="org-1">
        <SessionsView />
      </ControlPlaneProvider>
    );

    await act(async () => {
      await new Promise((r) => setTimeout(r, 30));
    });

    // Check table loaded session row
    expect(screen.getByTestId("sessions-table")).toBeDefined();
    expect(screen.getByText("sess-deploy-fix")).toBeDefined();
    expect(screen.getByText("Fix Kubernetes deployment manifest")).toBeDefined();
    expect(screen.getByText("Content Shared")).toBeDefined();

    // Click row to open drawer
    await act(async () => {
      screen.getByTestId("session-row-session-1").click();
      await new Promise((r) => setTimeout(r, 30));
    });

    // Check drawer content
    expect(screen.getByTestId("session-detail-content")).toBeDefined();
    expect(screen.getByText("Analyze deployment errors")).toBeDefined();
    expect(screen.getByText("Identified incorrect port in service definition")).toBeDefined();
  });
});
