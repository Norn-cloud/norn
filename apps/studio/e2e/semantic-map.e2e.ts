import { readFileSync } from "node:fs"
import { expect, test, type Page, type TestInfo } from "@playwright/test"
import { session } from "./object-cell.ts"

// SEMANTIC MAP (#1705): truncated inbox counts read as lower bounds, a source error is placed only by
// the mapping the server names, status reads in words, filtering fades only decoration, focus
// follows drill-in and Back, arrows follow the reading direction, and the phone skeleton holds the
// page's layout while it loads.

const definitions = JSON.parse(readFileSync(new URL("../../../fixtures/demo/explorer/three-systems.definitions.json", import.meta.url), "utf8")) as unknown

/** One held-back group: one record held by one rule. */
const group = (objectType: string, n: number) => ({
  kind: "held_back", ruleRef: { kind: "law", artifactId: `law.rule_${n}`, lawId: `law.rule_${n}`, version: "sha256:1" },
  title: `Rule ${n}`, rationale: null, objectType, count: 1, approximate: false,
  target: `/objects/${objectType}?population=held_back`, records: [],
})

/** The server caps held-back groups at 20: 21 one-record rules arrive as 20 groups and a section count of 21. */
const truncatedInbox = {
  scanLimit: 500,
  sections: [
    { kind: "held_back", count: 21, shown: 20, approximate: false },
    { kind: "decision", count: 0, shown: 0, approximate: false },
    { kind: "source_error", count: 1, shown: 1, approximate: false },
  ],
  items: [
    ...Array.from({ length: 20 }, (_, index) => group("canon.Item", index + 1)),
    // A failing connection whose record id reads like a mapping id, with no mapping named: it is not ERPNext's.
    { kind: "source_error", id: "erpnext.three.mapping", target: "/connections", values: { status: "failed" } },
  ],
}

const cleanInbox = {
  scanLimit: 500,
  sections: [
    { kind: "held_back", count: 0, shown: 0, approximate: false },
    { kind: "decision", count: 0, shown: 0, approximate: false },
    { kind: "source_error", count: 0, shown: 0, approximate: false },
  ],
  items: [],
}

/**
 * The same definitions with one cross-island link (a holding restated by the core model's
 * analytical restatement, an object type on another island), so the overview lists a bridge. The
 * three-systems fixture declares none across islands, and only object types are map nodes.
 */
const bridgedDefinitions = (definitions as { artifactId: string; payload: { objectType?: { links: unknown[] } } }[]).map((artifact) =>
  artifact.artifactId === "canon.Holding.entity"
    ? { ...artifact, payload: { ...artifact.payload, objectType: { ...artifact.payload.objectType!, links: [...artifact.payload.objectType!.links, { id: "restated_by", target: "core.analytics.Restatement", description: "The restatement that last revised the holding.", cardinality: { from: "many", to: "one", required: false } }] } } }
    : artifact)

async function cell(page: Page, inbox: unknown, definitionsServed: Promise<void> = Promise.resolve(), served: unknown = definitions) {
  await session(page)
  await page.route("**/api/host/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) {
      await definitionsServed
      return route.fulfill({ json: { artifacts: served } })
    }
    if (path.endsWith("/v1/inbox")) return route.fulfill({ json: inbox })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
}

