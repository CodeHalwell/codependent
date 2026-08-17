import { describe, it, expect, vi } from "vitest";
import { render, screen, act } from "@testing-library/react";
import { ControlPlaneProvider } from "@codypendent/control-plane-react";
import { Switcher } from "../src/components/Switcher.js";
import { ControlPlaneClient } from "@codypendent/control-plane";

describe("Switcher Component", () => {
  it("allows toggling dropdowns and switching organizations and repositories", async () => {
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
        slug: "org-alpha",
        displayName: "Alpha Org",
        maxPublicationClass: "metadata-shared",
        maxClassification: "internal",
        policyVersion: 1,
        createdAt: "",
        dataResidency: null,
        retentionDays: null,
      },
      {
        id: "org-2",
        slug: "org-beta",
        displayName: "Beta Org",
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
            federatedId: "a".repeat(64),
            displayName: "Alpha Repo",
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
          displayName: "Beta Repo",
          maxPublicationClass: "content-shared",
          maxClassification: "internal",
          policyVersion: 1,
          createdAt: "",
        },
      ];
    });

    vi.spyOn(mockClient, "listTeams").mockResolvedValue([]);

    render(
      <ControlPlaneProvider client={mockClient}>
        <Switcher />
      </ControlPlaneProvider>
    );

    await act(async () => {
      await new Promise((r) => setTimeout(r, 30));
    });

    // Verify initial active org is Alpha Org
    const orgButton = screen.getByTestId("org-switcher-button");
    expect(orgButton.textContent).toContain("Alpha Org");

    // Open org dropdown
    await act(async () => {
      orgButton.click();
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(screen.getByTestId("org-dropdown-menu")).toBeDefined();

    // Click Beta Org option
    await act(async () => {
      screen.getByTestId("org-option-org-beta").click();
      await new Promise((r) => setTimeout(r, 30));
    });

    // Check active org is now Beta Org
    expect(screen.getByTestId("org-switcher-button").textContent).toContain("Beta Org");
    // Check active repo updated to Beta Repo
    expect(screen.getByTestId("repo-switcher-button").textContent).toContain("Beta Repo");
  });
});
