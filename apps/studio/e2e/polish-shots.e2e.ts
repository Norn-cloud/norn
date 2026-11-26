import { readdirSync, readFileSync } from "node:fs"
import AxeBuilder from "@axe-core/playwright"
import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

/**
 * Studio design-bar visual polish & conformance suite (DESIGN.md §16.2 & §18):
 * Visits Studio's primary routes in Light (1440px), Dark (1440px), Mobile (390px), and RTL (`dir="rtl"`),
 * asserting zero horizontal overflow and zero serious/critical axe accessibility violations.
 * When `SHOTS=<dir>` is set, writes full screenshots for visual inspection.
 */

const SHOTS = process.env.SHOTS
const root = new URL("../../../", import.meta.url)
const read = (path: string) => JSON.parse(readFileSync(new URL(path, root), "utf8")) as unknown
const base = read("fixtures/demo/explorer/three-systems.definitions.json") as { artifactId: string }[]
const seen = new Set(base.map(({ artifactId }) => artifactId))
const scans = readdirSync(new URL("fixtures/packs/scans/artifacts/", root))
  .filter((name) => name.endsWith(".json"))
  .map((name) => read(`fixtures/packs/scans/artifacts/${name}`) as { artifactId: string })
  .filter(({ artifactId }) => !seen.has(artifactId))

const inboxFixture = {
  scanLimit: 500,
  sections: [
    { kind: "held_back", count: 3, shown: 1, approximate: false },
    { kind: "decision", count: 1, shown: 1, approximate: false },
    { kind: "source_error", count: 0, shown: 0, approximate: false },
  ],
  items: [
    {
      kind: "held_back",
      ruleRef: {
        kind: "law",
        artifactId: "canon.ItemizedDocument.total_matches_lines.law",
        lawId: "canon.ItemizedDocument.total_matches_lines",
        version: "sha256:1",
      },
      title: "Totals match lines",
      rationale: "Every itemized document total must equal the sum of its lines.",
      objectType: "canon.OrderLine",
      count: 3,
      approximate: false,
      target: "/objects/canon.OrderLine?population=held_back",
      records: [],
    },
    {
      kind: "decision",
      decision: {
        id: "dec_demo_001",
        question: "Approve stock adjustment for Warehouse B?",
        risk: "medium",
        flow: { ruleId: "inventory.reconcile.flow.rule", version: 1, title: "Inventory reconciliation" },
        step: { id: "approve_adjustment", label: "Approve adjustment" },
        requestedAtMs: 1_732_600_000_000,
        expiresAtMs: 1_900_000_000_000,
      },
      target: "/decisions?open=dec_demo_001",
    },
  ],
}

const decisionsFixture = {
  items: [
    {
      id: "dec_demo_001",
      run: { id: "run_demo_001", generation: 1, state: "waiting" },
      flow: { ruleId: "inventory.reconcile.flow.rule", version: 1, title: "Inventory reconciliation" },
      step: { id: "approve_adjustment", label: "Approve adjustment" },
      question: "Approve stock adjustment for Warehouse B?",
      risk: "medium",
      options: [
        { id: "approve", label: "Approve", scope: "decider", effect: { action: "approve_adjustment" } },
        { id: "reject", label: "Reject", scope: "decider", effect: { action: "reject_adjustment" } },
      ],
      agentPick: "approve",
      requestedAtMs: 1_732_600_000_000,
      expiresAtMs: 1_900_000_000_000,
      context: { subjects: [{ objectType: "canon.Holding", id: "demo/holding/001", label: "Holding 001" }] },
      inputSchema: null,
    },
  ],
  total: 1,
  nextCursor: null,
  serverNowMs: 1_732_600_100_000,
}

const dashboardsFixture = {
  items: [
    {
      id: "studio.dashboard.overview",
      title: "Weekly operations overview",
      version: 2,
      updatedAtMs: 1_732_600_000_000,
    },
    {
      id: "studio.dashboard.quality",
      title: "Data quality & rule holdings",
      version: 1,
      updatedAtMs: 1_732_550_000_000,
    },
  ],
  cursor: null,
}

