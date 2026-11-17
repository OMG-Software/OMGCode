import { defineConfig } from "vitest/config";

export default defineConfig({
  base: "./",
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    modulePreload: { polyfill: false },
    rollupOptions: {
      input: {
        index: "index.html",
        approvals: "approvals.html",
      },
    },
  },
  test: {
    environment: "jsdom",
    include: ["tests/**/*.test.ts"],
    // Used by `npm run coverage` only. The report feeds the Sonar analysis;
    // its paths are written relative to the repository root, which is where
    // that analysis runs.
    coverage: {
      provider: "v8",
      include: ["src/**/*.ts"],
      reporter: ["text-summary", ["lcovonly", { projectRoot: "../../.." }]],
      reportsDirectory: "coverage",
    },
  },
});
