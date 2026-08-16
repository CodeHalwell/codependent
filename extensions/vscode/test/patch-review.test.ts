import { describe, expect, it } from "vitest";
import {
  applyUnifiedPatch,
  assembleVerifiedArtifact,
  isWorkspaceUriPath,
  parseUnifiedDiff,
  patchSourcePath,
  reviewablePatchFiles,
} from "../src/patch-review.js";

describe("verified patch artifacts", () => {
  it("assembles ordered chunks and verifies sha256", () => {
    const bytes = assembleVerifiedArtifact([
      { offset: 0, bytes: Buffer.from("ab"), eof: false },
      { offset: 2, bytes: Buffer.from("c"), eof: true },
    ], "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    expect(bytes.toString()).toBe("abc");
    expect(() => assembleVerifiedArtifact([{ offset: 0, bytes: Buffer.from("bad"), eof: true }], "00".repeat(32))).toThrow(/hash/i);
    expect(() => assembleVerifiedArtifact([
      { offset: 0, bytes: Buffer.from("a"), eof: true },
      { offset: 1, bytes: Buffer.from("b"), eof: true },
    ], "00".repeat(32))).toThrow(/ended before/i);
    expect(() => assembleVerifiedArtifact([
      { offset: 0, bytes: Buffer.alloc(0), eof: false },
      { offset: 0, bytes: Buffer.alloc(0), eof: true },
    ], "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")).toThrow(/progress/i);
  });

  it("rejects malformed unified patches", () => {
    expect(() => parseUnifiedDiff("--- a/a.txt\n+++ b/a.txt\n@@ nope\n")).toThrow(/malformed/i);
  });

  it("parses and applies multiple files", () => {
    const patch = "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n--- /dev/null\n+++ b/b.txt\n@@ -0,0 +1 @@\n+created\n";
    const files = parseUnifiedDiff(patch);
    expect(files.map((file) => file.path)).toEqual(["a.txt", "b.txt"]);
    expect(applyUnifiedPatch(files[0]!, "old\n")).toEqual({ before: "old\n", after: "new\n" });
    expect(applyUnifiedPatch(files[1]!, "")).toEqual({ before: "", after: "created\n" });
  });

  it("accepts ordinary git extended headers and uses the old path for renames", () => {
    const patch = [
      "diff --git a/old.txt b/new.txt",
      "old mode 100644",
      "new mode 100755",
      "similarity index 80%",
      "dissimilarity index 20%",
      "rename from old.txt",
      "rename to new.txt",
      "index 3367afd..f2c27f1 100755",
      "--- a/old.txt",
      "+++ b/new.txt",
      "@@ -1 +1 @@",
      "-old",
      "+new",
      "",
    ].join("\n");
    const [file] = parseUnifiedDiff(patch);
    expect(file).toMatchObject({ oldPath: "old.txt", newPath: "new.txt", path: "new.txt" });
    expect(patchSourcePath(file!)).toBe("old.txt");
    expect(applyUnifiedPatch(file!, "old\n").after).toBe("new\n");
  });

  it("accepts creation/deletion/copy headers and rejects unknown extended headers", () => {
    for (const header of [
      "new file mode 100644", "deleted file mode 100644", "copy from a.txt", "copy to b.txt",
    ]) {
      expect(() => parseUnifiedDiff(`diff --git a/a.txt b/b.txt\n${header}\n--- a/a.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-a\n+b\n`)).not.toThrow();
    }
    expect(() => parseUnifiedDiff("diff --git a/a b/a\nbanana header\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-a\n+b\n")).toThrow(/extended header/i);
  });

  it("enforces the 32-file review boundary without truncation", () => {
    const file = parseUnifiedDiff("--- a/a\n+++ b/a\n@@ -1 +1 @@\n-a\n+b\n")[0]!;
    expect(reviewablePatchFiles(Array.from({ length: 32 }, () => file))).toHaveLength(32);
    expect(() => reviewablePatchFiles(Array.from({ length: 33 }, () => file))).toThrow(/33 files.*32/i);
  });

  it("keeps joined URI paths within the selected workspace", () => {
    expect(isWorkspaceUriPath("/work/repo", "/work/repo/src/a.ts")).toBe(true);
    expect(isWorkspaceUriPath("/work/repo", "/work/repository/a.ts")).toBe(false);
    expect(isWorkspaceUriPath("/work/repo", "/work/repo/../secret")).toBe(false);
  });
});
