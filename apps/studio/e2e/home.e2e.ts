import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { metrics } from "../test/fixtures/metrics-builder/metrics.json"
import scannedTotalRefusal from "../test/fixtures/metrics-builder/scanned-total-refusal.json"
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

test("home: the inbox groups held-back records by law, each with its finding in words and the fix the server offers", async ({ page }) => {
  await cell(page, { scanLimit: 10000, sections, items: [group([{
    objectType: "docs.ScannedDocument", id: "SCAN-INV-1042", target: "/sources/docs.ScannedDocument/SCAN-INV-1042",
    finding: { id: "f1", words: "total 720.00 ≠ lines 72.00", message: "m", compared: {} }, values: { doc_number: "INV-1042" },
    suggestedFix: { actionId: "docs.scanned_document.correct", title: "Correct an extracted total", label: "Use 72.00", property: "total", value: "72.00", arguments: {}, needs: ["correction_reason"] },
  }])] })
  await page.goto("/")
  await expect(page.getByRole("heading", { name: "Needs attention" })).toBeVisible()
  await expect(page.getByText("An itemized document's total equals the sum of its lines")).toBeVisible()
  await expect(page.getByText("total 720.00 ≠ lines 72.00")).toBeVisible()
  await expect(page.getByRole("link", { name: "Review" })).toHaveAttribute("href", "/objects/docs.ScannedDocument?population=held_back")
  await page.getByRole("link", { name: "Use 72.00" }).click()
  await expect(page).toHaveURL(/\/sources\/docs\.ScannedDocument\/SCAN-INV-1042$/)
})

test("home: a member's inbox names the held record with no finding words and no fix (the server sends none)", async ({ page }) => {
  await cell(page, { scanLimit: 10000, sections, items: [group([{ objectType: "docs.ScannedDocument", id: "SCAN-INV-1042", target: "/sources/docs.ScannedDocument/SCAN-INV-1042" }])] })
  await page.goto("/")
  await expect(page.getByRole("link", { name: "SCAN-INV-1042" })).toBeVisible()
  await expect(page.getByText("total 720.00")).toHaveCount(0)
  await expect(page.getByRole("link", { name: /Use / })).toHaveCount(0)
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
  await expect(page.getByRole("link", { name: /Use / })).toHaveCount(0)
})

test("home: nothing to do says so", async ({ page }) => {
  await cell(page, { scanLimit: 10000, sections: sections.map((entry) => ({ ...entry, count: 0, shown: 0 })), items: [] })
  await page.goto("/")
  await expect(page.getByText("Nothing needs you")).toBeVisible()
})

test("home: a key number the server refuses leaves Home, and the server's reason says why", async ({ page }, info) => {
  await cell(page, { scanLimit: 10000, sections: sections.map((entry) => ({ ...entry, count: 0, shown: 0 })), items: [] })
  const keyMetrics = metrics.filter(({ objectType }) => objectType === "docs.LedgerEntry" || objectType === "docs.ScannedDocument")
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
