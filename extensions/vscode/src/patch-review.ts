import { createHash } from "node:crypto";

export interface ArtifactPart { offset: number; bytes: Buffer; eof: boolean }
export interface PatchHunk { oldStart: number; oldCount: number; newStart: number; newCount: number; lines: string[] }
export interface PatchedFile { path: string; oldPath: string; newPath: string; hunks: PatchHunk[] }

export function assembleVerifiedArtifact(parts: ArtifactPart[], expectedSha256: string): Buffer {
  const chunks: Buffer[] = [];
  let offset = 0;
  for (const part of parts) {
    if (part.offset !== offset) throw new Error("artifact chunks are not contiguous");
    chunks.push(part.bytes);
    offset += part.bytes.length;
  }
  if (parts.length === 0 || !parts.at(-1)?.eof) throw new Error("artifact ended without a final chunk");
  const bytes = Buffer.concat(chunks);
  const actual = createHash("sha256").update(bytes).digest("hex");
  if (actual !== expectedSha256.toLowerCase()) throw new Error("artifact hash verification failed");
  return bytes;
}

function cleanPath(header: string): string {
  const path = header.split("\t", 1)[0] ?? "";
  if (path === "/dev/null") return path;
  const clean = path.replace(/^[ab]\//, "");
  if (!clean || clean.startsWith("/") || clean.split("/").includes("..")) throw new Error("malformed patch path");
  return clean;
}

export function parseUnifiedDiff(source: string): PatchedFile[] {
  const lines = source.split("\n");
  const files: PatchedFile[] = [];
  let i = 0;
  while (i < lines.length && lines[i] === "") i++;
  while (i < lines.length) {
    if (lines[i]?.startsWith("diff --git ")) i++;
    if (!lines[i]?.startsWith("--- ") || !lines[i + 1]?.startsWith("+++ ")) throw new Error("malformed unified patch headers");
    const oldPath = cleanPath(lines[i]!.slice(4));
    const newPath = cleanPath(lines[i + 1]!.slice(4));
    i += 2;
    const hunks: PatchHunk[] = [];
    while (i < lines.length && !lines[i]?.startsWith("--- ") && !lines[i]?.startsWith("diff --git ")) {
      if (lines[i] === "") { i++; continue; }
      const match = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(lines[i]!);
      if (!match) throw new Error("malformed unified patch hunk");
      const hunk: PatchHunk = { oldStart: Number(match[1]), oldCount: Number(match[2] ?? 1), newStart: Number(match[3]), newCount: Number(match[4] ?? 1), lines: [] };
      i++;
      while (i < lines.length && !lines[i]?.startsWith("--- ") && !lines[i]?.startsWith("diff --git ") && /^[ +\-\\]/.test(lines[i]!)) {
        if (!lines[i]!.startsWith("\\")) hunk.lines.push(lines[i]!);
        i++;
      }
      const oldCount = hunk.lines.filter((line) => line[0] !== "+").length;
      const newCount = hunk.lines.filter((line) => line[0] !== "-").length;
      if (oldCount !== hunk.oldCount || newCount !== hunk.newCount) throw new Error("malformed unified patch hunk counts");
      hunks.push(hunk);
    }
    if (hunks.length === 0) throw new Error("malformed unified patch: no hunks");
    files.push({ oldPath, newPath, path: newPath === "/dev/null" ? oldPath : newPath, hunks });
  }
  if (files.length === 0) throw new Error("malformed unified patch: no files");
  return files;
}

export function applyUnifiedPatch(file: PatchedFile, original: string): { before: string; after: string } {
  const hadNewline = original.endsWith("\n");
  const input = original === "" ? [] : original.replace(/\n$/, "").split("\n");
  const output: string[] = [];
  let cursor = 0;
  for (const hunk of file.hunks) {
    const start = hunk.oldStart === 0 ? 0 : hunk.oldStart - 1;
    if (start < cursor || start > input.length) throw new Error("patch hunk is outside the source document");
    output.push(...input.slice(cursor, start));
    cursor = start;
    for (const line of hunk.lines) {
      const text = line.slice(1);
      if (line[0] !== "+") {
        if (input[cursor] !== text) throw new Error("patch context does not match source document");
        cursor++;
      }
      if (line[0] !== "-") output.push(text);
    }
  }
  output.push(...input.slice(cursor));
  const after = output.join("\n") + (output.length > 0 && (hadNewline || file.oldPath === "/dev/null") ? "\n" : "");
  return { before: original, after };
}
