// Perf harness, deliberately OUTSIDE the gate: it measures rather than asserts,
// and a timing assertion in CI is a flake generator. Run it by hand with
// `npm run perf` before and after a change to the streaming path.
import { defineConfig } from "vitest/config";
import { fileURLToPath } from "node:url";

const fromConfig = (relativePath: string): string =>
  fileURLToPath(new URL(relativePath, import.meta.url));

export default defineConfig({
  resolve: {
    dedupe: ["react", "react-dom"],
    alias: {
      react: fromConfig("./node_modules/react"),
      "react-dom": fromConfig("./node_modules/react-dom"),
      "@codypendent/protocol": fromConfig("../../sdk/protocol/src/index.ts"),
    },
  },
  test: {
    environment: "jsdom",
    include: ["perf/**/*.bench.tsx"],
    setupFiles: ["./test/setup.ts"],
  },
});
