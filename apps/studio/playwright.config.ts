import { defineConfig } from "@playwright/test"

// Browser tests for the builder surfaces: one test per row of each surface's verb matrix.
// E2E_BASE points them at a deployed stage (`next`) and E2E_STATE at a signed-in Playwright storage state,
// as test/e2e/run.ts does; without E2E_BASE they run against `vite preview` of this build with a stubbed session.
const url = process.env.E2E_BASE

export default defineConfig({
  testDir: "e2e",
  testMatch: "*.e2e.ts",
  timeout: 60_000,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL: url ?? "http://localhost:4173",
    channel: process.env.PLAYWRIGHT_CHANNEL ?? "chrome",
    viewport: { width: 1680, height: 1050 },
    screenshot: "only-on-failure",
    ...(process.env.E2E_STATE ? { storageState: process.env.E2E_STATE } : {}),
  },
  ...(url ? {} : { webServer: { command: "bunx vite preview --port 4173 --strictPort", url: "http://localhost:4173", reuseExistingServer: false, timeout: 60_000 } }),
})
