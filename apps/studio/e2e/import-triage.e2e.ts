import { expect, test, type Page } from "@playwright/test"
import { importTriageCell } from "./import-triage-cell.ts"
import { session } from "./object-cell.ts"

// End-to-end verification for the domain-agnostic Split-Pane Import Triage surface (#1594):
// Step 1 (Match aliases / entities + combobox keyboard navigation + neutral Ignored badge),
// Step 2 (Split-pane artifact triage + keyboard pan/zoom + bbox filter reveal + withheld fields +
//   idempotency key reuse on retry + batch input locking + distinct non-fabricated request outcomes),
// Step 3 (Supersession diff card FEED-CONFLICT), and 404 route.not_found / no-cell states.
// SHOTS=<dir> captures light and dark screenshots of all three triage steps.

const shots = process.env.SHOTS

async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(300)
    await page.screenshot({ path: `${shots}/import-triage-${name}-${scheme}.png`, fullPage: true })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(({ }, info) => {
  test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses synthetic import triage stand-in`)
})

test.beforeEach(async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 960 })
})

test("step 1 (aliases): keyboard combobox, Suggested — you decide, + Create, and neutral Ignored badge", async ({ page }) => {
  await session(page)
  const cell = await importTriageCell(page)
  await page.goto("/imports?step=aliases")

  await expect(page.getByRole("heading", { level: 1, name: "Import triage" })).toBeVisible()
  const northRow = page.locator('[data-alias-row="alias-north-1"]')
  await expect(northRow).toBeVisible()
  await expect(northRow.locator("[data-advisory-badge]")).toHaveText("Suggested — you decide")
  await expect(northRow.locator('[data-fact-status="imported_unverified"]')).toHaveText("Imported (unverified)")
  await expect(northRow.getByRole("button", { name: "Bind alias" })).toBeDisabled()

  await shoot(page, "step1-aliases")

  // Keyboard combobox navigation: open, ArrowDown to second option (Unit Beta), Enter to pick.
  await northRow.getByRole("combobox", { name: /Pick canonical record/ }).click()
  const filterInput = page.getByLabel("Filter records")
  await expect(filterInput).toBeFocused()
  await page.keyboard.press("ArrowDown")
  await page.keyboard.press("Enter")
  await northRow.getByRole("button", { name: "Bind alias" }).click()
  await expect(northRow.locator('[data-alias-resolved="bound"]')).toContainText("Bound to Unit Beta")
  await expect(northRow.locator('[data-fact-status="verified_fact"]')).toHaveText("Verified fact")

  // Create mode on second alias.
  const tempRow = page.locator('[data-alias-row="alias-temp-9"]')
  await tempRow.getByRole("button", { name: "Create", exact: true }).click()
  await tempRow.getByLabel(/New record label/).fill("Unit Epsilon")
  await tempRow.getByRole("button", { name: "Create & bind" }).click()
  await expect(tempRow.locator('[data-alias-resolved="created"]')).toContainText("Created & bound Unit Epsilon")

  // Ignore mode on third alias: badged as neutral "Ignored", NEVER "Verified fact".
  const botRow = page.locator('[data-alias-row="alias-bot-notify"]')
  await botRow.getByRole("button", { name: "Ignore" }).click()
  await botRow.getByLabel(/Reason to ignore/).fill("Automated relay account")
  await botRow.getByRole("button", { name: "Confirm ignore" }).click()
  await expect(botRow.locator('[data-alias-resolved="ignored"]')).toContainText("Ignored handle")
  await expect(botRow.locator('[data-alias-status="ignored"]')).toHaveText("Ignored")
  await expect(botRow.locator('[data-fact-status="verified_fact"]')).toHaveCount(0)

  expect(cell.calls.map((c) => c.actionId)).toEqual([
    "canon.alias.bind",
    "canon.entity.create_and_bind",
    "canon.alias.ignore",
  ])
})

test("step 2 (split-pane review): keyboard zoom/pan, bbox clears hiding filter, draft persists across steps, and batch promotion locks inputs", async ({ page }) => {
  await session(page)
  const cell = await importTriageCell(page)
  await page.goto("/imports?step=aliases")

  // Bind alias so both clean rows are eligible for batch promotion.
  const northRow = page.locator('[data-alias-row="alias-north-1"]')
  await northRow.getByRole("button", { name: "Select suggestion" }).click()
  await northRow.getByRole("button", { name: "Bind alias" }).click()
  await expect(northRow.locator('[data-alias-resolved="bound"]')).toBeVisible()

  // Move to Step 2 — Split-Pane Artifact Triage.
  await page.locator('[data-triage-step="review"]').click()
  await expect(page.locator('[data-pane="artifact"]')).toBeVisible()
  await expect(page.locator('[data-pane="rows"]')).toBeVisible()

  // Zoom in -> keyboard Pan buttons appear and adjust pan offset -> Fit resets both.
  await expect(page.locator("[data-zoom-level]")).toHaveText("100%")
  await page.getByRole("button", { name: "Zoom in" }).click()
  await expect(page.locator("[data-zoom-level]")).toHaveText("125%")
  await page.getByRole("button", { name: "Pan right" }).click()
  await expect(page.locator("[data-pan-offset]")).toHaveAttribute("data-pan-offset", "-40,0")
  await page.getByRole("button", { name: "Reset zoom" }).click()
  await expect(page.locator("[data-zoom-level]")).toHaveText("100%")
  await expect(page.locator("[data-pan-offset]")).toHaveAttribute("data-pan-offset", "0,0")

  // Verify amber low-confidence field and red law violations (with verbatim invalid date).
  const row1 = page.locator('[data-candidate-row="row-w47-1"]')
  const lowConfField = row1.locator('[data-field-name="duration_hours"]')
  await expect(lowConfField).toHaveAttribute("data-field-tone", "amber")
  await expect(lowConfField.locator("[data-low-confidence]")).toContainText("68.0%")

  const violatingRow = page.locator('[data-candidate-row="row-w47-3"]')
  await expect(violatingRow.locator('[data-fact-status="quarantined"]')).toHaveText("Quarantined")
  await expect(violatingRow).toContainText("OBS-BAD")

  const invalidDateRow = page.locator('[data-candidate-row="row-w47-4"]')
  await expect(invalidDateRow.locator('[data-fact-status="quarantined"]')).toHaveText("Quarantined")
  await expect(invalidDateRow).toContainText("InvalidTemporalObservation")
  await expect(invalidDateRow).toContainText("effective 2026-02-31")

  // Switch filter to Violations (hiding row 1), then click row 1's bounding box -> clears filter and focuses row 1!
  await page.getByRole("button", { name: "Violations (2)" }).click()
  await expect(row1).toHaveCount(0)
  await page.locator('[data-bbox-field="f-1-hours"]').click()
  await expect(row1).toBeVisible()

  // Edit low-confidence field, switch to Step 1 and back to Step 2 -> unsent draft is preserved!
  await lowConfField.locator("input").fill("8.50")
  await expect(lowConfField).toContainText("Extracted: 8.00")
  await page.locator('[data-triage-step="aliases"]').click()
  await page.locator('[data-triage-step="review"]').click()
  await expect(lowConfField.locator("input")).toHaveValue("8.50")

  await shoot(page, "step2-review")

  // Hold row 1 promotion while batch starts to verify queued row inputs are locked mid-batch.
  const releaseRow1 = cell.holdNextPromote("row-w47-1")
  const promoteBtn = page.getByRole("button", { name: "Promote 2 clean rows" })
  await promoteBtn.click()
  await expect(row1.locator('[data-field-name="duration_hours"] input')).toBeDisabled()
  await expect(page.locator('[data-candidate-row="row-w47-2"] [data-field-name="duration_hours"] input')).toBeDisabled()
  releaseRow1()

  await expect(row1.locator('[data-fact-status="verified_fact"]')).toHaveText("Verified fact")
  await expect(page.locator('[data-candidate-row="row-w47-2"]').locator('[data-fact-status="verified_fact"]')).toHaveText("Verified fact")
  await expect(violatingRow.locator('[data-fact-status="quarantined"]')).toHaveText("Quarantined")
  await expect(invalidDateRow.locator('[data-fact-status="quarantined"]')).toHaveText("Quarantined")

  const promoteCalls = cell.calls.filter((c) => c.actionId === "canon.import.promote_row")
  expect(promoteCalls.map((c) => c.input.rowId)).toEqual(["row-w47-1", "row-w47-2"])
  expect((promoteCalls[0]!.input.values as Record<string, string>).duration_hours).toBe("8.50")
})

test("step 2 (honest outcomes & idempotency): error retry reuses same key, edit rotates key, pending/refused never fabricate quarantine findings", async ({ page }) => {
  await session(page)
  const cell = await importTriageCell(page)
  await page.goto("/imports?step=review")

  const row1 = page.locator('[data-candidate-row="row-w47-1"]')

  // 1) Transport failure (500): shows error outcome banner, does NOT flip status to quarantined, retry reuses exact same key.
  cell.failNextPromote("row-w47-1")
  await row1.getByRole("button", { name: "Promote row" }).click()
  await expect(row1.locator('[data-row-outcome="error"]')).toBeVisible()
  await expect(row1).toHaveAttribute("data-row-status", "imported_unverified")
  await expect(row1.locator("[data-cell-findings]")).toHaveCount(0)

  // Retry without editing -> must reuse exact same idempotencyKey; inject a 202 pending approval on this retry.
  cell.pendingNextPromote("row-w47-1", "dec_approval_99")
  await row1.locator('[data-row-outcome="error"]').getByRole("button", { name: "Retry" }).click()
  await expect(row1.locator('[data-row-outcome="pending"]')).toContainText("dec_approval_99")
  await expect(row1).toHaveAttribute("data-row-status", "imported_unverified")
  await expect(row1.locator("[data-cell-findings]")).toHaveCount(0)

  // 2) Edit field -> rotates idempotency key; inject 422 refusal -> distinct refusal banner, no fabricated cell finding.
  await row1.locator('[data-field-name="duration_hours"] input').fill("9.00")
  cell.refuseNextPromote("row-w47-1", "law.boundary_cap", "Cell law refused duration 9.00.")
  await row1.getByRole("button", { name: "Promote row" }).click()
  await expect(row1.locator('[data-row-outcome="refused"]')).toContainText("law.boundary_cap")
  await expect(row1).toHaveAttribute("data-row-status", "imported_unverified")
  await expect(row1.locator("[data-cell-findings]")).toHaveCount(0)

  const keys = cell.calls.map((c) => c.idempotencyKey)
  expect(keys).toHaveLength(3)
  expect(keys[0]).toBe(keys[1]) // retry reused the exact same key
  expect(keys[2]).not.toBe(keys[1]) // edit rotated to a fresh key
})

test("step 2 (withheld fields & external artifact rejection): redacted fields stay read-only and external HTTP URLs are blocked", async ({ page }) => {
  await session(page)
  await importTriageCell(page, { includeWithheldRow: true, externalArtifactUri: true })
  await page.goto("/imports?step=review")

  // Arbitrary external https:// artifact URI is rejected cleanly before loading.
  await expect(page.locator("[data-media-rejected]")).toContainText("External artifact URLs are blocked")

  // Withheld row shows Withheld status badge and read-only masked field (no editable input).
  const withheldRow = page.locator('[data-candidate-row="row-w47-withheld"]')
  await expect(withheldRow.locator('[data-fact-status="withheld"]')).toHaveText("Withheld")
  const redactedField = withheldRow.locator('[data-field-withheld="redacted"]')
  await expect(redactedField).toContainText("Redacted from your role")
  await expect(redactedField.locator("input")).toHaveCount(0)
})

test("step 3 (supersession diff): FEED-CONFLICT groups both immutable artifacts, shows changed cells, defaults to accepting newer while preserving earlier evidence", async ({ page }) => {
  await session(page)
  const cell = await importTriageCell(page)
  await page.goto("/imports?step=supersession")

  const card = page.locator('[data-supersession-card="conflict-w47-north"]')
  await expect(card).toBeVisible()
  await expect(card).toContainText("FEED-CONFLICT")
  await expect(card).toContainText("+2h 15m later")
  await expect(card.locator('[data-artifact-side="earlier"]')).toContainText("Immutable evidence")
  await expect(card.locator('[data-artifact-side="newer"]')).toContainText("Capture B")
  await expect(card.locator("tbody tr")).toHaveCount(2)
  await card.getByRole("button", { name: "All cells (3)" }).click()
  await expect(card.locator("tbody tr")).toHaveCount(3)
  await card.getByRole("button", { name: "Changed only (2)" }).click()
  await expect(card.locator("tbody tr")).toHaveCount(2)

  await expect(card.getByRole("radio", { name: /Accept newer artifact/ })).toBeChecked()

  await shoot(page, "step3-supersession")

  await card.getByLabel(/Supersession rationale/).fill("Verified against revised capture.")
  await card.getByRole("button", { name: "Resolve supersession" }).click()

  await expect(card.locator('[data-supersession-resolved="accept_newer"]')).toContainText("Accepted newer artifact as superseding correction")
  await expect(card.locator('[data-supersession-resolved="accept_newer"]')).toContainText("art-w47-a")
  await expect(card.locator('[data-fact-status="verified_fact"]')).toHaveText("Verified fact")

  expect(cell.calls).toHaveLength(1)
  expect(cell.calls[0]!.actionId).toBe("canon.import.resolve_supersession")
  expect(cell.calls[0]!.input).toMatchObject({
    conflictId: "conflict-w47-north",
    choice: "accept_newer",
    preserveEarlierEvidence: true,
    earlierArtifactId: "art-w47-a",
    newerArtifactId: "art-w47-b",
  })
})

test("404 route.not_found and no-cell states render calm empty states with page heading and zero refusal alerts", async ({ page }) => {
  await session(page)
  await page.route("**/api/host/v1/imports/triage**", (route) =>
    route.fulfill({ status: 404, json: { code: "route.not_found", category: "route", message: "Route not found." } }))
  await page.goto("/imports")
  await expect(page.getByRole("heading", { level: 1, name: "Import triage" })).toBeVisible()
  await expect(page.getByText("This cell doesn't serve import triage yet.")).toBeVisible()
  await expect(page.getByRole("alert")).toHaveCount(0)

  await page.unrouteAll({ behavior: "ignoreErrors" })
  await session(page, null)
  let calls = 0
  await page.route("**/api/host/**", (route) => { calls += 1; return route.fulfill({ status: 500 }) })
  await page.goto("/imports")
  await expect(page.getByText("This workspace has no data cell yet.")).toBeVisible()
  expect(calls).toBe(0)
})
