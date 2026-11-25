import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

// MARKINGS VISIBLE: what a marking is, where it applies and who may reveal it, from the server's
// declarations (the registry's privacy pack over the three-systems world) and the server's own Party
// pages for a reader without reveal rights and one with them (fixtures/privacy).
const json = (path: string) => JSON.parse(readFileSync(new URL(`../../../${path}`, import.meta.url), "utf8")) as { artifactId?: string }
const overlay = json("registry/privacy/overlays/canon.Party.entity.json")
const artifacts = [
  ...(json("fixtures/demo/explorer/three-systems.definitions.json") as unknown as { artifactId: string }[]).map((artifact) => artifact.artifactId === overlay.artifactId ? overlay : artifact),
  json("registry/privacy/artifacts/privacy.markings.entity.json"),
  json("registry/privacy/artifacts/privacy.reveal.policy.json"),
]
const recorded = (name: string): string => readFileSync(new URL(`../../../fixtures/privacy/${name}`, import.meta.url), "utf8")

async function privacyCell(page: Page, reader: "viewer" | "auditor") {
  await page.route("**/api/**", (route) => route.fulfill({ status: 404, json: { error: "not found" } }))
  await session(page)
  await page.route("**/api/host/v1/definitions**", (route) => route.fulfill({ json: { artifacts } }))
  await page.route("**/api/host/v1/objects/**", async (route) => {
    const url = new URL(route.request().url())
    const party = JSON.parse(recorded(`canon.Party.${reader}.page.json`)) as { records: { id: string }[] }
    if (url.pathname.endsWith("/v1/objects/canon.Party")) return route.fulfill({ json: party })
    const one = /\/v1\/objects\/canon\.Party\/(.+?)(\/links|\/lineage|\/history|\/findings)?$/.exec(url.pathname)
    if (one !== null && one[2] === undefined) {
      const record = party.records.find(({ id }) => id === decodeURIComponent(one[1] ?? ""))
      return record ? route.fulfill({ json: record }) : route.fulfill({ status: 404, json: { code: "object.not_found", message: "No such readable object." } })
    }
    if (one?.[2] === "/links") return route.fulfill({ json: { objectType: "canon.Party", id: decodeURIComponent(one[1] ?? ""), items: [], next: null } })
    return route.fulfill({ status: 404, json: { code: "object.not_found", message: "No such readable object." } })
  })
}

async function shoot(page: Page, info: { outputPath: (name: string) => string }, name: string) {
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(300)
    await page.screenshot({ path: info.outputPath(`markings-${name}-${scheme}.png`) })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: replays recorded cell documents`) })

for (const reader of ["viewer", "auditor"] as const) {
  test(`markings: the Party grid names each marked column and each pseudonym's marking (${reader})`, async ({ page }, info) => {
    await page.setViewportSize({ width: 1440, height: 900 })
    await privacyCell(page, reader)
    await page.goto("/objects/canon.Party")
    // The column header carries the marking chip in the catalog's words.
    await expect(page.getByRole("columnheader").locator('[data-marking="pii.contact"]').first()).toHaveText("Contact")
    await expect(page.locator("[data-mask-hint]").first()).toHaveText("pseudonymised: pii.contact")
    // Reveal only where the server says this reader may reveal.
    await expect(page.locator("[data-reveal]")).toHaveCount(reader === "auditor" ? await page.locator('[data-masked="pseudonym"]').count() : 0)
    await shoot(page, info, `grid-${reader}`)
  })
}

test("markings: the Markings page lists each marking, its projection, its facts and who may reveal it", async ({ page }, info) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await privacyCell(page, "viewer")
  await page.goto("/markings")
  const contact = page.locator('[data-marking-row="pii.contact"]')
  await expect(contact).toContainText("Shown as a stable pseudonym")
  await expect(contact).toContainText("display name")
  await expect(contact).toContainText("admin")
  await expect(contact).toContainText("owner")
  await expect(page.locator('[data-marking-row="pii.identity"]')).toContainText("Never shown")
  await expect(page.getByRole("navigation", { name: "Studio navigation" }).getByRole("tree", { name: "Builder pages" }).getByRole("link", { name: "Markings" })).toBeVisible()
  await shoot(page, info, "page")
})

test("markings: a type's facts carry their marking chips", async ({ page }, info) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await privacyCell(page, "viewer")
  await page.goto("/sources/canon.Party")
  await expect(page.locator('[data-marking="pii.contact"]').first()).toBeVisible()
  await shoot(page, info, "definition")
})
