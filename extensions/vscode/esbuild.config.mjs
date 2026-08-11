// esbuild bundle for the VS Code / Cursor extension host entry point.
//
// The extension host loads CommonJS, so we emit `format: "cjs"`, `platform:
// "node"`. The `vscode` module is provided by the host at runtime and must never
// be bundled — it is marked external. Node built-ins stay external automatically
// under `platform: "node"`.
import { build, context } from "esbuild";

const watch = process.argv.includes("--watch");
const production = process.argv.includes("--production");

/** @type {import('esbuild').BuildOptions} */
const extensionOptions = {
  entryPoints: ["src/extension.ts"],
  bundle: true,
  outfile: "dist/extension.js",
  external: ["vscode"],
  format: "cjs",
  platform: "node",
  target: "node18",
  sourcemap: !production,
  minify: production,
  logLevel: "info",
};

/** @type {import('esbuild').BuildOptions} */
const webviewOptions = {
  entryPoints: ["src/webview/remote-ui/main.tsx"],
  bundle: true,
  outfile: "dist/webview.js",
  format: "iife",
  platform: "browser",
  target: "es2022",
  sourcemap: !production,
  minify: production,
  logLevel: "info",
};

if (watch) {
  const [extensionContext, webviewContext] = await Promise.all([
    context(extensionOptions),
    context(webviewOptions),
  ]);
  await Promise.all([extensionContext.watch(), webviewContext.watch()]);
  console.log("esbuild: watching for changes...");
} else {
  await Promise.all([build(extensionOptions), build(webviewOptions)]);
}
