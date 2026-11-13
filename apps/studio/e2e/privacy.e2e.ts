import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

// PRIVACY-1/2: the grid shows what the server sent for a marked property: a pseudonym chip for a
// viewer, a blurred chip with "Reveal" for a reader whose policy allows it, and the value with Copy
// only after the cell answers the reveal with its receipt. The documents are the server's own
// (crates/norn-api/tests/privacy.rs writes them); the browser never holds a value it was not sent.
// SHOTS=<dir> saves each state in light and dark.

const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400)
    await page.screenshot({ path: `${shots}/privacy-${name}-${scheme}.png` })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

const recorded = (name: string): string =>
  readFileSync(new URL(`../../../fixtures/demo/privacy/${name}`, import.meta.url), "utf8")

/** Serve the server's Party page for `reader`, and its reveal reply; record what the browser asked. */
async function partyCell(page: Page, reader: "viewer" | "auditor") {
  const reveals: unknown[] = []
  await page.route("**/api/host/v1/objects/**", async (route) => {
    const url = new URL(route.request().url())
    if (route.request().method() === "POST" && url.pathname.endsWith("/reveal")) {
      reveals.push(JSON.parse(route.request().postData() ?? "null"))
      if (reader !== "auditor") return route.fulfill({ status: 403, json: { code: "authz.denied", message: "The reveal is not authorized." } })
      return route.fulfill({ status: 200, contentType: "application/json", body: recorded("canon.Party.reveal.json") })
    }
    // HOST-READ-7 over the same page: counts of the categorical property, as the cell would give them.
    if (url.pathname.endsWith("/v1/objects/canon.Party:facets")) {
      const page = JSON.parse(recorded(`canon.Party.${reader}.page.json`)) as { records: { values: Record<string, string> }[] }
      const counts = new Map<string, number>()
      for (const record of page.records) counts.set(record.values["kind"] ?? "", (counts.get(record.values["kind"] ?? "") ?? 0) + 1)
      return route.fulfill({ json: { objectType: "canon.Party", rows: page.records.length, withheldRows: 0, quarantinedRows: 0, lowerBound: false, scanLimit: 10000, facets: [{ property: "kind", values: [...counts].map(([value, count]) => ({ value, count })), otherRows: 0, nulls: 0 }] } })
    }
    const one = /\/v1\/objects\/canon\.Party\/(.+?)(\/links|\/lineage)?$/.exec(url.pathname)
    if (one !== null && route.request().method() === "GET") {
      const id = decodeURIComponent(one[1] ?? "")
      if (one[2] === "/links") return route.fulfill({ json: { objectType: "canon.Party", id, items: [], next: null } })
      if (one[2] === "/lineage") return route.fulfill({ status: 404, json: { code: "object.not_found", message: "No such readable object." } })
      const page = JSON.parse(recorded(`canon.Party.${reader}.page.json`)) as { records: { id: string }[] }
      const record = page.records.find((candidate) => candidate.id === id)
      return record === undefined ? route.fulfill({ status: 404, json: { code: "object.not_found", message: "No such readable object." } }) : route.fulfill({ json: record })
    }
    if (url.pathname.endsWith("/v1/objects/canon.Party")) {
      return route.fulfill({ status: 200, contentType: "application/json", body: recorded(`canon.Party.${reader}.page.json`) })
    }
    return route.fulfill({ status: 404, json: { code: "object.not_found", message: "No such readable object." } })
  })
  return { reveals }
}

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: replays recorded cell documents`) })

test("a viewer without access sees stable pseudonyms, never the names", async ({ page }) => {
  await session(page)
  await partyCell(page, "viewer")
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.goto("/objects/canon.Party")
  const chips = page.locator('[data-masked="pseudonym"]')
  await expect(chips.first()).toBeVisible()
  await expect(chips.first()).toContainText("Customer #")
  // The same customer twice keeps one pseudonym, so counts and joins hold.
  const texts = await chips.allInnerTexts()
  expect(new Set(texts).size).toBeLessThan(texts.length)
  await expect(page.getByText("Amira Haddad")).toHaveCount(0)
  await expect(page.locator("[data-reveal]")).toHaveCount(0)
  await shoot(page, "viewer")
})

test("a reader allowed to reveal sees a blurred chip; Reveal records a receipt, then shows the value with Copy", async ({ page }) => {
  await session(page)
  const cell = await partyCell(page, "auditor")
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.goto("/objects/canon.Party")
  await expect(page.locator("[data-reveal]").first()).toBeVisible()
  await expect(page.getByText("Amira Haddad")).toHaveCount(0)
  await shoot(page, "revealable")
  await page.locator("[data-reveal]").first().click()
  await page.getByLabel("Purpose").fill("KYC review for refund #4471")
  await shoot(page, "reveal-purpose")
  await page.locator("[data-confirm-reveal]").click()
  await expect(page.locator("[data-revealed]").first()).toContainText("Amira Haddad")
  await expect(page.locator("[data-revealed]").first()).toContainText("receipt #")
  expect(cell.reveals).toHaveLength(1)
  expect(cell.reveals[0]).toMatchObject({ property: "display_name", purpose: "KYC review for refund #4471" })
  await shoot(page, "revealed")
})
