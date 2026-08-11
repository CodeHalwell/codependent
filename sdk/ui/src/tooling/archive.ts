import { createHash } from "node:crypto";
import { readdir, readFile, stat } from "node:fs/promises";
import { join, relative, sep } from "node:path";
import { gzipSync } from "node:zlib";

function octal(value: number, width: number): Uint8Array {
  return Buffer.from(value.toString(8).padStart(width - 1, "0") + "\0", "ascii");
}
function put(header: Uint8Array, offset: number, size: number, value: Uint8Array): void { header.set(value.slice(0, size), offset); }

function tarHeader(name: string, size: number): Uint8Array {
  if (Buffer.byteLength(name) > 100) throw new Error(`package path exceeds portable tar limit: ${name}`);
  const header = new Uint8Array(512);
  put(header, 0, 100, Buffer.from(name)); put(header, 100, 8, octal(0o644, 8)); put(header, 108, 8, octal(0, 8)); put(header, 116, 8, octal(0, 8));
  put(header, 124, 12, octal(size, 12)); put(header, 136, 12, octal(0, 12)); header.fill(0x20, 148, 156); header[156] = "0".charCodeAt(0);
  put(header, 257, 6, Buffer.from("ustar\0")); put(header, 263, 2, Buffer.from("00")); put(header, 265, 32, Buffer.from("codypendent")); put(header, 297, 32, Buffer.from("codypendent"));
  const checksum = header.reduce((sum, byte) => sum + byte, 0);
  put(header, 148, 8, Buffer.from(checksum.toString(8).padStart(6, "0") + "\0 ", "ascii"));
  return header;
}

const EXCLUDED = new Set([".git", "node_modules", ".DS_Store", "plugin.toml"]);
const MAX_PACKAGE_FILES = 10_000;
const MAX_PACKAGE_ENTRIES = 20_000;
const MAX_PACKAGE_DIRECTORIES = 10_000;
const MAX_PACKAGE_BYTES = 256 * 1024 * 1024;
const MAX_PACKAGE_ARCHIVE_BYTES = 10 * 1024 * 1024;

interface WalkState { entries: number; directories: number; }

async function files(root: string, directory = root, state: WalkState = { entries: 0, directories: 0 }): Promise<string[]> {
  const result: string[] = [];
  for (const entry of (await readdir(directory, { withFileTypes: true })).sort((left, right) => left.name.localeCompare(right.name))) {
    if (EXCLUDED.has(entry.name) || entry.name.endsWith(".pem") || entry.name.endsWith(".key") || entry.name.endsWith(".cody-ui.tgz")) continue;
    const path = join(directory, entry.name);
    if (directory === root && !["dist", "assets", "package.json", "README.md", "LICENSE", "LICENSE.md"].includes(entry.name)) continue;
    state.entries += 1;
    if (state.entries > MAX_PACKAGE_ENTRIES) throw new Error(`package exceeds ${MAX_PACKAGE_ENTRIES} entries`);
    if (entry.isSymbolicLink()) throw new Error(`package cannot contain symlink: ${relative(root, path)}`);
    if (entry.isDirectory()) {
      state.directories += 1;
      if (state.directories > MAX_PACKAGE_DIRECTORIES) throw new Error(`package exceeds ${MAX_PACKAGE_DIRECTORIES} directories`);
      result.push(...await files(root, path, state));
    }
    else if (entry.isFile()) result.push(path);
  }
  return result;
}

/** Reproducible ustar+gzip artifact (sorted paths, stable mode/uid/gid/mtime). */
export async function createDeterministicArchive(root: string): Promise<Uint8Array> {
  const chunks: Uint8Array[] = [];
  const packageFiles = await files(root);
  if (packageFiles.length > MAX_PACKAGE_FILES) throw new Error(`package exceeds ${MAX_PACKAGE_FILES} files`);
  let totalBytes = 0;
  for (const path of packageFiles) {
    const info = await stat(path);
    if (info.size > 64 * 1024 * 1024) throw new Error(`package file is too large: ${relative(root, path)}`);
    totalBytes += info.size;
    if (totalBytes > MAX_PACKAGE_BYTES) throw new Error(`package exceeds ${MAX_PACKAGE_BYTES} uncompressed bytes`);
    const content = await readFile(path);
    const name = relative(root, path).split(sep).join("/");
    chunks.push(tarHeader(name, content.byteLength), content);
    const padding = (512 - (content.byteLength % 512)) % 512;
    if (padding > 0) chunks.push(new Uint8Array(padding));
  }
  chunks.push(new Uint8Array(1024));
  const archive = gzipSync(Buffer.concat(chunks), { level: 9 });
  if (archive.byteLength > MAX_PACKAGE_ARCHIVE_BYTES) throw new Error(`compressed package exceeds ${MAX_PACKAGE_ARCHIVE_BYTES} bytes`);
  return archive;
}

export function artifactChecksum(artifact: Uint8Array): string {
  return `sha256:${createHash("sha256").update(artifact).digest("hex")}`;
}
