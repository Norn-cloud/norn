import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import listedMetrics from "../test/fixtures/metrics-builder/metrics.json" with { type: "json" }
import scannedTotalRefusal from "../test/fixtures/metrics-builder/scanned-total-refusal.json" with { type: "json" }
import { session } from "./object-cell.ts"

/** A live cell: its definitions, and the work queue it serves this caller (`GET /v1/inbox`, as the server projects it). */
async function cell(page: Page, inbox: unknown) {
  await session(page)
  const definitions = JSON.parse(readFileSync(new URL("../../../fixtures/demo/explorer/three-systems.definitions.json", import.meta.url), "utf8")) as unknown
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: definitions } })
    if (path.endsWith("/v1/inbox")) return route.fulfill({ json: inbox })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
}

const group = (records: unknown[]) => ({
  kind: "held_back", ruleRef: { kind: "law", artifactId: "canon.ItemizedDocument.total_matches_lines.law", lawId: "canon.ItemizedDocument.total_matches_lines", version: "sha256:1" },
  title: "An itemized document's total equals the sum of its lines", rationale: null, objectType: "docs.ScannedDocument", count: 1, approximate: false,
  target: "/objects/docs.ScannedDocument?population=held_back", records,
})
const sections = [{ kind: "held_back", count: 1, shown: 1, approximate: false }, { kind: "decision", count: 0, shown: 0, approximate: false }, { kind: "source_error", count: 0, shown: 0, approximate: false }]

test("home: the inbox groups held-back records by law, each with its finding in words and a link to review a suggested fix", async ({ page }) => {
  await cell(page, { scanLimit: 10000, sections, items: [group([{
    objectType: "docs.ScannedDocument", id: "SCAN-INV-1042", target: "/sources/docs.ScannedDocument/SCAN-INV-1042",
    finding: { id: "f1", words: "total 720.00 ≠ lines 72.00", message: "m", compared: {} }, values: { doc_number: "INV-1042" },
    suggestedFix: { actionId: "docs.scanned_document.correct", title: "Correct an extracted total", label: "Use 72.00", property: "total", value: "72.00", arguments: {}, needs: ["correction_reason"] },
  }])] })
  await page.goto("/")
  await expect(page.getByRole("heading", { name: "Needs attention" })).toBeVisible()
  await expect(page.getByText("An itemized document's total equals the sum of its lines")).toBeVisible()
  await expect(page.getByText("total 720.00 ≠ lines 72.00")).toBeVisible()
  await expect(page.getByRole("link", { name: "Review", exact: true })).toHaveAttribute("href", "/objects/docs.ScannedDocument?population=held_back")
  await page.getByRole("link", { name: "Review fix", exact: true }).click()
  await expect(page).toHaveURL(/\/sources\/docs\.ScannedDocument\/SCAN-INV-1042$/)
  expect(await page.evaluate(() => window.history.state?.suggestedFix)).toBeUndefined()
})

test("home: a member's inbox names the held record with no finding words and no fix (the server sends none)", async ({ page }) => {
  await cell(page, { scanLimit: 10000, sections, items: [group([{ objectType: "docs.ScannedDocument", id: "SCAN-INV-1042", target: "/sources/docs.ScannedDocument/SCAN-INV-1042" }])] })
  await page.goto("/")
  await expect(page.getByRole("link", { name: "SCAN-INV-1042" })).toBeVisible()
  await expect(page.getByText("total 720.00")).toHaveCount(0)
  await expect(page.getByRole("link", { name: "Review fix", exact: true })).toHaveCount(0)
})

test("home: a record two laws disagree on offers no fix and names both laws", async ({ page }) => {
  await cell(page, { scanLimit: 10000, sections, items: [group([{
    objectType: "docs.ScannedDocument", id: "SCAN-INV-1043", target: "/sources/docs.ScannedDocument/SCAN-INV-1043", values: { doc_number: "INV-1043" },
    finding: { id: "f2", words: "total 720.00 ≠ ledger entry 72.00", message: "m", compared: {} },
    otherFindings: [{ id: "lines", words: "total 72.00 ≠ lines 720.00", message: "m", compared: {} }],
  }])] })
  await page.goto("/")
  await expect(page.getByText("Needs review: 2 laws disagree")).toBeVisible()
  await expect(page.getByText("total 72.00 ≠ lines 720.00")).toBeVisible()
  await expect(page.getByRole("link", { name: "Review fix", exact: true })).toHaveCount(0)
})

test("home: nothing to do says so", async ({ page }) => {
  await cell(page, { scanLimit: 10000, sections: sections.map((entry) => ({ ...entry, count: 0, shown: 0 })), items: [] })
  await page.goto("/")
  await expect(page.getByText("Nothing needs you")).toBeVisible()
})

