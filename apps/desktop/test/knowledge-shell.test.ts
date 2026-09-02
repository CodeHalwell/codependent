/**
 * The shell-backed knowledge transport: what it sends the shell, in what
 * order, and what happens when a step fails.
 *
 * The four knowledge views used to be honest dead ends — every surface named
 * the bridge command it was waiting for. The shell now serves them, and this
 * pins the webview side of the contract: the command names the views name
 * in their unavailable panels, the argument shapes the Rust commands take,
 * and the lease sequence a document edit is composed of.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn<(command: string, args?: Record<string, unknown>) => Promise<unknown>>();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args?: Record<string, unknown>) => invoke(command, args),
  Channel: class {},
}));

import { createKnowledgeTransport } from "../src/transport.js";

function inShell<T>(run: () => T): T {
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
  try {
    return run();
  } finally {
    delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
  }
}

describe("createKnowledgeTransport", () => {
  beforeEach(() => {
    invoke.mockReset();
  });

  it("is absent outside the shell, so the views say so rather than pretend", () => {
    expect(createKnowledgeTransport()).toBeNull();
  });

  it("names the commands the views name in their unavailable panels", async () => {
    const transport = inShell(() => createKnowledgeTransport());
    expect(transport).not.toBeNull();
    invoke.mockResolvedValue([]);
    await transport!.listSkills();
    await transport!.listMemories();
    await transport!.listLearnings();
    await transport!.listDocuments();
    await transport!.listUiPlugins();
    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      "list_skills",
      "list_memories",
      "list_learnings",
      "list_documents",
      "list_ui_plugins",
    ]);
  });

  it("sends a mutation with the arguments the shell's command takes", async () => {
    const transport = inShell(() => createKnowledgeTransport())!;
    invoke.mockResolvedValue("learning pinned");
    const outcome = await transport.mutateLearning("l-1", 4, { type: "SetPinned", pinned: true });
    expect(outcome).toBe("learning pinned");
    expect(invoke).toHaveBeenCalledWith("mutate_learning", {
      learningId: "l-1",
      revision: 4,
      mutation: { type: "SetPinned", pinned: true },
    });
    await transport.enableUiPlugin("plug", "session");
    expect(invoke).toHaveBeenLastCalledWith("enable_ui_plugin", { pluginId: "plug", scope: "session" });
  });

  /** One document whose only block still holds `editable`. */
  function documentWith(editable: string) {
    return [
      {
        document_id: "doc-1",
        title: "Runbook",
        scope: "repository",
        status: "active",
        mode: "edit",
        revision: "r1",
        blocks: [{ id: "b1", kind: "paragraph", text: editable, editable }],
        suggestions: [],
      },
    ];
  }

  it("replaces a block under its own lease, deleting the original's code points", async () => {
    const transport = inShell(() => createKnowledgeTransport())!;
    invoke.mockImplementation(async (command) => {
      if (command === "acquire_document_lease") return { lease_id: "lease-9" };
      // Re-read UNDER the lease: the text is still what the editor opened.
      if (command === "list_documents") return documentWith("🙂🙃");
      return undefined;
    });
    // Two astral code points: `.length` would say 4 and over-delete.
    await transport.replaceDocumentBlock("doc-1", "b1", "🙂🙃", "hello");
    expect(invoke.mock.calls).toEqual([
      ["acquire_document_lease", { documentId: "doc-1", blockId: "b1" }],
      ["list_documents", undefined],
      [
        "mutate_document",
        {
          documentId: "doc-1",
          mutation: { op: "edit_text", block_id: "b1", position: 0, delete_len: 2, insert: "hello" },
        },
      ],
      ["release_document_lease", { leaseId: "lease-9" }],
    ]);
  });

  it("refuses a full replacement when the block moved before the lease was held", async () => {
    // The editor compared against its cached projection, which document writes
    // are not streamed into, and the lease is only taken inside the transport.
    // Another writer editing in that window would have had their text deleted
    // by this full replacement, so the re-read under the lease refuses.
    const transport = inShell(() => createKnowledgeTransport())!;
    invoke.mockImplementation(async (command) => {
      if (command === "acquire_document_lease") return { lease_id: "lease-9" };
      if (command === "list_documents") return documentWith("somebody else got here first");
      return undefined;
    });

    await expect(
      transport.replaceDocumentBlock("doc-1", "b1", "the text I opened", "my replacement"),
    ).rejects.toThrow(/changed since you opened it/);

    const commands = invoke.mock.calls.map(([command]) => command);
    expect(commands).not.toContain("mutate_document");
    // The lease is still released, or it blocks that range until it expires.
    expect(commands.at(-1)).toBe("release_document_lease");
  });

  it("refuses a full replacement when the block is gone", async () => {
    const transport = inShell(() => createKnowledgeTransport())!;
    invoke.mockImplementation(async (command) => {
      if (command === "acquire_document_lease") return { lease_id: "lease-9" };
      if (command === "list_documents") return [];
      return undefined;
    });

    await expect(
      transport.replaceDocumentBlock("doc-1", "b1", "the text I opened", "my replacement"),
    ).rejects.toThrow(/no longer in the document/);
    expect(invoke.mock.calls.map(([command]) => command)).not.toContain("mutate_document");
  });

  it("deletes a block under the whole-document lease", async () => {
    const transport = inShell(() => createKnowledgeTransport())!;
    invoke.mockImplementation(async (command) =>
      command === "acquire_document_lease" ? { lease_id: "lease-3" } : undefined,
    );
    await transport.deleteDocumentBlock("doc-1", "b2");
    expect(invoke.mock.calls[0]).toEqual([
      "acquire_document_lease",
      { documentId: "doc-1", blockId: null },
    ]);
    expect(invoke.mock.calls[1]).toEqual([
      "mutate_document",
      { documentId: "doc-1", mutation: { op: "delete", block_id: "b2" } },
    ]);
    expect(invoke.mock.calls[2]).toEqual(["release_document_lease", { leaseId: "lease-3" }]);
  });

  it("releases the lease when the mutation fails, and reports the mutation's error", async () => {
    const transport = inShell(() => createKnowledgeTransport())!;
    invoke.mockImplementation(async (command) => {
      if (command === "acquire_document_lease") {
        return { lease_id: "lease-5" };
      }
      // The re-read under the lease agrees, so the mutation is reached and
      // this test still exercises the failure path it was written for.
      if (command === "list_documents") {
        return documentWith("x");
      }
      if (command === "mutate_document") {
        throw new Error("MutateDocument rejected: stale revision (document.stale)");
      }
      if (command === "release_document_lease") {
        throw new Error("release also failed");
      }
      return undefined;
    });
    await expect(transport.replaceDocumentBlock("doc-1", "b1", "x", "y")).rejects.toThrow(
      "stale revision",
    );
    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      "acquire_document_lease",
      "list_documents",
      "mutate_document",
      "release_document_lease",
    ]);
  });

  it("hands back the parked publish plan", async () => {
    const transport = inShell(() => createKnowledgeTransport())!;
    invoke.mockResolvedValue({
      approval_id: "ap-1",
      target: "docs/branch",
      changed_files: ["docs/a.md"],
      git_action: "commit",
    });
    const plan = await transport.publishDocument("doc-1", {
      kind: "repository_file",
      path: "docs/a.md",
    });
    expect(plan.approval_id).toBe("ap-1");
    expect(invoke).toHaveBeenCalledWith("publish_document", {
      documentId: "doc-1",
      target: { kind: "repository_file", path: "docs/a.md" },
    });
  });
});
