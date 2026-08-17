import { describe, it, expect, vi } from "vitest";
import { render, screen, act } from "@testing-library/react";
import { ControlPlaneProvider } from "@codypendent/control-plane-react";
import { AuditLogsView } from "../src/views/AuditLogsView.js";
import { ControlPlaneClient } from "@codypendent/control-plane";

describe("AuditLogsView Component", () => {
  it("renders audit table and triggers hash chain verification", async () => {
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

    vi.spyOn(mockClient, "listAuditRecords").mockResolvedValue({
      items: [
        {
          id: "rec-1",
          organizationId: "org-1",
          actorKind: "user",
          actorId: "u-1",
          action: "role.grant",
          targetKind: "role_grant",
          targetId: "grant-101",
          actionDigest: "abcdef1234567890",
          correlationId: "corr-1",
          prevHash: null,
          recordHash: "hash-001",
          detail: { role: "contributor" },
          occurredAt: "2026-08-17T10:00:00Z",
        },
      ],
      cursor: null,
      hasMore: false,
    });

    vi.spyOn(mockClient, "verifyAuditChain").mockResolvedValue({
      valid: true,
      totalRecordsChecked: 1,
      message: "Audit chain cryptographically verified: 1 records intact",
    });

    render(
      <ControlPlaneProvider client={mockClient} initialOrganizationId="org-1">
        <AuditLogsView />
      </ControlPlaneProvider>
    );

    await act(async () => {
      await new Promise((r) => setTimeout(r, 30));
    });

    expect(screen.getByTestId("audit-table")).toBeDefined();
    expect(screen.getByText("role.grant")).toBeDefined();
    expect(screen.getByText("role_grant:grant-101")).toBeDefined();

    // Click verify hash chain button
    await act(async () => {
      screen.getByTestId("verify-audit-chain-btn").click();
      await new Promise((r) => setTimeout(r, 30));
    });

    // Check verification banner appeared
    expect(screen.getByTestId("audit-verification-banner")).toBeDefined();
    expect(screen.getByText(/Audit chain cryptographically verified/)).toBeDefined();
  });
});
