import { readdirSync, readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

// RULES: each rule in plain words, the records it holds back right now with the fix the server
// offers, and the platform's own rules folded under System. The world is the three-systems
// recording plus the registry's documents pack; the work queue is the server's inbox shape.
const root = new URL("../../../", import.meta.url)
const read = (path: string) => JSON.parse(readFileSync(new URL(path, root), "utf8")) as unknown
const pack = readdirSync(new URL("registry/packs/documents/artifacts/", root)).filter((name) => name.endsWith(".json")).map((name) => read(`registry/packs/documents/artifacts/${name}`))
const artifacts = [...read("fixtures/demo/explorer/three-systems.definitions.json") as unknown[], ...pack]

const held = {
  kind: "held_back", ruleRef: { kind: "law", artifactId: "canon.ItemizedDocument.total_matches_lines.law", lawId: "canon.ItemizedDocument.total_matches_lines", version: "sha256:1" },
  title: "An itemized document's total equals the sum of its lines", rationale: null, objectType: "docs.ScannedDocument", count: 1, approximate: false,
  target: "/objects/docs.ScannedDocument?population=held_back",
  records: [{
    objectType: "docs.ScannedDocument", id: "SCAN-INV-0042", target: "/sources/docs.ScannedDocument/SCAN-INV-0042",
    finding: { id: "f1", words: "total 720.00 ≠ lines 72.00", message: "m", compared: {} }, values: { doc_number: "INV-0042" },
    suggestedFix: { actionId: "docs.scanned_document.correct", title: "Correct an extracted total", label: "Use 72.00", property: "total", value: "72.00", arguments: {}, needs: ["correction_reason"] },
  }],
}
const inbox = { scanLimit: 10000, items: [held], sections: [{ kind: "held_back", count: 1, shown: 1, approximate: false }, { kind: "decision", count: 0, shown: 0, approximate: false }, { kind: "source_error", count: 0, shown: 0, approximate: false }] }

async function cell(page: Page) {
  await session(page)
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts } })
    if (path.endsWith("/v1/inbox")) return route.fulfill({ json: inbox })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
}

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: replays recorded cell documents`) })

test("rules: each rule in plain words, the records it holds back and the server's fix; platform rules fold under System", async ({ page }, info) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await cell(page)
  // The old address still opens it.
  await page.goto("/guarantees")
  await expect(page).toHaveURL((url) => url.pathname === "/rules")
  await expect(page.getByRole("heading", { name: "Rules", level: 1 })).toBeVisible()
  const rule = page.locator('[data-law="canon.ItemizedDocument.total_matches_lines"]').first()
  await expect(rule).toContainText("An itemized document's total equals the sum of its lines")
  await expect(rule.locator("[data-held]")).toHaveText("1 held back")
  await expect(rule).toContainText("total 720.00 ≠ lines 72.00")
  await expect(rule.getByRole("link", { name: "Use 72.00" })).toHaveAttribute("href", "/sources/docs.ScannedDocument/SCAN-INV-0042")
  // The platform's restatement rule is folded, and its engine words with it.
  await expect(page.getByText(/RESTATE-HISTORY-1/)).toBeHidden()
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(300)
    await page.screenshot({ path: info.outputPath(`rules-page-${scheme}.png`) })
  }
  await page.getByText(/^System rules/).click()
  await expect(page.getByText("Approve complete restatement generations")).toBeVisible()
})

test("rules: an object type's page has a Rules tab with the rules its records must keep", async ({ page }, info) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await cell(page)
  await page.goto("/objects/docs.ScannedDocument")
  await page.getByRole("tab", { name: "Rules" }).click()
  await expect(page.locator('[data-law="canon.ItemizedDocument.total_matches_lines"]').first()).toContainText("1 held back")
  await page.screenshot({ path: info.outputPath("rules-type-tab-light.png") })
})
