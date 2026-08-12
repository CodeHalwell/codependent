import { createPublicKey, verify as edVerify } from "node:crypto";
import { readFile } from "node:fs/promises";
import { describe, expect, it } from "vitest";
import { parseCanonicalManifest, rustSigningDigest, validateUiManifest } from "@codypendent/ui/tooling";

const manifestUrl = new URL("../plugin.toml", import.meta.url);
const publicKeyUrl = new URL("../publisher.ed25519.pub", import.meta.url);

describe("packaged manifest", () => {
  it("declares only the capability the card actually reads", async () => {
    const manifest = validateUiManifest(await readFile(manifestUrl, "utf8"));
    const ui = manifest.ui as { requested_capabilities: string[]; contributions: { point: string }[] };
    expect(ui.requested_capabilities).toEqual(["workflow-read"]);
    expect(ui.contributions.map((contribution) => contribution.point)).toEqual([
      "dashboard-card",
      "workflow-inspector",
    ]);
  });

  it("carries a real Ed25519 signature over the committed manifest", async () => {
    // The digest covers everything but `security.signature` — including the
    // artifact checksum and the requested capabilities — so this is the same
    // bytes the daemon's verifier checks (`crates/sandbox/src/verify.rs`,
    // cross-checked against `rustSigningDigest` by a golden digest test).
    const source = await readFile(manifestUrl, "utf8");
    const manifest = parseCanonicalManifest(source);
    const security = manifest.security as { checksum: string; signature: string };
    expect(security.checksum).toMatch(/^sha256:[0-9a-f]{64}$/u);
    expect(security.signature).not.toBe("set-during-packaging");

    // The packager writes the raw 32-byte Ed25519 public key; rebuild the SPKI
    // wrapper Node needs to import it.
    const raw = await readFile(publicKeyUrl);
    expect(raw).toHaveLength(32);
    const key = createPublicKey({
      key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), raw]),
      format: "der",
      type: "spki",
    });
    expect(edVerify(null, rustSigningDigest(manifest), key, Buffer.from(security.signature, "base64"))).toBe(true);
  });

  it("rejects a manifest whose capabilities were widened after signing", async () => {
    const tampered = parseCanonicalManifest(
      (await readFile(manifestUrl, "utf8")).replace('requested_capabilities = ["workflow-read"]', 'requested_capabilities = ["workflow-read", "command-invoke"]'),
    );
    const raw = await readFile(publicKeyUrl);
    const key = createPublicKey({
      key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), raw]),
      format: "der",
      type: "spki",
    });
    const signature = Buffer.from((tampered.security as { signature: string }).signature, "base64");
    expect(edVerify(null, rustSigningDigest(tampered), key, signature)).toBe(false);
  });
});
