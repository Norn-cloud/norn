import { expect, test, type Page } from "@playwright/test"
import { importTriageCell } from "./import-triage-cell.ts"
import { session } from "./object-cell.ts"

// End-to-end verification for the domain-agnostic Split-Pane Import Triage surface (#1594):
// Step 1 (Match aliases / entities), Step 2 (Split-pane artifact triage + batch governed promotion),
// Step 3 (Supersession diff card FEED-CONFLICT), persistent honest status badges, and unavailable/no-cell states.
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

test("step 1 (aliases): Suggested — you decide is never auto-linked; searchable picker, + Create, and Ignore commit through governed actions", async ({ page }) => {
  await session(page)
  const cell = await importTriageCell(page)
  await page.goto("/imports?step=aliases")

  await expect(page.getByRole("heading", { level: 1, name: "Import triage" })).toBeVisible()
  const northRow = page.locator('[data-alias-row="alias-north-1"]')
  await expect(northRow).toBeVisible()
  await expect(northRow.locator("[data-advisory-badge]")).toHaveText("Suggested — you decide")
  await expect(northRow.locator('[data-fact-status="imported_unverified"]')).toHaveText("Imported (unverified)")
  // Bind button stays disabled until the operator explicitly selects a record (never auto-linked).
  await expect(northRow.getByRole("button", { name: "Bind alias" })).toBeDisabled()

  await shoot(page, "step1-aliases")

  // Searchable record picker: open, filter for Beta, pick rec-002, and commit governed bind.
  await northRow.getByRole("combobox", { name: /Pick canonical record/ }).click()
  await page.getByLabel("Filter records").fill("Beta")
  await page.getByRole("option", { name: /Unit Beta/ }).getByRole("button").click()
  await northRow.getByRole("button", { name: "Bind alias" }).click()
  await expect(northRow.locator('[data-alias-resolved="bound"]')).toContainText("Bound to Unit Beta")
  await expect(northRow.locator('[data-fact-status="verified_fact"]')).toHaveText("Verified fact")

  // + Create mode on second alias.
  const tempRow = page.locator('[data-alias-row="alias-temp-9"]')
  await tempRow.getByRole("button", { name: "Create", exact: true }).click()
  await tempRow.getByLabel(/New record label/).fill("Unit Epsilon")
  await tempRow.getByRole("button", { name: "Create & bind" }).click()
  await expect(tempRow.locator('[data-alias-resolved="created"]')).toContainText("Created & bound Unit Epsilon")

  // Ignore mode on third alias.
  const botRow = page.locator('[data-alias-row="alias-bot-notify"]')
  await botRow.getByRole("button", { name: "Ignore" }).click()
  await botRow.getByLabel(/Reason to ignore/).fill("Automated relay account")
  await botRow.getByRole("button", { name: "Confirm ignore" }).click()
  await expect(botRow.locator('[data-alias-resolved="ignored"]')).toContainText("Ignored handle")

  expect(cell.calls.map((c) => c.actionId)).toEqual([
    "canon.alias.bind",
    "canon.entity.create_and_bind",
    "canon.alias.ignore",
  ])
})

