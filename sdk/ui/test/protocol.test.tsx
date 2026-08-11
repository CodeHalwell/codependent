/** @jsxImportSource ../src */
import { describe, expect, it } from "vitest";
import goldenDocument from "./fixtures/ui-document.json";
import {
  Button,
  Image,
  Stack,
  Text,
  createDocument,
  diffDocuments,
  MINIMAL_TERMINAL_CAPABILITIES,
  projectDocument,
  validateDocument,
  type UiDocument,
} from "../src/index.js";

describe("semantic protocol", () => {
  it("creates deterministic, valid wire trees from pure TSX", () => {
    const view = (
      <Stack id="root" gap="sm" accessibleLabel="Build summary">
        <Text id="heading" role="heading">Build results</Text>
        <Button id="open" action="build.open" label="Open failures" />
      </Stack>
    );
    const document = createDocument(view, { documentId: "doc" });
    expect(validateDocument(document)).toEqual({ valid: true, issues: [] });
    expect(document.root).toMatchObject({ kind: "element", id: "root", type: "Stack" });
  });

  it("parses the cross-language golden document", () => {
    expect(validateDocument(goldenDocument as UiDocument)).toEqual({ valid: true, issues: [] });
  });

  it("emits move patches for stable keyed nodes", () => {
    const before = createDocument(
      <Stack id="root"><Text id="a">A</Text><Text id="b">B</Text></Stack>,
      { documentId: "moves", revision: 1 },
    );
    const after = createDocument(
      <Stack id="root"><Text id="b">B</Text><Text id="a">A</Text></Stack>,
      { documentId: "moves", revision: 2 },
    );
    expect(diffDocuments(before, after).patches).toEqual(expect.arrayContaining([
      { op: "move", nodeId: "b", parentId: "root", index: 0 },
    ]));
  });

  it("projects unsupported media to its terminal fallback", () => {
    const document = createDocument(
      <Image id="preview" src="artifact://image" alt="Screenshot" fallback={<Text id="fallback">Screenshot unavailable</Text>} />,
      { documentId: "fallback" },
    );
    expect(projectDocument(document, MINIMAL_TERMINAL_CAPABILITIES).root).toMatchObject({ id: "fallback", kind: "element", type: "Text" });
  });
});
