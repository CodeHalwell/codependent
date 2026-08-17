import { describe, it, expect, vi } from "vitest";
import { render, screen, act, fireEvent } from "@testing-library/react";
import { ControlPlaneProvider } from "@codypendent/control-plane-react";
import { ApprovalsView } from "../src/views/ApprovalsView.js";
import { ControlPlaneClient } from "@codypendent/control-plane";

describe("ApprovalsView Component", () => {
  it("renders pending approvals and allows approving with custom reason", async () => {
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

    vi.spyOn(mockClient, "listPendingApprovals").mockResolvedValue([
      {
        id: "appr-99",
        organizationId: "org-1",
        repositoryId: "repo-1",
        daemonId: "d-1",
        action: "command_run",
        targetKind: "command",
        targetId: "kubectl apply",
        actionDigest: "sha256-1122334455",
        description: "Execute deployment command",
        requestedAt: "2026-08-17T10:00:00Z",
        expiresAt: null,
        status: "pending",
        decidedBy: null,
        decidedAt: null,
        decisionReason: null,
      },
    ]);

    vi.spyOn(mockClient, "listInbox").mockResolvedValue({
      items: [],
      cursor: null,
      hasMore: false,
    });

    const decideSpy = vi.spyOn(mockClient, "decideApproval").mockResolvedValue({
      approvalId: "appr-99",
      status: "approved",
      decidedAt: "2026-08-17T10:05:00Z",
      receiptId: "rcpt-99",
    });

    render(
      <ControlPlaneProvider client={mockClient} initialOrganizationId="org-1">
        <ApprovalsView />
      </ControlPlaneProvider>
    );

    await act(async () => {
      await new Promise((r) => setTimeout(r, 30));
    });

    expect(screen.getByTestId("approval-card-appr-99")).toBeDefined();
    expect(screen.getByText("Execute deployment command")).toBeDefined();

    // Click Approve
    await act(async () => {
      screen.getByTestId("approve-btn-appr-99").click();
      await new Promise((r) => setTimeout(r, 10));
    });

    const reasonInput = screen.getByTestId("decision-reason-input");
    fireEvent.change(reasonInput, { target: { value: "Approved for prod release" } });

    // Confirm Decision
    await act(async () => {
      screen.getByTestId("confirm-decision-btn").click();
      await new Promise((r) => setTimeout(r, 30));
    });

    expect(decideSpy).toHaveBeenCalledWith("org-1", "appr-99", {
      decision: "approve",
      actionDigest: "sha256-1122334455",
      reason: "Approved for prod release",
    });
  });
});
