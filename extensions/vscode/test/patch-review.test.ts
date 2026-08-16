import { describe, expect, it } from "vitest";
import {
  applyUnifiedPatch,
  assembleVerifiedArtifact,
  isWorkspaceUriPath,
  parseUnifiedDiff,
  patchSourcePath,
  readPatchSource,
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

  it("adds and removes the final newline according to EOF markers", () => {
    const add = parseUnifiedDiff("--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n")[0]!;
    const remove = parseUnifiedDiff("--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n\\ No newline at end of file\n")[0]!;
    expect(applyUnifiedPatch(add, "old").after).toBe("new\n");
    expect(applyUnifiedPatch(remove, "old\n").after).toBe("new");
  });

  it("applies EOF markers to context, creation, and deletion", () => {
    const context = parseUnifiedDiff("--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n same\n\\ No newline at end of file\n")[0]!;
    const creation = parseUnifiedDiff("--- /dev/null\n+++ b/a.txt\n@@ -0,0 +1 @@\n+new\n\\ No newline at end of file\n")[0]!;
    const deletion = parseUnifiedDiff("--- a/a.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-old\n\\ No newline at end of file\n")[0]!;
    expect(applyUnifiedPatch(context, "same").after).toBe("same");
    expect(applyUnifiedPatch(creation, "").after).toBe("new");
    expect(applyUnifiedPatch(deletion, "old").after).toBe("");
  });

  it.each([
    "@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n@@ -2 +2 @@\n-two\n+second",
    "@@ -1 +1 @@\n-old\n+new\n\\ No newline at end of file\n@@ -2 +2 @@\n-two\n+second",
  ])("rejects another hunk after an EOF marker", (hunks) => {
    expect(() => parseUnifiedDiff(`--- a/a.txt\n+++ b/a.txt\n${hunks}\n`)).toThrow(/newline marker.*final hunk/i);
  });

  it("validates destination coordinates while applying multiple hunks", () => {
    const valid = parseUnifiedDiff([
      "--- a/a.txt", "+++ b/a.txt",
      "@@ -1 +1 @@", "-one", "+first",
      "@@ -4 +4,2 @@", "-four", "+four-a", "+four-b", "",
    ].join("\n"))[0]!;
    expect(applyUnifiedPatch(valid, "one\ntwo\nthree\nfour\nfive\n").after)
      .toBe("first\ntwo\nthree\nfour-a\nfour-b\nfive\n");

    const deletion = parseUnifiedDiff([
      "--- a/a.txt", "+++ b/a.txt",
      "@@ -1 +1 @@", "-one", "+first",
      "@@ -3 +2,0 @@", "-three", "",
    ].join("\n"))[0]!;
    expect(applyUnifiedPatch(deletion, "one\ntwo\nthree\n").after).toBe("first\ntwo\n");

    for (const newStart of [3, 5]) {
      const malformed = parseUnifiedDiff([
        "--- a/a.txt", "+++ b/a.txt",
        "@@ -1 +1 @@", "-one", "+first",
        `@@ -4 +${newStart} @@`, "-four", "+last", "",
      ].join("\n"))[0]!;
      expect(() => applyUnifiedPatch(malformed, "one\ntwo\nthree\nfour\n"))
        .toThrow(/destination placement/i);
    }
  });

  it.each([
    "@@ -0 +1 @@\n-a\n+b",
    "@@ -1 +0 @@\n-a\n+b",
    "@@ -0,1 +1 @@\n-a\n+b",
    "@@ -1 +0,1 @@\n-a\n+b",
  ])("rejects nonsensical zero hunk coordinate %j", (hunk) => {
    expect(() => parseUnifiedDiff(`--- a/a.txt\n+++ b/a.txt\n${hunk}\n`)).toThrow(/coordinate/i);
  });

  it("preserves ordinary no-marker newline behavior", () => {
    const file = parseUnifiedDiff("--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n")[0]!;
    expect(applyUnifiedPatch(file, "old\n").after).toBe("new\n");
    expect(applyUnifiedPatch(file, "old").after).toBe("new");
  });

  it.each([
    "\\ No newline at end of file",
    "-old\n\\ no newline at end of file",
    "-old\n\\ No newline at end of file!",
    "-old\n\\ No newline at end of file\n\\ No newline at end of file",
  ])("rejects malformed or stray EOF marker %j", (body) => {
    expect(() => parseUnifiedDiff(`--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n${body}\n+new\n`)).toThrow(/newline marker/i);
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

  it.each([
    "..\\..\\secret", "C:\\secret", "C:/secret", "\\\\server\\share\\secret",
    "%2e%2e/secret", "%2fsecret", "%5csecret", "/etc/passwd", "../secret",
    "./secret", "src//secret", "src/../secret", "src/./secret", "src/control\u0000.txt",
  ])("rejects unsafe or non-portable patch path %j", (path) => {
    expect(() => parseUnifiedDiff(`--- a/${path}\n+++ b/${path}\n@@ -1 +1 @@\n-a\n+b\n`)).toThrow(/path/i);
  });

  it("accepts normal nested paths and exact /dev/null creation", () => {
    expect(parseUnifiedDiff("--- a/src/lib/a.ts\n+++ b/src/lib/a.ts\n@@ -1 +1 @@\n-a\n+b\n")[0]).toMatchObject({
      oldPath: "src/lib/a.ts", newPath: "src/lib/a.ts",
    });
    expect(parseUnifiedDiff("--- /dev/null\n+++ b/src/new.ts\n@@ -0,0 +1 @@\n+new\n")[0]?.oldPath).toBe("/dev/null");
  });

  it("uses empty source only for creation and propagates real source read failures", async () => {
    const created = parseUnifiedDiff("--- /dev/null\n+++ b/new.ts\n@@ -0,0 +1 @@\n+new\n")[0]!;
    const modified = parseUnifiedDiff("--- a/old.ts\n+++ b/new.ts\n@@ -1 +1 @@\n-old\n+new\n")[0]!;
    const read = async (): Promise<Uint8Array> => { throw new Error("unreadable"); };
    await expect(readPatchSource(created, read)).resolves.toBe("");
    await expect(readPatchSource(modified, read)).rejects.toThrow("unreadable");
  });
});
