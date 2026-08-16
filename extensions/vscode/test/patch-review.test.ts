import { describe, expect, it } from "vitest";
import { applyUnifiedPatch, assembleVerifiedArtifact, parseUnifiedDiff } from "../src/patch-review.js";

describe("verified patch artifacts", () => {
  it("assembles ordered chunks and verifies sha256", () => {
    const bytes = assembleVerifiedArtifact([
      { offset: 0, bytes: Buffer.from("ab"), eof: false },
      { offset: 2, bytes: Buffer.from("c"), eof: true },
    ], "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    expect(bytes.toString()).toBe("abc");
    expect(() => assembleVerifiedArtifact([{ offset: 0, bytes: Buffer.from("bad"), eof: true }], "00".repeat(32))).toThrow(/hash/i);
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
});
