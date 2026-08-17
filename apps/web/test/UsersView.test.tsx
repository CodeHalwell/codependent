import { describe, it, expect, vi } from "vitest";
import { render, screen, act, fireEvent } from "@testing-library/react";
import { ControlPlaneProvider } from "@codypendent/control-plane-react";
import { UsersView } from "../src/views/UsersView.js";
import { ControlPlaneClient } from "@codypendent/control-plane";

describe("UsersView Component", () => {
  it("renders members table and allows inviting new member", async () => {
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
    vi.spyOn(mockClient, "listRepositories").mockResolvedValue([]);
    vi.spyOn(mockClient, "listTeams").mockResolvedValue([]);

    vi.spyOn(mockClient, "listUsers").mockResolvedValue([
      {
        id: "u-1",
        displayName: "Alice",
        primaryEmail: "alice@acme.com",
        state: "active",
        createdAt: "2026-08-17T00:00:00Z",
        updatedAt: "",
      },
    ]);

    vi.spyOn(mockClient, "listRoleGrants").mockResolvedValue([
      {
        id: "g-1",
        organizationId: "org-1",
        userId: "u-1",
        teamId: null,
        repositoryId: null,
        role: "organization-admin",
        actionScope: null,
        grantedBy: "u-1",
        grantedAt: "",
        expiresAt: null,
        revokedAt: null,
      },
    ]);

    const inviteSpy = vi.spyOn(mockClient, "inviteUser").mockResolvedValue({
      id: "u-2",
      displayName: "Bob",
      primaryEmail: "bob@acme.com",
      state: "active",
      createdAt: "",
      updatedAt: "",
    });

    render(
      <ControlPlaneProvider client={mockClient} initialOrganizationId="org-1">
        <UsersView />
      </ControlPlaneProvider>
    );

    await act(async () => {
      await new Promise((r) => setTimeout(r, 30));
    });

    expect(screen.getByTestId("members-table")).toBeDefined();
    expect(screen.getByText("Alice")).toBeDefined();
    expect(screen.getByText("organization-admin")).toBeDefined();

    // Open invite modal
    await act(async () => {
      screen.getByTestId("invite-member-btn").click();
      await new Promise((r) => setTimeout(r, 10));
    });

    const emailInput = screen.getByTestId("invite-email-input");
    fireEvent.change(emailInput, { target: { value: "bob@acme.com" } });

    // Submit invite
    await act(async () => {
      screen.getByTestId("send-invite-btn").click();
      await new Promise((r) => setTimeout(r, 30));
    });

    expect(inviteSpy).toHaveBeenCalledWith("org-1", "bob@acme.com", "contributor");
  });
});