test("home: a key number the server refuses leaves Home, and the server's reason says why", async ({ page }, info) => {
  await cell(page, { scanLimit: 10000, sections: sections.map((entry) => ({ ...entry, count: 0, shown: 0 })), items: [] })
  const keyMetrics = listedMetrics.metrics.filter(({ objectType }) => objectType === "docs.LedgerEntry" || objectType === "docs.ScannedDocument")
  const scannedMetric = keyMetrics.find(({ objectType }) => objectType === "docs.ScannedDocument")?.name
  const answered = JSON.parse(readFileSync(new URL("../test/fixtures/metrics-builder/unit-groups.json", import.meta.url), "utf8")) as unknown
  await page.route("**/api/host/v1/questions:metrics**", (route) => route.fulfill({ json: { metrics: keyMetrics } }))
  await page.route("**/api/host/v1/questions:evaluate**", (route) => {
    const { question } = route.request().postDataJSON() as { question: { aggregate: { metric: string }[] } }
    // Captured from this route with the scans pack installed and no records loaded.
    return question.aggregate[0]?.metric === scannedMetric
      ? route.fulfill({ status: 422, json: scannedTotalRefusal })
      : route.fulfill({ json: answered })
  })
  await page.goto("/")
  const keyNumbers = page.getByRole("region", { name: "Key numbers" })
  const strip = keyNumbers.locator('[data-slot="stat-strip"]')
  const refused = keyNumbers.getByRole("list", { name: "Key numbers not shown" })
  await expect(refused.getByRole("listitem")).toHaveText("Scanned document · Total (not shown). The time axis must be a single-valued date or timestamp.")
  await expect(strip.getByRole("heading", { name: "Total amount" })).toBeVisible()
  await expect(strip.getByText("70,356", { exact: true })).toBeVisible()
  await expect(strip.locator('[data-slot="kpi"]')).toHaveCount(1)
  await page.setViewportSize({ width: 1440, height: 900 })
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await expect(page.locator("html")).toHaveClass(scheme === "dark" ? /\bdark\b/ : /^(?!.*\bdark\b).*$/)
    await keyNumbers.scrollIntoViewIfNeeded()
    await page.screenshot({ path: info.outputPath(`home-refused-number-${scheme}.png`), animations: "disabled" })
  }
})

// UI polish: two groups held by one law tell themselves apart on the title line (type and count); each
// record's finding starts at one x; at 390px the actions sit under the title and a comparison never splits.
test("home: one law's groups name their type and count on the title line, findings share one column, and phones keep titles and comparisons whole", async ({ page }) => {
  const ledger = { ...group([{ objectType: "docs.LedgerEntry", id: "LE-1", target: "/sources/docs.LedgerEntry/LE-1", finding: { id: "l1", words: "total 720.00 ≠ lines 72.00", message: "m", compared: {} } }]), objectType: "docs.LedgerEntry", count: 5, target: "/objects/docs.LedgerEntry?population=held_back" }
  const scanned = { ...group([
    { objectType: "docs.ScannedDocument", id: "SCAN-INV-0042", target: "/sources/docs.ScannedDocument/SCAN-INV-0042", finding: { id: "s1", words: "total 720.00 ≠ lines 72.00", message: "m", compared: {} } },
    { objectType: "docs.ScannedDocument", id: "SCAN-INV-0099", target: "/sources/docs.ScannedDocument/SCAN-INV-0099", values: { doc_number: "October office supply invoice" }, finding: { id: "s2", words: "total 1400.00 ≠ ledger entry 140.00", message: "m", compared: {} } },
  ]), count: 500, approximate: true }
  await cell(page, { scanLimit: 10000, sections: [{ kind: "held_back", count: 505, shown: 3, approximate: true }, sections[1], sections[2]], items: [ledger, scanned] })
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.goto("/")
  const heads = page.getByRole("heading", { level: 3 })
  await expect(heads.nth(0)).toContainText("An itemized document's total equals the sum of its lines")
  await expect(heads.nth(0)).toContainText("5 held back")
  await expect(heads.nth(1)).toContainText("500+ held back")
  expect(await heads.nth(0).innerText()).not.toBe(await heads.nth(1).innerText())
  // Every finding starts at one x.
  const xs = await page.locator("[data-held-record] [data-finding]").evaluateAll((nodes) => nodes.map((node) => Math.round(node.getBoundingClientRect().x)))
  expect(xs.length).toBe(3)
  expect(new Set(xs).size).toBe(1)
  // The column is centred in the canvas.
  const column = await page.locator("[data-home]").boundingBox()
  const canvas = await page.locator("[data-route]").boundingBox()
  expect(Math.abs((column!.x - canvas!.x) - (canvas!.x + canvas!.width - column!.x - column!.width))).toBeLessThan(50)
  await page.setViewportSize({ width: 390, height: 844 })
  // At 390 the Review action sits under its title, and "ledger entry 140.00" stays on one line.
  const title = heads.nth(1)
  const review = page.getByRole("link", { name: "Review", exact: true }).nth(1)
  expect((await review.boundingBox())!.y).toBeGreaterThan((await title.boundingBox())!.y + (await title.boundingBox())!.height - 2)
  const side = page.locator('[data-held-record="SCAN-INV-0099"] [data-finding] span').nth(1)
  await expect(side).toHaveText("≠ ledger entry 140.00")
  // One line high: the inline block moved whole rather than breaking inside.
  expect(await side.evaluate((element) => element.getBoundingClientRect().height)).toBeLessThan(26)
})
