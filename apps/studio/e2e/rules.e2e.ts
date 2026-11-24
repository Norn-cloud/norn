import { readdirSync, readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

// RULES: each rule in plain words, grouped by type and rule, with the records it holds back now
// (lower-bound counts, withholding and disagreement semantics, main's record title resolution, and
// the record review link), platform rules folded under System, the /guarantees → /rules
// redirect, held-work reachability from Workspace/Inbox, and the record & type Rules tabs.
const root = new URL("../../../", import.meta.url)
const read = (path: string) => JSON.parse(readFileSync(new URL(path, root), "utf8")) as { artifactId?: string; payload?: Record<string, unknown> }
const base = read("fixtures/demo/explorer/three-systems.definitions.json") as unknown as { artifactId: string }[]
const scans = readdirSync(new URL("fixtures/packs/scans/artifacts/", root))
  .filter((name) => name.endsWith(".json"))
  .map((name) => read(`fixtures/packs/scans/artifacts/${name}`) as { artifactId: string; payload?: Record<string, unknown> })
const seen = new Set(base.map(({ artifactId }) => artifactId))
const warningLaw = {
  artifactId: "docs.ScannedDocument.vendor_present.law",
  dependencies: [],
  irVersion: "0.1",
  kind: "law",
  lifecycle: "active",
  name: "Every scanned document names a vendor",
  namespace: "docs",
  payload: {
    id: "docs.ScannedDocument.vendor_present",
    lawVersion: "0.1",
    modality: { kind: "always" },
    owner: "docs",
    provenance: { source: "authoring_sdk", sourcePath: "fixtures/packs/scans/scans.ts" },
    rationale: "A scanned document without a vendor cannot be routed for review.",
    response: { expression: "self.vendor != ''", kind: "predicate" },
    scope: { objectTypes: ["docs.ScannedDocument"], root: { id: "docs.ScannedDocument", kind: "entity" } },
    severity: "warning",
    subject: "docs.ScannedDocument",
  },
  provenance: { source: "system" },
  scope: {},
  version: "0.1.0",
}
const artifacts = [...base, ...scans.filter(({ artifactId }) => !seen.has(artifactId)), warningLaw]

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
      suggestedFix: {
        actionId: "docs.scanned_document.correct",
        title: "Correct an extracted total",
        label: "Use 72.00",
        property: "total",
        value: "72.00",
        arguments: {
          id: "SCAN-INV-0042",
          total: "72.00",
          currency: "USD",
          correction_evidence: "SCAN-INV-0042~total",
        },
        needs: ["correction_reason"],
      },
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
      suggestedFix: {
        actionId: "docs.scanned_document.correct",
        title: "Correct an extracted total",
        label: "Use 140.00",
        property: "total",
        value: "140.00",
        arguments: {
          id: "SCAN-INV-0099",
          total: "140.00",
          currency: "USD",
          correction_evidence: "SCAN-INV-0099~total",
        },
        needs: ["correction_reason"],
      },
    },
  ],
}

// Server returns raw engine IDs as titles for built-ins, invariants and unmatched laws (inbox.rs:625-641).
const heldBuiltin = {
  kind: "held_back",
  ruleRef: { kind: "builtin", id: "core.boundary.link_held", version: "1" },
  title: "core.boundary.link_held",
  rationale: null,
  objectType: "docs.ScannedDocument",
  count: 1,
  approximate: false,
  target: "/objects/docs.ScannedDocument?population=held_back",
  records: [],
}

const heldInvariant = {
  kind: "held_back",
  ruleRef: { kind: "invariant", invariantId: "non_negative_quantity", owner: { kind: "object", name: "docs.ScannedDocument" }, version: "sha256:inv" },
  title: "non_negative_quantity",
  rationale: null,
  objectType: "docs.ScannedDocument",
  count: 1,
  approximate: false,
  target: "/objects/docs.ScannedDocument?population=held_back",
  records: [],
}