test("step 2 (split-pane review): zoomable artifact, low-confidence amber edits, red violations quarantined, and batch governed promotion", async ({ page }) => {
  await session(page)
  const cell = await importTriageCell(page)
  await page.goto("/imports?step=aliases")

  // Resolve the alias first so both clean rows are eligible for batch promotion.
  const northRow = page.locator('[data-alias-row="alias-north-1"]')
  await northRow.getByRole("button", { name: "Select suggestion" }).click()
  await northRow.getByRole("button", { name: "Bind alias" }).click()
  await expect(northRow.locator('[data-alias-resolved="bound"]')).toBeVisible()

  // Move to Step 2 — Split-Pane Artifact Triage.
  await page.locator('[data-triage-step="review"]').click()
  await expect(page.locator('[data-pane="artifact"]')).toBeVisible()
  await expect(page.locator('[data-pane="rows"]')).toBeVisible()

  // Verify zoom controls on the left pane.
  await expect(page.locator("[data-zoom-level]")).toHaveText("100%")
  await page.getByRole("button", { name: "Zoom in" }).click()
  await expect(page.locator("[data-zoom-level]")).toHaveText("125%")
  await page.getByRole("button", { name: "Reset zoom" }).click()
  await expect(page.locator("[data-zoom-level]")).toHaveText("100%")

  // Verify amber low-confidence field and red law violations are highlighted honestly.
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

  // Click bounding box on left pane and edit low-confidence amber field on right pane.
  await page.locator('[data-bbox-field="f-1-hours"]').click()
  await lowConfField.locator("input").fill("8.50")
  await expect(lowConfField).toContainText("Extracted: 8.00")

  await shoot(page, "step2-review")

  // One-click batch governed promotion promotes ONLY the 2 clean rows while keeping the 2 violating rows quarantined.
  const promoteBtn = page.getByRole("button", { name: "Promote 2 clean rows" })
  await expect(promoteBtn).toBeVisible()
  await promoteBtn.click()

  await expect(row1.locator('[data-fact-status="verified_fact"]')).toHaveText("Verified fact")
  await expect(page.locator('[data-candidate-row="row-w47-2"]').locator('[data-fact-status="verified_fact"]')).toHaveText("Verified fact")
  // Both violating rows remain strictly quarantined.
  await expect(violatingRow.locator('[data-fact-status="quarantined"]')).toHaveText("Quarantined")
  await expect(invalidDateRow.locator('[data-fact-status="quarantined"]')).toHaveText("Quarantined")

  const promoteCalls = cell.calls.filter((c) => c.actionId === "canon.import.promote_row")
  expect(promoteCalls.map((c) => c.input.rowId)).toEqual(["row-w47-1", "row-w47-2"])
  expect((promoteCalls[0]!.input.values as Record<string, string>).duration_hours).toBe("8.50")
})

test("step 2 (server law refusal): a row refused by the cell during batch promotion stays quarantined with the refusal message", async ({ page }) => {
  await session(page)
  const cell = await importTriageCell(page)
  cell.refuseNextPromote("row-w47-1", "law.boundary_cap", "Cell law refused duration 8.00 for this allocation window.")
  await page.goto("/imports?step=review")

  await page.getByRole("button", { name: "Promote 1 clean row" }).click()
  const row1 = page.locator('[data-candidate-row="row-w47-1"]')
  await expect(row1.locator('[data-fact-status="quarantined"]')).toHaveText("Quarantined")
  await expect(row1).toContainText("law.boundary_cap")
  await expect(row1).toContainText("Cell law refused duration 8.00")
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
  // Changed-only view shows the 2 changed cells by default; toggling shows all 3.
  await expect(card.locator("tbody tr")).toHaveCount(2)
  await card.getByRole("button", { name: "All cells (3)" }).click()
  await expect(card.locator("tbody tr")).toHaveCount(3)
  await card.getByRole("button", { name: "Changed only (2)" }).click()
  await expect(card.locator("tbody tr")).toHaveCount(2)

  // Default choice is Accept newer artifact.
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

test("unavailable and no-cell states render clean empty states with page heading and no stray calls", async ({ page }) => {
  await session(page)
  await page.route("**/api/host/v1/imports/triage**", (route) => route.fulfill({ status: 404, body: "" }))
  await page.goto("/imports")
  await expect(page.getByRole("heading", { level: 1, name: "Import triage" })).toBeVisible()
  await expect(page.getByText("This cell doesn't serve import triage batches yet.")).toBeVisible()

  await page.unrouteAll({ behavior: "ignoreErrors" })
  await session(page, null)
  let calls = 0
  await page.route("**/api/host/**", (route) => { calls += 1; return route.fulfill({ status: 500 }) })
  await page.goto("/imports")
  await expect(page.getByText("This workspace has no data cell yet.")).toBeVisible()
  expect(calls).toBe(0)
})