async function setupCell(page: Page) {
  await session(page)
  await page.route("**/api/host/v1/**", (route) => {
    const url = new URL(route.request().url())
    const path = url.pathname
    if (path.endsWith("/v1/definitions:list")) return route.fulfill({ json: dashboardsFixture })
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: [...base, ...scans] } })
    if (path.endsWith("/v1/inbox")) return route.fulfill({ json: inboxFixture })
    if (path.endsWith("/v1/decisions/requested")) return route.fulfill({ json: { items: [], total: 0, nextCursor: null } })
    if (path.endsWith("/v1/decisions")) return route.fulfill({ json: decisionsFixture })
    if (path.endsWith("/v1/studio/views")) return route.fulfill({ json: { items: [], cursor: null } })
    if (path.endsWith("/v1/metrics")) return route.fulfill({ json: { metrics: [] } })
    if (path.endsWith("/v1/sources/connections")) return route.fulfill({ json: { connections: [] } })
    if (path.endsWith("/v1/counts")) return route.fulfill({ json: { counts: { "canon.Holding": 12, "canon.OrderLine": 48 } } })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
}

const PAGES = [
  { name: "home", path: "/" },
  { name: "inbox-decisions", path: "/decisions" },
  { name: "sources", path: "/sources" },
  { name: "map", path: "/map" },
  { name: "dashboards", path: "/dashboards" },
  { name: "rules-laws", path: "/rules" },
] as const

interface ModeSpec {
  readonly name: string
  readonly width: number
  readonly height: number
  readonly scheme: "light" | "dark"
  readonly locale: "en" | "ar"
  readonly fullPage: boolean
}

const MODES: readonly ModeSpec[] = [
  { name: "light-1440", width: 1440, height: 900, scheme: "light", locale: "en", fullPage: false },
  { name: "dark-1440", width: 1440, height: 900, scheme: "dark", locale: "en", fullPage: false },
  { name: "mobile-390", width: 390, height: 844, scheme: "light", locale: "en", fullPage: true },
  { name: "rtl-1440", width: 1440, height: 900, scheme: "light", locale: "ar", fullPage: false },
]

test.describe("Studio polish matrix (Light, Dark, Mobile 390px, RTL)", () => {
  test.skip(Boolean(process.env.E2E_BASE), "uses fixture cell routes")

  for (const mode of MODES) {
    test(`${mode.name}: zero horizontal overflow and clean axe across primary routes`, async ({ page }) => {
      test.setTimeout(120_000)
      await page.addInitScript((locale: string) => {
        try {
          localStorage.setItem("norn.studio.locale", locale)
          localStorage.setItem("norn.studio.skin", "luma")
          localStorage.setItem("norn.studio.shell", "sidebar")
        } catch { /* ignore */ }
      }, mode.locale)
      await setupCell(page)
      await page.setViewportSize({ width: mode.width, height: mode.height })
      await page.emulateMedia({ colorScheme: mode.scheme })

      for (const route of PAGES) {
        await page.goto(route.path)
        await expect(page.locator("h1").first(), `${mode.name} ${route.path} heading`).toBeVisible({ timeout: 20_000 })
        await expect(page.getByText("Connecting…", { exact: true })).toHaveCount(0)
        if (mode.locale === "ar") {
          await expect.poll(() => page.evaluate(() => document.documentElement.dir)).toBe("rtl")
        }
        await page.waitForTimeout(300)

        // Assert zero horizontal overflow on the document.
        const overflow = await page.evaluate(() => ({
          scrollWidth: document.documentElement.scrollWidth,
          clientWidth: document.documentElement.clientWidth,
        }))
        expect(
          overflow.scrollWidth,
          `${mode.name} ${route.path} horizontal overflow (${overflow.scrollWidth} > ${overflow.clientWidth})`,
        ).toBeLessThanOrEqual(overflow.clientWidth + 1)

        // Run axe accessibility check (critical & serious violations fail).
        const axe = await new AxeBuilder({ page })
          .withTags(["wcag2a", "wcag2aa"])
          .analyze()
        const serious = axe.violations.filter((v) => v.impact === "critical" || v.impact === "serious")
        expect(
          serious.map((v) => `${v.id} (${v.impact}): ${v.nodes.map((n) => n.target.join(" ")).join("; ")}`),
          `${mode.name} ${route.path} axe violations`,
        ).toEqual([])

        if (SHOTS) {
          await page.screenshot({
            path: `${SHOTS}/${route.name}-${mode.name}.png`,
            fullPage: mode.fullPage,
          })
        }
      }
    })
  }
})
