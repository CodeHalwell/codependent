import { describe, it, expect } from "vitest";
import { createDocument, Box, Text } from "../src/index.js";

describe("@codypendent/remote-ui authoring SDK", () => {
  it("creates valid UiDocuments via components", () => {
    const root = Box({ border: true, children: [Text({ value: "Hello from TSX Remote UI" })] });
    const doc = createDocument(root, { documentId: "doc-1" });

    expect(doc.documentId).toBe("doc-1");
    expect(doc.root.kind).toBe("element");
    if (doc.root.kind === "element") {
      expect(doc.root.type).toBe("Box");
      expect(doc.root.children.length).toBe(1);
    }
  });
});
