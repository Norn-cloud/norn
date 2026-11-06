// Runs every builder verb suite in a real browser against a deployment, one fresh context per verb.
// Usage (on Boat): E2E_BASE=https://next.norn.cloud E2E_STATE=state.json bun test/e2e/run.ts
// E2E_STATE is a Playwright storage state holding a session cookie; it is deleted once read.
import { readFile, rm } from "node:fs/promises"
import { exitStatus, supported, type E2EPage, type VerbSuite } from "./verbs.ts"
import { everyPage } from "./pages.ts"
import { SUITES } from "./suites.ts"

const base = process.env.E2E_BASE ?? ""
const statePath = process.env.E2E_STATE
if (!/^https:\/\/[a-z0-9.-]+$/.test(base)) throw new Error("E2E_BASE must be an https origin")
const state = statePath ? JSON.parse(await readFile(statePath, "utf8")) : undefined
if (statePath) await rm(statePath, { force: true })
const { chromium } = await import("playwright-core" as string) as { chromium: { launch(o: object): Promise<{ newContext(o: object): Promise<{ newPage(): Promise<unknown>; close(): Promise<void> }>; close(): Promise<void> }> } }
const browser = await chromium.launch({ executablePath: process.env.CHROME ?? "/usr/bin/google-chrome", args: ["--no-sandbox"] })
let failed = 0
let ran = 0
{
  // Every page first: the shared smoke no page may skip.
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, ...state ? { storageState: state } : {} })
  const broken = await everyPage({ page: await context.newPage() as E2EPage, base })
  for (const line of broken) console.log(`FAIL page ${line}`)
  if (broken.length === 0) console.log("PASS every page renders")
  failed += broken.length
  await context.close()
}
for (const suite of SUITES as readonly VerbSuite[]) {
  for (const verb of supported(suite)) {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, ...state ? { storageState: state } : {} })
    const page = await context.newPage() as E2EPage
    const started = Date.now()
    ran += 1
    try {
      await (suite.specs[verb] as (e2e: { page: E2EPage; base: string }) => Promise<void>)({ page, base })
      console.log(`PASS ${suite.kind}: ${verb} (${Date.now() - started}ms)`)
    } catch (error) {
      failed += 1
      console.log(`FAIL ${suite.kind}: ${verb} — ${error instanceof Error ? error.message.split("\n")[0] : String(error)}`)
    }
    await context.close()
  }
}
await browser.close()
console.log(ran === 0 ? "no verb ran" : failed === 0 ? `all ${ran} verbs pass` : `${failed} of ${ran} verb(s) failed`)
process.exit(exitStatus(ran, failed))
