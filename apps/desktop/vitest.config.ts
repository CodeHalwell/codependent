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
    include: ["test/**/*.test.ts", "test/**/*.test.tsx"],
    setupFiles: ["./test/setup.ts"],
  },
});
