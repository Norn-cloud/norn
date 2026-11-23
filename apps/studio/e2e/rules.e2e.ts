import { readdirSync, readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

// RULES: each rule in plain words, grouped by type and rule, with the records it holds back now
// (lower-bound counts, withholding and disagreement semantics, main's record title resolution, and
// the server's fix-offer link), platform rules folded under System, the /guarantees → /rules
// redirect, held-work reachability from Workspace/Inbox, and the record & type Rules tabs.
const root = new URL("../../../", import.meta.url)
const read = (path: string) => JSON.parse(readFileSync(new URL(path, root), "utf8")) as { artifactId?: string }
const base = read("fixtures/demo/explorer/three-systems.definitions.json") as unknown as { artifactId: string }[]
const scans = readdirSync(new URL("fixtures/packs/scans/artifacts/", root))
  .filter((name) => name.endsWith(".json"))
  .map((name) => read(`fixtures/packs/scans/artifacts/${name}`) as { artifactId: string })
const seen = new Set(base.map(({ artifactId }) => artifactId))
const artifacts = [...base, ...scans.filter(({ artifactId }) => !seen.has(artifactId))]

const heldLines = {
  kind: "held_back",
  ruleRef: { kind: "law", artifactId: "canon.ItemizedDocument.total_matches_lines.law", lawId: "canon.ItemizedDocument.total_matches_lines", version: "sha256:1" },
  title: "An itemized document's total equals the sum of its lines",
  rationale: null,
  objectType: "docs.ScannedDocument",
  count: 500,
  approximate: true,
  target: "/objects/docs.ScannedDocument?population=held_back",
  records: [
    {
      objectType: "docs.ScannedDocument",
      id: "SCAN-INV-0042",
      target: "/sources/docs.ScannedDocument/SCAN-INV-0042",
      finding: { id: "f1", words: "total 720.00 ≠ lines 72.00", message: "m", compared: {} },
      values: { doc_number: "INV-0042" },
      suggestedFix: { actionId: "docs.scanned_document.correct", title: "Correct an extracted total", label: "Use 72.00", property: "total", value: "72.00", arguments: {}, needs: ["correction_reason"] },
    },
    {
      objectType: "docs.ScannedDocument",
      id: "SCAN-INV-0043",
      target: "/sources/docs.ScannedDocument/SCAN-INV-0043",
      finding: { id: "f2", words: "total 720.00 ≠ lines 72.00", message: "m", compared: {} },
      values: { label: "October office supply invoice" },
      otherFindings: [{ id: "f3", words: "total 720.00 ≠ ledger entry 72.00", message: "m", compared: {} }],
    },
    {
      // Withheld values & finding (a reader without Sources on this row): only the record id and link appear.
      objectType: "docs.ScannedDocument",
      id: "SCAN-INV-0044",
      target: "/sources/docs.ScannedDocument/SCAN-INV-0044",
    },
  ],
}

// The same inherited law over another type, listed FIRST in items: each type reads its own group, never another type's count.
const elsewhere = {
  ...heldLines,
  objectType: "docs.LedgerEntry",
  count: 5,
  approximate: false,
  target: "/objects/docs.LedgerEntry?population=held_back",
  records: [],
}

const heldLedger = {
  kind: "held_back",
  ruleRef: { kind: "law", artifactId: "docs.ScannedDocument.total_matches_ledger.law", lawId: "docs.ScannedDocument.total_matches_ledger", version: "sha256:1" },
  title: "A booked document's total equals its journal entry",
  rationale: null,
  objectType: "docs.ScannedDocument",
  count: 1,
  approximate: false,
  target: "/objects/docs.ScannedDocument?population=held_back",
  records: [
    {
      objectType: "docs.ScannedDocument",
      id: "SCAN-INV-0099",
      target: "/sources/docs.ScannedDocument/SCAN-INV-0099",
      finding: { id: "f4", words: "total 1400.00 ≠ ledger entry 140.00", message: "m", compared: {} },
      values: { doc_number: "INV-0099" },
      suggestedFix: { actionId: "docs.scanned_document.correct", title: "Correct an extracted total", label: "Use 140.00", property: "total", value: "140.00", arguments: {}, needs: ["correction_reason"] },
    },
  ],
}

const inbox = {
  scanLimit: 10000,
  items: [elsewhere, heldLines, heldLedger],
  sections: [
    { kind: "held_back", count: 506, shown: 3, approximate: true },
    { kind: "decision", count: 0, shown: 0, approximate: false },
    { kind: "source_error", count: 0, shown: 0, approximate: false },
  ],
}

const scannedRecord = {
  objectType: "docs.ScannedDocument",
  id: "SCAN-INV-0042",
  scope: "org_demo/dev",
  version: 1,
  values: { id: "SCAN-INV-0042", doc_number: "INV-0042", kind: "invoice", vendor: "Acme Paper", currency: "USD", total: "720.00", line_count: 3 },
  withheld: false,
  withheldProperties: [],
}

const shots = process.env.SHOTS

async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(350)
    await page.screenshot({ path: `${shots}/${name}-${scheme}.png`, fullPage: true })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

async function cell(page: Page) {
  await session(page)
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts } })
    if (path.endsWith("/v1/definitions:list")) return route.fulfill({ json: { items: [], cursor: null } })
    if (path.endsWith("/v1/inbox")) return route.fulfill({ json: inbox })
    if (path.endsWith("/v1/decisions")) return route.fulfill({ json: { items: [], total: 0, approximate: false, serverNowMs: Date.UTC(2026, 10, 23, 12, 0, 0) } })
    if (path.endsWith("/v1/decisions/requested")) return route.fulfill({ json: { items: [], serverNowMs: Date.UTC(2026, 10, 23, 12, 0, 0) } })
    if (path.endsWith("/v1/objects/docs.ScannedDocument:quarantined")) {
      return route.fulfill({ json: { objectType: "docs.ScannedDocument", items: [{ id: "SCAN-INV-0042", openFindings: 1 }], approximate: true, scanLimit: 10000 } })
    }
    if (path.endsWith("/v1/objects/docs.ScannedDocument/SCAN-INV-0042")) return route.fulfill({ json: scannedRecord })
    if (path.endsWith("/v1/objects/docs.ScannedDocument/SCAN-INV-0042/links")) return route.fulfill({ json: { objectType: "docs.ScannedDocument", id: "SCAN-INV-0042", items: [], next: null } })
    if (path.endsWith("/v1/objects/docs.ScannedDocument/SCAN-INV-0042/findings")) {
      return route.fulfill({
        json: {
          objectType: "docs.ScannedDocument",
          id: "SCAN-INV-0042",
          quarantined: true,
          evidence: [],
          findings: [{
            id: "f1",
            ruleRef: { kind: "law", artifactId: "canon.ItemizedDocument.total_matches_lines.law", lawId: "canon.ItemizedDocument.total_matches_lines", version: "sha256:1" },
            severity: "error",
            message: "total 720.00 ≠ lines 72.00",
            status: "open",
            firstSeen: "2026-11-23T10:00:00Z",
            lastSeen: "2026-11-23T10:00:00Z",
            values: { total: 720, "lines.amount": [30, 40, 2] },
          }],
        },
      })
    }
    if (path.endsWith("/v1/objects/docs.ScannedDocument/SCAN-INV-0042/history")) {
      return route.fulfill({ json: { objectType: "docs.ScannedDocument", id: "SCAN-INV-0042", truncated: false, versions: [{ version: 1, commit: 412, logicalTime: Date.UTC(2026, 10, 23, 10, 0, 0), values: scannedRecord.values, writer: { kind: "other" } }] } })
    }
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
}

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: replays recorded cell documents`) })
test.beforeEach(async ({ page }) => { await page.setViewportSize({ width: 1440, height: 900 }) })

test("rules: /guarantees redirects to /rules, Builder nav & command palette use Rules, system rules fold, and technical ids stay under Technical", async ({ page }) => {
  await cell(page)
  await page.goto("/guarantees")
  await expect(page).toHaveURL((url) => url.pathname === "/rules")
  await expect(page.getByRole("heading", { name: "Rules", level: 1 })).toBeVisible()

  // Builder sidebar names Rules, never Guarantees.
  const builderNav = page.getByRole("navigation", { name: "Builder" })
  await expect(builderNav.getByRole("link", { name: "Rules" })).toBeVisible()
  await expect(builderNav.getByRole("link", { name: "Guarantees" })).toHaveCount(0)

  // Command palette lists Rules and not Guarantees.
  await page.getByRole("button", { name: "Search" }).click()
  const dialog = page.getByRole("dialog")
  const input = dialog.getByRole("combobox", { name: "Search pages, object types, metrics" })
  await input.fill("Guarantees")
  await expect(dialog.getByText("Nothing matches.")).toBeVisible()
  await input.fill("Rules")
  const rulesOption = dialog.getByRole("option", { name: /Rules/ }).first()
  await expect(rulesOption).toBeVisible()
  await rulesOption.click()
  await expect(dialog).toBeHidden()
  await expect(page).toHaveURL((url) => url.pathname === "/rules")

  // Business rules lead with authored names; engine ids stay hidden in Business view.
  const rule = page.locator('[data-law="canon.ItemizedDocument.total_matches_lines"]').first()
  await expect(rule).toContainText("An itemized document's total equals the sum of its lines")
  await expect(rule).toContainText("from Itemized document")
  await expect(rule).not.toContainText("canon.ItemizedDocument.total_matches_lines")
  await expect(page.locator('[data-law="docs.ScannedDocument.total_matches_ledger"]').first()).toContainText("A booked document's total equals its journal entry")

  // Platform rules fold under System rules, hiding engine rationales until opened.
  await expect(page.getByText(/RESTATE-HISTORY-1/)).toBeHidden()
  await shoot(page, "rules-all")
  await page.getByText(/^System rules/).click()
  await expect(page.getByText("Approve complete restatement generations")).toBeVisible()

  // Technical view reveals engine ids, owner chips and type ids.
  await page.getByRole("button", { name: "Account" }).click()
  await page.getByRole("menuitemradio", { name: "Technical" }).click()
  await expect(rule).toContainText("canon.ItemizedDocument.total_matches_lines")
  await expect(page.getByText("docs.ScannedDocument", { exact: true }).first()).toBeVisible()
})

test("rules: held-record counts preserve lower bounds, match by type and rule, respect withholding/disagreement, and link into review and fix flows", async ({ page }) => {
  await cell(page)
  await page.goto("/rules")

  // StatStrip shows the lower-bound held count across rules.
  const heldKpi = page.locator('[data-slot="kpi"]', { hasText: "Held back now" })
  await expect(heldKpi).toContainText("506+")
  await expect(heldKpi).toContainText("scan bound")

  // Scanned document's inherited rule shows 500+ (not LedgerEntry's 5 listed first in inbox).
  const rule = page.locator('[data-law="canon.ItemizedDocument.total_matches_lines"]').first()
  const chip = rule.locator("[data-held]")
  await expect(chip).toHaveText("500+ held back")
  await expect(chip).toHaveAttribute("data-held", "500")
  await expect(chip).toHaveAttribute("data-approximate", "true")
  await expect(chip).toHaveAttribute("title", /At least 500 held back.*10,000 rows/)

  // Record 1: main's title resolution uses id (never guessing doc_number INV-0042), shows finding and server's fix offer.
  await expect(rule.getByRole("link", { name: "SCAN-INV-0042", exact: true })).toHaveAttribute("href", "/sources/docs.ScannedDocument/SCAN-INV-0042")
  await expect(rule.getByRole("link", { name: "INV-0042", exact: true })).toHaveCount(0)
  await expect(rule).toContainText("total 720.00 ≠ lines 72.00")
  const fix = rule.getByRole("link", { name: "Use 72.00" })
  await expect(fix).toHaveAttribute("href", "/sources/docs.ScannedDocument/SCAN-INV-0042")
  await expect(fix).toHaveAttribute("title", "Correct an extracted total")

  // Record 2: label resolves via recordName; disagreement shows badge and offers no fix.
  await expect(rule.getByRole("link", { name: "October office supply invoice" })).toBeVisible()
  await expect(rule).toContainText("Needs review: 2 laws disagree")
  await expect(rule).toContainText("total 720.00 ≠ ledger entry 72.00")

  // Record 3: withheld values/finding shows id only with no fix.
  await expect(rule.getByRole("link", { name: "SCAN-INV-0044" })).toBeVisible()

  // Review all link carries the lower bound and opens the type's Quarantine tab.
  const reviewAll = rule.getByRole("link", { name: "Review all 500+" })
  await expect(reviewAll).toHaveAttribute("href", "/objects/docs.ScannedDocument?population=held_back")

  // Following the fix offer enters the record page review & fix flow.
  await fix.click()
  await expect(page).toHaveURL(/\/sources\/docs\.ScannedDocument\/SCAN-INV-0042$/)
  await expect(page.getByRole("heading", { name: "SCAN-INV-0042", level: 1 })).toBeVisible()

  // Going back and clicking Review all lands directly on the type's Quarantine tab.
  await page.goBack()
  await expect(page).toHaveURL((url) => url.pathname === "/rules")
  await rule.getByRole("link", { name: "Review all 500+" }).click()
  await expect(page).toHaveURL(/\/objects\/docs\.ScannedDocument\?population=held_back$/)
  await expect(page.getByRole("tab", { name: "Quarantine" })).toHaveAttribute("aria-selected", "true")
})

test("rules: held-work view is reachable from Workspace Home and from Inbox", async ({ page }) => {
  await cell(page)
  // From Workspace Home ("Needs attention").
  await page.goto("/")
  await expect(page.getByRole("heading", { name: "Needs attention" })).toBeVisible()
  await page.getByRole("link", { name: "Held by rules" }).click()
  await expect(page).toHaveURL((url) => url.pathname === "/rules" && url.searchParams.get("view") === "held")
  await expect(page.getByRole("tab", { name: /Held back/ })).toHaveAttribute("aria-selected", "true")
  await expect(page.locator('[data-law="canon.ItemizedDocument.total_matches_lines"]').first()).toContainText("500+ held back")
  await expect(page.getByText(/^System rules/)).toHaveCount(0)
  await shoot(page, "rules-held")

  // From Inbox (/decisions).
  await page.goto("/decisions")
  const inboxLink = page.getByRole("link", { name: /Held by rules/ })
  await expect(inboxLink).toContainText("506+")
  await inboxLink.click()
  await expect(page).toHaveURL((url) => url.pathname === "/rules" && url.searchParams.get("view") === "held")
  await expect(page.getByRole("tab", { name: /Held back/ })).toHaveAttribute("aria-selected", "true")
})

test("rules: the record page and object type page both expose a Rules tab leading with authored names", async ({ page }) => {
  await cell(page)
  // Record page Rules tab.
  await page.goto("/sources/docs.ScannedDocument/SCAN-INV-0042")
  await expect(page.getByRole("heading", { name: "SCAN-INV-0042", level: 1 })).toBeVisible()
  await expect(page.getByRole("tab", { name: "Guarantees" })).toHaveCount(0)
  await expect(page.getByRole("heading", { name: "Rules it keeps" })).toBeVisible()
  await expect(page.getByRole("link", { name: "All rules" })).toHaveAttribute("href", "/rules")
  await page.getByRole("tab", { name: "Rules" }).click()
  await expect(page.getByRole("heading", { name: "Rules over this object type" })).toBeVisible()
  await expect(page.locator('[data-law="canon.ItemizedDocument.total_matches_lines"]')).toContainText("An itemized document's total equals the sum of its lines")
  await expect(page.locator('[data-law="docs.ScannedDocument.total_matches_ledger"]')).toContainText("A booked document's total equals its journal entry")
  await shoot(page, "rules-record-tab")

  // Object type page Rules tab (Workspace Records view).
  await page.goto("/objects/docs.ScannedDocument")
  await page.getByRole("tab", { name: "Rules" }).click()
  const rule = page.locator('[data-law="canon.ItemizedDocument.total_matches_lines"]').first()
  await expect(rule).toContainText("500+ held back")
  await expect(rule.getByRole("link", { name: "Use 72.00" })).toHaveAttribute("href", "/sources/docs.ScannedDocument/SCAN-INV-0042")
  await shoot(page, "rules-type-tab")
})