const heldUnmatchedLaw = {
  kind: "held_back",
  ruleRef: { kind: "law", artifactId: "docs.ScannedDocument.unmatched_check.law", lawId: "docs.ScannedDocument.unmatched_check", version: "sha256:2" },
  title: "docs.ScannedDocument.unmatched_check",
  rationale: null,
  objectType: "docs.ScannedDocument",
  count: 1,
  approximate: false,
  target: "/objects/docs.ScannedDocument?population=held_back",
  records: [],
}

const defaultInbox = {
  scanLimit: 10000,
  items: [elsewhere, heldLines, heldLedger, heldBuiltin, heldInvariant, heldUnmatchedLaw],
  sections: [
    { kind: "held_back", count: 509, shown: 6, approximate: true },
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

interface CellOptions {
  readonly inbox?: unknown
  readonly profile?: { readonly rows: number; readonly quarantinedRows: number; readonly approximate: boolean; readonly scanLimit: number }
  readonly servesRecord?: boolean
}

async function cell(page: Page, options: CellOptions = {}) {
  await session(page)
  await page.route("**/api/host/v1/**", (route) => {
    const url = new URL(route.request().url())
    const path = url.pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts } })
    if (path.endsWith("/v1/definitions:list")) return route.fulfill({ json: { items: [], cursor: null } })
    if (path.endsWith("/v1/inbox")) return route.fulfill({ json: options.inbox ?? defaultInbox })
    if (path.endsWith("/v1/decisions")) return route.fulfill({ json: { items: [], total: 0, approximate: false, serverNowMs: Date.UTC(2026, 10, 23, 12, 0, 0) } })
    if (path.endsWith("/v1/decisions/requested")) return route.fulfill({ json: { items: [], serverNowMs: Date.UTC(2026, 10, 23, 12, 0, 0) } })
    const notFound = { status: 404, json: { code: "object.not_found", message: "no such readable object" } }
    if (path.endsWith("/v1/actions")) {
      return url.searchParams.has("objectId") && !options.servesRecord
        ? route.fulfill(notFound)
        : route.fulfill({ json: { items: [] } })
    }
    if (path.endsWith("/v1/objects/docs.ScannedDocument:profile")) {
      const profile = options.profile ?? { rows: 9500, quarantinedRows: 500, approximate: true, scanLimit: 10000 }
      return route.fulfill({ json: { objectType: "docs.ScannedDocument", withheldRows: 0, engine: "cell", properties: [], ...profile } })
    }
    if (path.endsWith("/v1/objects/docs.ScannedDocument:quarantined")) {
      return route.fulfill({ json: { objectType: "docs.ScannedDocument", items: [{ id: "SCAN-INV-0042", openFindings: 1 }], approximate: true, scanLimit: 10000 } })
    }
    if (path.endsWith("/v1/objects/docs.ScannedDocument/SCAN-INV-0042")) return route.fulfill(options.servesRecord ? { json: scannedRecord } : notFound)
    if (path.endsWith("/v1/objects/docs.ScannedDocument/SCAN-INV-0042/links")) return route.fulfill(options.servesRecord ? { json: { objectType: "docs.ScannedDocument", id: "SCAN-INV-0042", items: [], next: null } } : notFound)
    if (path.endsWith("/v1/objects/docs.ScannedDocument/SCAN-INV-0042/findings")) {
      return route.fulfill({
        json: {
          objectType: "docs.ScannedDocument",
          id: "SCAN-INV-0042",
          quarantined: !options.servesRecord,
          evidence: [],
          findings: options.servesRecord ? [] : [{
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

test("rules: /guarantees redirects preserving suffix/query/hash, nav & palette use Rules, severity stays separate from enforcement, and raw engine IDs stay under Technical", async ({ page }) => {
  await cell(page)

  // Redirect preserves path suffix, query string and hash.
  await page.goto("/guarantees/detail/sub?tab=held&from=legacy#section-1")
  await expect(page).toHaveURL((url) => url.pathname === "/rules/detail/sub" && url.search === "?tab=held&from=legacy" && url.hash === "#section-1")

  // Redirect with ?view=held and hash lands on the Held back tab with both intact.
  await page.goto("/guarantees?view=held#held-top")
  await expect(page).toHaveURL((url) => url.pathname === "/rules" && url.search === "?view=held" && url.hash === "#held-top")
  await expect(page.getByRole("tab", { name: /Held back/ })).toHaveAttribute("aria-selected", "true")

  // Bare /guarantees redirects to /rules.
  await page.goto("/guarantees")
  await expect(page).toHaveURL((url) => url.pathname === "/rules" && url.search === "" && url.hash === "")
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

  // Reporting severity stays separate from enforcement: warning-severity governing laws are not labeled "Warns".
  const severityKpi = page.locator('[data-slot="kpi"]', { hasText: "Reporting severity" })
  await expect(severityKpi).toContainText("separate from enforcement")
  await expect(severityKpi).toContainText("1 warning")
  await expect(page.locator('[data-slot="kpi"]', { hasText: "Refuse a breaking write" })).toHaveCount(0)
  const warningRow = page.locator('[data-law="docs.ScannedDocument.vendor_present"]').first()
  await expect(warningRow).toContainText("Every scanned document names a vendor")
  await expect(warningRow).toContainText("Warning severity")
  await expect(warningRow).not.toContainText("Warns")

  // Business rules lead with authored names; raw engine IDs (including built-ins, invariants and unmatched laws) stay hidden in Business view.
  const rule = page.locator('[data-law="canon.ItemizedDocument.total_matches_lines"]').first()
  await expect(rule).toContainText("An itemized document's total equals the sum of its lines")
  await expect(rule).toContainText("Error severity")
  await expect(rule).toContainText("from Itemized document")
  await expect(rule).not.toContainText("canon.ItemizedDocument.total_matches_lines")
  await expect(page.locator('[data-law="docs.ScannedDocument.total_matches_ledger"]').first()).toContainText("A booked document's total equals its journal entry")

  const builtinRow = page.locator('[data-rule="docs.ScannedDocument:builtin:core.boundary.link_held"]').first()
  await expect(builtinRow).toContainText("Held link")
  await expect(builtinRow).not.toContainText("core.boundary.link_held")

  const invariantRow = page.locator('[data-rule="docs.ScannedDocument:invariant:non_negative_quantity"]').first()
  await expect(invariantRow).toContainText("Non negative quantity")
  await expect(invariantRow).not.toContainText("non_negative_quantity")

  const unmatchedRow = page.locator('[data-rule="docs.ScannedDocument:law:docs.ScannedDocument.unmatched_check"]').first()
  await expect(unmatchedRow).toContainText("Unmatched check")
  await expect(unmatchedRow).not.toContainText("docs.ScannedDocument.unmatched_check")

  // Platform rules fold under System rules, hiding engine rationales until opened.
  await expect(page.getByText(/RESTATE-HISTORY-1/)).toBeHidden()
  await shoot(page, "rules-all")
  await page.getByText(/^System rules/).click()
  await expect(page.getByText("Approve complete restatement generations")).toBeVisible()

  // Technical view reveals engine IDs for laws, built-ins, invariants, unmatched laws and type IDs.
  await page.getByRole("button", { name: "Account" }).click()
  await page.getByRole("menuitemradio", { name: "Technical" }).click()
  await expect(rule).toContainText("canon.ItemizedDocument.total_matches_lines")
  await expect(builtinRow).toContainText("core.boundary.link_held")
  await expect(invariantRow).toContainText("non_negative_quantity")
  await expect(unmatchedRow).toContainText("docs.ScannedDocument.unmatched_check")
  await expect(page.getByText("docs.ScannedDocument", { exact: true }).first()).toBeVisible()
})

test("rules: held-record counts preserve lower bounds (including profile), scope withholding/disagreement to rows, and link to the quarantined record", async ({ page }) => {
  await cell(page)
  await page.goto("/rules")

  // StatStrip labels per-rule holds accurately and names shown type-rule groups, never "rules".
  const heldKpi = page.locator('[data-slot="kpi"]', { hasText: "Held back (per rule)" })
  await expect(heldKpi).toContainText("509+")
  await expect(heldKpi).toContainText("showing 6 of 6+ type-rule groups")
  await expect(heldKpi).toContainText("scan bound")

  // Profile counts on Rules qualify both quarantined and served counts as lower bounds when approximate is true.
  const profileSummary = page.locator('[data-profile-summary="docs.ScannedDocument"]')
  await expect(profileSummary).toHaveAttribute("data-approximate", "true")
  await expect(profileSummary.locator("[data-profile-quarantined]")).toHaveText("500+ in quarantine")
  await expect(profileSummary.locator("[data-profile-served]")).toHaveText("9500+ served")
  await expect(profileSummary).toContainText("Lower bound · scan bound of 10,000 rows reached")

  // Scanned document's inherited rule shows 500+ (not LedgerEntry's 5 listed first in inbox).
  const rule = page.locator('[data-law="canon.ItemizedDocument.total_matches_lines"]').first()
  const chip = rule.locator("[data-held]")
  await expect(chip).toHaveText("500+ held back")
  await expect(chip).toHaveAttribute("data-held", "500")
  await expect(chip).toHaveAttribute("data-approximate", "true")
  await expect(chip).toHaveAttribute("title", /At least 500 held back.*10,000 rows/)

  // Record 1 (scoped to its row): main's title resolution uses id (never guessing doc_number INV-0042), shows the finding and a link to review the record.
  const row1 = rule.locator('[data-held-record="SCAN-INV-0042"]')
  await expect(row1.getByRole("link", { name: "SCAN-INV-0042", exact: true })).toHaveAttribute("href", "/sources/docs.ScannedDocument/SCAN-INV-0042")
  await expect(row1.getByRole("link", { name: "INV-0042", exact: true })).toHaveCount(0)
  await expect(row1).toContainText("total 720.00 ≠ lines 72.00")
  await expect(row1).not.toContainText("Needs review")
  const review = row1.getByRole("link", { name: "Review fix", exact: true })
  await expect(review).toHaveAttribute("href", "/sources/docs.ScannedDocument/SCAN-INV-0042")

  // Record 2 (scoped to its row): label resolves via recordName; disagreement shows badge and other finding, and offers no fix control.
  const row2 = rule.locator('[data-held-record="SCAN-INV-0043"]')
  await expect(row2.getByRole("link", { name: "October office supply invoice", exact: true })).toHaveAttribute("href", "/sources/docs.ScannedDocument/SCAN-INV-0043")
  await expect(row2).toContainText("total 720.00 ≠ lines 72.00")
  await expect(row2).toContainText("Needs review: 2 laws disagree")
  await expect(row2).toContainText("total 720.00 ≠ ledger entry 72.00")
  await expect(row2.getByRole("link", { name: "Review fix", exact: true })).toHaveCount(0)
  await expect(row2.getByRole("link")).toHaveCount(1)

  // Record 3 (scoped to its row): withheld values/finding shows record id link only, with no finding details, disagreement, or fix controls.
  const row3 = rule.locator('[data-held-record="SCAN-INV-0044"]')
  await expect(row3.getByRole("link", { name: "SCAN-INV-0044", exact: true })).toHaveAttribute("href", "/sources/docs.ScannedDocument/SCAN-INV-0044")
  await expect(row3).toHaveText("SCAN-INV-0044")
  await expect(row3).not.toContainText("≠")
  await expect(row3).not.toContainText("Needs review")
  await expect(row3.getByRole("link")).toHaveCount(1)
  await expect(row3.getByRole("button")).toHaveCount(0)

  // Review all link carries the lower bound and opens the type's Quarantine tab.
  const reviewAll = rule.getByRole("link", { name: "Review all 500+" })
  await expect(reviewAll).toHaveAttribute("href", "/objects/docs.ScannedDocument?population=held_back")

  await review.click()
  await expect(page).toHaveURL(/\/sources\/docs\.ScannedDocument\/SCAN-INV-0042$/)
  await expect(page.getByRole("heading", { name: "SCAN-INV-0042", level: 1 })).toBeVisible()
  await expect(page.getByRole("status").filter({ hasText: "Held in quarantine" })).toBeVisible()
  await expect(page.getByText("total 720.00 ≠ lines 72.00", { exact: true })).toBeVisible()
  await expect(page.getByRole("dialog")).toHaveCount(0)
  expect(await page.evaluate(() => window.history.state?.suggestedFix)).toBeUndefined()
  await shoot(page, "rules-review-quarantine")

  // Going back and clicking Review all lands directly on the type's Quarantine tab.
  await page.goBack()
  await expect(page).toHaveURL((url) => url.pathname === "/rules")
  await rule.getByRole("link", { name: "Review all 500+" }).click()
  await expect(page).toHaveURL(/\/objects\/docs\.ScannedDocument\?population=held_back$/)
  await expect(page.getByRole("tab", { name: "Quarantine" })).toHaveAttribute("aria-selected", "true")
})

test("rules: empty bounded scan reports incomplete scan instead of Nothing held back, and truncated groups show shown-of-total scope", async ({ page }) => {
  // 1. Empty bounded scan (count: 0, approximate: true).
  await cell(page, {
    inbox: {
      scanLimit: 10000,
      items: [],
      sections: [
        { kind: "held_back", count: 0, shown: 0, approximate: true },
        { kind: "decision", count: 0, shown: 0, approximate: false },
        { kind: "source_error", count: 0, shown: 0, approximate: false },
      ],
    },
  })
  await page.goto("/rules?view=held")
  const heldKpi = page.locator('[data-slot="kpi"]', { hasText: "Held back (per rule)" })
  await expect(heldKpi).toContainText("0+")
  await expect(heldKpi).toContainText("scan incomplete")
  await expect(page.getByRole("tab", { name: /Held back/ })).toContainText("0+")
  await expect(page.getByText("None found within the scan bound")).toBeVisible()
  await expect(page.getByText(/first 10,000 scanned rows/)).toBeVisible()
  await expect(page.getByText("Nothing held back")).toHaveCount(0)
  await shoot(page, "rules-held-bounded-empty")

  // 2. Truncated group list (server capped groups at 20 after counting all matches: shownHolds < section.count).
  await page.unrouteAll({ behavior: "ignoreErrors" })
  await cell(page, {
    inbox: {
      scanLimit: 10000,
      items: [elsewhere, heldLines],
      sections: [
        { kind: "held_back", count: 600, shown: 2, approximate: false },
        { kind: "decision", count: 0, shown: 0, approximate: false },
        { kind: "source_error", count: 0, shown: 0, approximate: false },
      ],
    },
  })
  await page.goto("/rules?view=held")
  await expect(heldKpi).toContainText("600")
  await expect(heldKpi).toContainText("showing 505 of 600 in top 2 type-rule groups")
  await expect(page.getByText("Showing top 2 type-rule groups (505 of 600 per-rule holds; a record held by two rules counts once per rule).")).toBeVisible()
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
  await expect(inboxLink).toContainText("509+")
  await inboxLink.click()
  await expect(page).toHaveURL((url) => url.pathname === "/rules" && url.searchParams.get("view") === "held")
  await expect(page.getByRole("tab", { name: /Held back/ })).toHaveAttribute("aria-selected", "true")
})

test("rules: the record page and object type page both expose a Rules tab leading with authored names", async ({ page }) => {
  await cell(page, { servesRecord: true })
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
  await expect(rule.getByRole("link", { name: "Review fix", exact: true })).toHaveAttribute("href", "/sources/docs.ScannedDocument/SCAN-INV-0042")
  await shoot(page, "rules-type-tab")
})
