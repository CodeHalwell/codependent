import { describe, it, expect, vi } from "vitest";
import { render, screen, act, fireEvent } from "@testing-library/react";
import { ControlPlaneProvider } from "@codypendent/control-plane-react";
import { ApiKeysView } from "../src/views/ApiKeysView.js";
import { ControlPlaneClient } from "@codypendent/control-plane";

describe("ApiKeysView Component", () => {
  it("renders API keys list and generates a new API key with copy token display", async () => {
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

    vi.spyOn(mockClient, "listApiKeys").mockResolvedValue([
      {
        id: "key-1",
        organizationId: "org-1",
        name: "Existing Key",
        keyPrefix: "cody_live_ex",
        role: "observer",
        createdAt: "2026-08-17T00:00:00Z",
        lastUsedAt: null,
        expiresAt: null,
        revokedAt: null,
      },
    ]);
    vi.spyOn(mockClient, "listDaemons").mockResolvedValue([]);

    const createKeySpy = vi.spyOn(mockClient, "createApiKey").mockResolvedValue({
      apiKey: {
        id: "key-2",
        organizationId: "org-1",
        name: "Deploy Key",
        keyPrefix: "cody_live_dep",
        role: "contributor",
        createdAt: "2026-08-17T01:00:00Z",
        lastUsedAt: null,
        expiresAt: null,
        revokedAt: null,
      },
      token: "cody_live_dep_secret_token_123456",
    });

    render(
      <ControlPlaneProvider client={mockClient} initialOrganizationId="org-1">
        <ApiKeysView />
      </ControlPlaneProvider>
    );

    await act(async () => {
      await new Promise((r) => setTimeout(r, 30));
    });

    expect(screen.getByTestId("api-keys-table")).toBeDefined();
    expect(screen.getByText("Existing Key")).toBeDefined();

    // Click Generate API Key
    await act(async () => {
      screen.getByTestId("create-key-btn").click();
      await new Promise((r) => setTimeout(r, 10));
    });

    const keyNameInput = screen.getByTestId("key-name-input");
    fireEvent.change(keyNameInput, { target: { value: "Deploy Key" } });

    await act(async () => {
      screen.getByTestId("confirm-create-key-btn").click();
      await new Promise((r) => setTimeout(r, 30));
    });

    expect(createKeySpy).toHaveBeenCalled();
    expect(screen.getByTestId("secret-token-display")).toBeDefined();
    expect((screen.getByTestId("secret-token-display") as HTMLInputElement).value).toBe(
      "cody_live_dep_secret_token_123456"
    );
  });
});
