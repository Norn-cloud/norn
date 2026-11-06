import { defineConfig } from "@playwright/test"

// Browser tests for the builder surfaces: one test per row of each surface's verb matrix.
// STUDIO_URL points them at a deployed stage (`next`); without it they run against `vite preview` of this build.
const url = process.env.STUDIO_URL

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
  },
  ...(url ? {} : { webServer: { command: "bunx vite preview --port 4173 --strictPort", url: "http://localhost:4173", reuseExistingServer: false, timeout: 60_000 } }),
})
