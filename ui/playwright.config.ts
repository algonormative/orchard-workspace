import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  timeout: 45_000,
  fullyParallel: false,
  use: { baseURL: "http://127.0.0.1:4174", screenshot: "only-on-failure" },
  webServer: { command: "node scripts/fixture-server.mjs", port: 4174, reuseExistingServer: false },
});
