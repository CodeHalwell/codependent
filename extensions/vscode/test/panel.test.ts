import { describe, expect, it } from "vitest";

import { makeNonce, renderPanelHtml } from "../src/webview/panel.js";

describe("webview panel security", () => {
  it("loads the graphical renderer under a nonce-only CSP", () => {
    const html = renderPanelHtml({
      nonce: "fixed-nonce",
      cspSource: "vscode-webview-resource:",
      scriptUri: "vscode-webview-resource:/dist/webview.js",
    });
    expect(html).toContain('id="remote-ui"');
    expect(html).toContain('nonce="fixed-nonce" src="vscode-webview-resource:/dist/webview.js"');
    expect(html).toContain("default-src 'none'");
    expect(html).not.toContain("'unsafe-eval'");
    expect(html).not.toContain("'unsafe-inline'");
    expect(html).not.toContain("https:;");
    expect(html).toContain("button:focus-visible");
    expect(html).toContain("overflow-wrap: anywhere");
    expect(html).toContain("prefers-reduced-motion: reduce");
    expect(html).toContain("overscroll-behavior: contain");
    expect(html).toContain("grid-template-areas:");
    expect(html).toContain("ui-host-region-overlay");
  });

  it("generates strong unique CSP nonces", () => {
    const first = makeNonce();
    const second = makeNonce();
    expect(first).toMatch(/^[a-f0-9]{32}$/);
    expect(second).not.toBe(first);
  });
});