async function shoot(page: Page, info: TestInfo, name: string) {
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(300)
    await page.screenshot({ path: info.outputPath(`map-${name}-${scheme}.png`) })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

const island = (page: Page, title: string) => page.locator('[data-slot="semantic-island"]').filter({ has: page.getByRole("heading", { name: title, exact: true }) })

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: replays a stubbed cell`) })

test("map: a capped inbox reads as lower bounds, and a connection id is never taken for a mapping", async ({ page }, info) => {
  await cell(page, truncatedInbox)
  await page.goto("/map")
  const erp = island(page, "ERPNext")
  // 20 groups came back of 21: the island's count is at least 20, never exactly 20; the section's 21 is exact.
  await expect(erp.getByText("20+ rule holdings")).toBeVisible()
  // The failing connection named no mapping, so ERPNext shows no sync error for it, and no island reads clear.
  await expect(erp.getByText(/sync/)).toHaveCount(0)
  await expect(island(page, "Canon").getByText("None in returned slice")).toBeVisible()
  await expect(page.locator('[data-slot="kpi"]').filter({ hasText: "Inbox alerts" }).locator('[data-slot="kpi-well"] > :first-child')).toHaveText("22")
  await shoot(page, info, "overview")

  await erp.getByRole("button", { name: "Expand island" }).click()
  const item = page.locator('[data-slot="semantic-node"][data-node-id="canon.Item"]')
  await expect(item).toContainText("20+ holdings")
  // A node without a count says its status in words, not only with a dot.
  await expect(page.locator('[data-slot="semantic-node"][data-node-id="canon.Holding"] [data-slot="node-status"]')).toHaveText("Alerts not known")
  await shoot(page, info, "island")
})

test("map: a clean live inbox reads 'No open alerts' on each node in words", async ({ page }) => {
  await cell(page, cleanInbox)
  await page.goto("/map")
  await island(page, "ERPNext").getByRole("button", { name: "Expand island" }).click()
  await expect(page.locator('[data-slot="semantic-node"] [data-slot="node-status"]')).toHaveText(Array.from({ length: 8 }, () => "No open alerts"))
})

test("map: a lens fades only the frame of the cards outside it; their text and ids stay at full contrast", async ({ page }, info) => {
  await cell(page, truncatedInbox)
  await page.goto("/map")
  await page.getByRole("radio", { name: "Attention" }).click()
  const outside = page.locator('[data-slot="semantic-island"][data-outside-lens]')
  await expect(outside).toHaveCount(3)
  await expect(outside.first()).toHaveCSS("opacity", "1")
  await expect(outside.first()).toHaveCSS("border-top-style", "dashed")
  await expect(island(page, "ERPNext")).not.toHaveAttribute("data-outside-lens")

  await island(page, "ERPNext").getByRole("button", { name: "Expand island" }).click()
  const holding = page.locator('[data-slot="semantic-node"][data-node-id="canon.Holding"]')
  await expect(holding).toHaveAttribute("data-outside-lens", "true")
  await expect(holding).toHaveCSS("opacity", "1")
  await expect(holding.getByText("canon.Holding", { exact: true })).toHaveCSS("opacity", "1")
  await expect(holding).toContainText("Outside the current lens.")
  await shoot(page, info, "lens")
})

test("map: drilling in focuses the island's heading; Back returns focus to the island's trigger", async ({ page }) => {
  await cell(page, cleanInbox)
  await page.goto("/map")
  const trigger = island(page, "ERPNext").getByRole("button", { name: "Expand island" })
  await trigger.focus()
  await page.keyboard.press("Enter")
  await expect(page.getByRole("heading", { level: 2, name: "ERPNext" })).toBeFocused()
  await page.getByRole("button", { name: "All islands" }).click()
  await expect(trigger).toBeFocused()
  // The keyboard path round-trips the same way.
  await page.keyboard.press("Enter")
  await expect(page.getByRole("heading", { level: 2, name: "ERPNext" })).toBeFocused()
  await page.keyboard.press("Shift+Tab")
  await expect(page.getByRole("button", { name: "All islands" })).toBeFocused()
  await page.keyboard.press("Enter")
  await expect(trigger).toBeFocused()
})

// #1724 (Fable r1): an island opened from the bridges list returns focus to the bridge end that opened it.
test("map: Back from an island opened in the bridges list returns focus to that bridge's button", async ({ page }) => {
  await cell(page, cleanInbox, Promise.resolve(), bridgedDefinitions)
  await page.goto("/map")
  const end = page.locator('[data-bridge-end="to"]').first()
  await expect(end).toBeVisible()
  const title = (await end.innerText()).trim()
  await end.focus()
  await page.keyboard.press("Enter")
  await expect(page.getByRole("heading", { level: 2, name: title })).toBeFocused()
  await page.getByRole("button", { name: "All islands" }).click()
  await expect(end).toBeFocused()
})

test("map: arrows that point along the reading direction flip in right-to-left text", async ({ page }) => {
  await cell(page, cleanInbox)
  await page.goto("/map")
  const expand = island(page, "ERPNext").getByRole("button", { name: "Expand island" }).locator("svg")
  // Tailwind's `rtl:-scale-x-100` mirrors through the CSS `scale` property.
  await expect(expand).toHaveCSS("scale", "none")
  await page.evaluate(() => { document.documentElement.dir = "rtl" })
  await expect(expand).toHaveCSS("scale", "-1 1")
  await island(page, "ERPNext").getByRole("button", { name: "Expand island" }).click()
  await expect(page.getByRole("button", { name: "All islands" }).locator("svg")).toHaveCSS("scale", "-1 1")
})

test("map on a phone: the skeleton reserves the four stacked key-number tiles, so the first island does not jump on load", async ({ page }, info) => {
  await page.setViewportSize({ width: 390, height: 844 })
  let serve!: () => void
  const served = new Promise<void>((resolve) => { serve = resolve })
  await cell(page, cleanInbox, served)
  await page.goto("/map")
  const overlay = page.locator('[data-slot="bone-skeleton"][data-bone="semantic-map"] [data-boneyard-overlay="true"]')
  await expect(overlay).toBeVisible()
  await page.screenshot({ path: info.outputPath("map-phone-skeleton.png") })
  const skeleton = await overlay.evaluate((el) => {
    const top = el.getBoundingClientRect().top
    const bones = [...el.querySelectorAll<HTMLElement>('[data-boneyard-bone="true"]')].map((bone) => bone.getBoundingClientRect())
    return { height: el.getBoundingClientRect().height, tops: bones.map((bone) => Math.round(bone.top - top)) }
  })
  serve()
  const first = page.locator('[data-slot="semantic-island"]').first()
  await expect(first).toBeVisible()
  // Loaded, the content takes the skeleton's place: measure from the top of its first block, the key-number strip.
  const loaded = await page.locator('[data-slot="stat-strip"]').evaluate((strip) => {
    const top = strip.getBoundingClientRect().top
    const tiles = [...strip.querySelectorAll<HTMLElement>('[data-slot="kpi"]')].map((tile) => Math.round(tile.getBoundingClientRect().top - top))
    const card = document.querySelector<HTMLElement>('[data-slot="semantic-island"]')!.getBoundingClientRect()
    return { tiles, islandTop: Math.round(card.top - top), islandBottom: Math.round(card.bottom - top) }
  })
  await page.screenshot({ path: info.outputPath("map-phone-loaded.png") })
  // Four stacked tiles on a phone, and a bone starting where each tile and the first island card start.
  expect(loaded.tiles).toHaveLength(4)
  for (const y of [...loaded.tiles, loaded.islandTop]) {
    expect(skeleton.tops.some((top) => Math.abs(top - y) <= 24), `a bone near y=${y} (bones at ${skeleton.tops.join(", ")})`).toBe(true)
  }
  expect(Math.abs(skeleton.height - loaded.islandBottom)).toBeLessThanOrEqual(48)
})
