import { expect, test, type Page } from "@playwright/test"
import { importTriageCell } from "./import-triage-cell.ts"
import { session } from "./object-cell.ts"

// End-to-end verification for the domain-agnostic Split-Pane Import Triage surface (#1594):
// Step 1 (Match aliases / entities + combobox keyboard navigation + neutral Ignored badge),
// Step 2 (Split-pane artifact triage + keyboard pan/zoom + bbox filter reveal + withheld fields +
//   scoped blob media + image error state + locks surviving step switches + batch switch abort +
//   authoritative verified values + structured 500 retry reusing idempotency key + disputed row hold),
// Step 3 (Supersession diff card FEED-CONFLICT + default retry key stability + honest status badge +
//   releasing disputed row for promotion), and 404 route.not_found / no-cell states.
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

test("step 2 (split-pane review): locks persist across step navigation, disputed FEED-CONFLICT row stays held until Step 3 resolves it", async ({ page }) => {
  await session(page)
  const cell = await importTriageCell(page)
  await page.goto("/imports?step=aliases")

  // Bind alias so both non-disputed clean rows are eligible for batch promotion.
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

  const disputedRow = page.locator('[data-candidate-row="row-w47-5"]')
  await expect(disputedRow.locator('[data-fact-status="quarantined"]')).toHaveText("Quarantined")
  await expect(disputedRow).toContainText("FEED-CONFLICT")

  // Switch filter to Violations (hiding row 1), then click row 1's bounding box -> clears filter and reveals row 1.
  await page.getByRole("button", { name: "Violations (3)" }).click()
  await expect(row1).toHaveCount(0)
  await page.locator('[data-bbox-field="f-1-hours"]').click()
  await expect(row1).toBeVisible()

  // Edit low-confidence field, switch to Step 1 and back to Step 2 -> unsent draft persists.
  await lowConfField.locator("input").fill("8.50")
  await expect(lowConfField).toContainText("Extracted: 8.00")
  await page.locator('[data-triage-step="aliases"]').click()
  await page.locator('[data-triage-step="review"]').click()
  await expect(lowConfField.locator("input")).toHaveValue("8.50")

  await shoot(page, "step2-review")

  // Hold row 1 promotion in flight, switch Step 2 -> Step 1 -> Step 2, and assert locks STILL hold!
  const releaseRow1 = cell.holdNextPromote("row-w47-1")
  await page.getByRole("button", { name: "Promote 2 clean rows" }).click()
  await expect(row1.locator('[data-field-name="duration_hours"] input')).toBeDisabled()
  await expect(page.locator('[data-candidate-row="row-w47-2"] [data-field-name="duration_hours"] input')).toBeDisabled()

  await page.locator('[data-triage-step="aliases"]').click()
  await page.locator('[data-triage-step="review"]').click()
  await expect(row1.locator('[data-field-name="duration_hours"] input')).toBeDisabled()
  await expect(page.locator('[data-candidate-row="row-w47-2"] [data-field-name="duration_hours"] input')).toBeDisabled()
  await expect(page.getByRole("button", { name: "Promoting clean rows…" })).toBeDisabled()
  releaseRow1()

  await expect(row1.locator('[data-fact-status="verified_fact"]')).toHaveText("Verified fact")
  await expect(page.locator('[data-candidate-row="row-w47-2"]').locator('[data-fact-status="verified_fact"]')).toHaveText("Verified fact")
  // All 3 quarantined rows (OBS-BAD, InvalidTemporalObservation, FEED-CONFLICT disputed) stay held!
  await expect(violatingRow.locator('[data-fact-status="quarantined"]')).toHaveText("Quarantined")
  await expect(invalidDateRow.locator('[data-fact-status="quarantined"]')).toHaveText("Quarantined")
  await expect(disputedRow.locator('[data-fact-status="quarantined"]')).toHaveText("Quarantined")

  const promoteCalls = cell.calls.filter((c) => c.actionId === "canon.import.promote_row")
  expect(promoteCalls.map((c) => c.input.rowId)).toEqual(["row-w47-1", "row-w47-2"])
  expect((promoteCalls[0]!.input.values as Record<string, string>).duration_hours).toBe("8.50")

  // Resolve the FEED-CONFLICT in Step 3 -> returns disputed row to Imported (unverified) for governed promotion.
  await page.locator('[data-triage-step="supersession"]').click()
  const card = page.locator('[data-supersession-card="conflict-w47-south"]')
  await card.getByRole("button", { name: "Resolve supersession" }).click()
  await expect(card.locator('[data-supersession-resolved="accept_newer"]')).toBeVisible()
  // Step 3 must reflect authoritative cell status ("Imported (unverified)"), never fabricating "Verified fact".
  await expect(card.locator('[data-fact-status="imported_unverified"]')).toHaveText("Imported (unverified)")
  await expect(card.locator('[data-fact-status="verified_fact"]')).toHaveCount(0)

  // Return to Step 2 and switch to Capture B (art-w47-b) -> disputed row is now released with Capture B's values, boxes, and evidence!
  await page.locator('[data-triage-step="review"]').click()
  await page.getByLabel("Source artifact", { exact: true }).selectOption("art-w47-b")
  await expect(disputedRow.locator('[data-fact-status="imported_unverified"]')).toHaveText("Imported (unverified)")
  await expect(disputedRow.locator('[data-field-name="duration_hours"] input')).toHaveValue("8.50")
  // Capture B bounding box aligns with Capture B's drawn row coordinates (y = 0.30, not Capture A's 0.82).
  await expect(page.locator('[data-bbox-field="f-5-hours"]')).toHaveAttribute("style", /top:\s*30%/)

  // Hold the authoritative refetch so we verify the receipt NEVER optimistically paints "Verified fact",
  // and that batch promotion excludes the committed-awaiting-read row!
  const releaseRead = cell.holdNextTriageRead()
  await disputedRow.getByRole("button", { name: "Promote row" }).click()
  await expect(disputedRow.locator('[data-row-outcome="committed"]')).toBeVisible()
  await expect(disputedRow.locator('[data-fact-status="imported_unverified"]')).toHaveText("Imported (unverified)")
  await expect(disputedRow.locator('[data-fact-status="verified_fact"]')).toHaveCount(0)
  await expect(page.getByRole("button", { name: "Promote 0 clean rows" })).toBeDisabled()

  // Once the authoritative cell read arrives, the row transitions to Verified fact.
  releaseRead()
  await expect(disputedRow.locator('[data-fact-status="verified_fact"]')).toHaveText("Verified fact")

  // Final promotion request for the released row must carry Capture B's evidence digests alongside Capture B's values.
  const disputedPromote = cell.calls.find(
    (c) => c.actionId === "canon.import.promote_row" && c.input.rowId === "row-w47-5",
  )
  expect(disputedPromote).toBeDefined()
  expect(disputedPromote!.input.artifactId).toBe("art-w47-b")
  expect(disputedPromote!.input.blobDigest).toBe(`sha256:${(102).toString(16).padStart(64, "b")}`)
  expect(disputedPromote!.input.functionReceiptDigest).toBe(`sha256:${(202).toString(16).padStart(64, "b")}`)
  expect(disputedPromote!.input.observedAt).toBe("2026-11-16T11:19:00Z")
  expect(disputedPromote!.input.values).toEqual({ slot_code: "SLOT-10", duration_hours: "8.50" })
})

test("step 2 (batch switch mid-queue & stale local draft vs authoritative verified value)", async ({ page }) => {
  await session(page)
  const cell = await importTriageCell(page)
  await page.goto("/imports?step=aliases")
  const northRow = page.locator('[data-alias-row="alias-north-1"]')
  await northRow.getByRole("button", { name: "Select suggestion" }).click()
  await northRow.getByRole("button", { name: "Bind alias" }).click()
  await expect(northRow.locator('[data-alias-resolved="bound"]')).toBeVisible()

  await page.locator('[data-triage-step="review"]').click()
  const row1 = page.locator('[data-candidate-row="row-w47-1"]')

  // 1) Stale local edit is replaced by authoritative committed value when another operator verifies the row.
  await row1.locator('[data-field-name="duration_hours"] input').fill("9.99")
  await expect(row1.locator('[data-field-name="duration_hours"] input')).toHaveValue("9.99")
  cell.commitRowElsewhere("row-w47-1", { duration_hours: "8.25" })
  await page.getByRole("button", { name: "Refresh" }).click()
  await expect(row1.locator('[data-fact-status="verified_fact"]')).toHaveText("Verified fact")
  await expect(row1.locator('[data-field-name="duration_hours"] input')).toHaveValue("8.25")

  // 2) Switch active batch mid-queue: hold row 2, switch to batch-w48-sync, release -> zero extra old-batch dispatches.
  cell.switchBatch("batch-w48-sync")
  await page.getByRole("button", { name: "Refresh" }).click()
  await expect(page.locator('[data-triage-batch="batch-w48-sync"]')).toBeVisible()
  await expect(row1.locator('[data-fact-status="imported_unverified"]')).toHaveText("Imported (unverified)")

  // Bind alias in batch-w48-sync so 2 clean rows exist, then hold row 1 and switch to batch-w49-sync mid-queue.
  await page.locator('[data-triage-step="aliases"]').click()
  await northRow.getByRole("button", { name: "Select suggestion" }).click()
  await northRow.getByRole("button", { name: "Bind alias" }).click()
  await expect(northRow.locator('[data-alias-resolved="bound"]')).toBeVisible()
  await page.locator('[data-triage-step="review"]').click()

  const release = cell.holdNextPromote("row-w47-1")
  await page.getByRole("button", { name: "Promote 2 clean rows" }).click()
  await expect.poll(() => cell.calls.filter((c) => c.actionId === "canon.import.promote_row").length).toBe(1)

  cell.switchBatch("batch-w49-sync")
  await page.getByRole("button", { name: "Refresh" }).click()
  await expect(page.locator('[data-triage-batch="batch-w49-sync"]')).toBeVisible()
  release()
  await page.waitForTimeout(300)

  // Row 2 of batch-w48-sync must NEVER have been dispatched after switching to batch-w49-sync!
  const w48Promotes = cell.calls.filter((c) => c.actionId === "canon.import.promote_row" && c.input.batchId === "batch-w48-sync")
  expect(w48Promotes.map((c) => c.input.rowId)).toEqual(["row-w47-1"])

  // 3) Leaving `/imports` mid-queue unmounts ImportTriagePage and halts remaining queued rows.
  await page.locator('[data-triage-step="aliases"]').click()
  await northRow.getByRole("button", { name: "Select suggestion" }).click()
  await northRow.getByRole("button", { name: "Bind alias" }).click()
  await expect(northRow.locator('[data-alias-resolved="bound"]')).toBeVisible()
  await page.locator('[data-triage-step="review"]').click()

  const releaseW49 = cell.holdNextPromote("row-w47-1")
  await page.getByRole("button", { name: "Promote 2 clean rows" }).click()
  await expect.poll(() => cell.calls.filter((c) => c.actionId === "canon.import.promote_row" && c.input.batchId === "batch-w49-sync").length).toBe(1)

  // Navigate off `/imports` so ImportTriagePage unmounts while row 1 is in flight.
  await page.locator('a[href="/"]').first().click()
  await expect(page.locator('[data-triage-batch]')).toHaveCount(0)
  releaseW49()
  await page.waitForTimeout(300)

  const w49Promotes = cell.calls.filter((c) => c.actionId === "canon.import.promote_row" && c.input.batchId === "batch-w49-sync")
  expect(w49Promotes.map((c) => c.input.rowId)).toEqual(["row-w47-1"])
})

test("step 2 (structured 500 retry, pending approval, and 422 refusal): exact key reuse and zero fabricated findings", async ({ page }) => {
  await session(page)
  const cell = await importTriageCell(page)
  await page.goto("/imports?step=review")

  const row1 = page.locator('[data-candidate-row="row-w47-1"]')
  const hoursInput = row1.locator('[data-field-name="duration_hours"] input')
  const resetHoursBtn = row1.getByRole("button", { name: "Reset Duration (hours) edit" })

  // No Reset button before any local edit exists.
  await expect(resetHoursBtn).toHaveCount(0)

  // Structured JSON HTTP 500 -> retryable error banner (NOT non-retryable refusal), no fabricated finding.
  cell.failNextPromote("row-w47-1")
  await row1.getByRole("button", { name: "Promote row" }).click()
  await expect(row1.locator('[data-row-outcome="error"]')).toContainText("internal.storage_busy")
  await expect(row1).toHaveAttribute("data-row-status", "imported_unverified")
  await expect(row1.locator("[data-cell-findings]")).toHaveCount(0)

  // Retry button reuses the exact same idempotencyKey.
  cell.failNextPromote("row-w47-1")
  await row1.locator('[data-row-outcome="error"]').getByRole("button", { name: "Retry" }).click()
  await expect(row1.locator('[data-row-outcome="error"]')).toContainText("internal.storage_busy")

  // Edit then Reset back to baseline -> Reset disappears, and retrying the unchanged outgoing payload
  // STILL reuses the exact same idempotencyKey!
  await hoursInput.fill("9.50")
  await expect(resetHoursBtn).toBeVisible()
  await resetHoursBtn.click()
  await expect(resetHoursBtn).toHaveCount(0)
  await expect(hoursInput).toHaveValue("8.00")

  // Re-submitting unchanged payload reuses exact same idempotencyKey; 422 refusal -> distinct refusal banner without Retry or fabricated findings.
  cell.refuseNextPromote("row-w47-1", "law.boundary_cap", "Cell law refused duration 8.00.")
  await row1.getByRole("button", { name: "Promote row" }).click()
  await expect(row1.locator('[data-row-outcome="refused"]')).toContainText("law.boundary_cap")
  await expect(row1.locator('[data-row-outcome="refused"]').getByRole("button", { name: "Retry" })).toHaveCount(0)
  await expect(row1).toHaveAttribute("data-row-status", "imported_unverified")
  await expect(row1.locator("[data-cell-findings]")).toHaveCount(0)

  // Edit field -> rotates idempotency key; cell returns 202 pending approval, which completely locks
  // inputs, Reset, single-row Promote, and batch promotion so nothing can re-submit while awaiting approval.
  await hoursInput.fill("9.00")
  await expect(resetHoursBtn).toBeVisible()
  cell.pendingNextPromote("row-w47-1", "dec_approval_99")
  await row1.getByRole("button", { name: "Promote row" }).click()
  await expect(row1.locator('[data-row-outcome="pending"]')).toContainText("dec_approval_99")
  await expect(row1).toHaveAttribute("data-row-status", "imported_unverified")
  await expect(row1.locator("[data-cell-findings]")).toHaveCount(0)
  await expect(hoursInput).toBeDisabled()
  await expect(resetHoursBtn).toBeDisabled()
  await expect(row1.getByRole("button", { name: "Pending approval…" })).toBeDisabled()
  await expect(page.getByRole("button", { name: "Promote 0 clean rows" })).toBeDisabled()

  const keys = cell.calls.map((c) => c.idempotencyKey)
  expect(keys).toHaveLength(4)
  expect(keys[0]).toBe(keys[1])
  expect(keys[2]).toBe(keys[0])
  expect(keys[3]).not.toBe(keys[2])
})

test("step 2 (scoped blob media, broken image error state, withheld fields, and external URL rejection)", async ({ page }) => {
  // 1) Cell-scoped blob path loads via scoped /api/host/v1/blobs/... fetch into a blob: URL.
  await session(page)
  const blobCell = await importTriageCell(page, { includeWithheldRow: true, artifactMediaMode: "cell-blob" })
  await page.goto("/imports?step=review")
  const img = page.locator('[data-pane="artifact"] img')
  await expect(img).toBeVisible()
  await expect(img).toHaveAttribute("src", /^blob:/)
  expect(blobCell.blobRequests[0]).toContain("/api/host/v1/blobs/capture-w47-a")
  expect(blobCell.blobRequests[0]).toContain("orgId=org_demo")
  expect(blobCell.blobRequests[0]).toContain("environmentId=analytics")

  // Withheld row stays read-only with no editable input.
  const withheldRow = page.locator('[data-candidate-row="row-w47-withheld"]')
  await expect(withheldRow.locator('[data-fact-status="withheld"]')).toHaveText("Withheld")
  const redactedField = withheldRow.locator('[data-field-withheld="redacted"]')
  await expect(redactedField).toContainText("Redacted from your role")
  await expect(redactedField.locator("input")).toHaveCount(0)

  // 2) Broken image bytes trigger onError -> shows [data-media-error] and hides bounding-box overlays.
  await page.unrouteAll({ behavior: "ignoreErrors" })
  await session(page)
  await importTriageCell(page, { artifactMediaMode: "broken-blob" })
  await page.goto("/imports?step=review")
  await expect(page.locator("[data-media-error]")).toContainText("Source artifact image could not be loaded")
  await expect(page.locator("[data-bbox-field]")).toHaveCount(0)

  // 3) Arbitrary external https:// artifact URL is rejected before any browser request.
  await page.unrouteAll({ behavior: "ignoreErrors" })
  await session(page)
  await importTriageCell(page, { artifactMediaMode: "external" })
  await page.goto("/imports?step=review")
  await expect(page.locator("[data-media-rejected]")).toContainText("External artifact URLs are blocked")
})

test("step 3 (supersession diff): default submit structured 500 retry reuses exact idempotency key and preserves earlier evidence", async ({ page }) => {
  await session(page)
  const cell = await importTriageCell(page)
  await page.goto("/imports?step=supersession")

  const card = page.locator('[data-supersession-card="conflict-w47-south"]')
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

  // Submit default supersession with an injected structured 500 -> Retry reuses the EXACT same idempotencyKey!
  cell.failNextSupersession("conflict-w47-south")
  await card.getByRole("button", { name: "Resolve supersession" }).click()
  await expect(card.locator('[data-supersession-notice="error"]')).toContainText("internal.storage_busy")
  await card.locator('[data-supersession-notice="error"]').getByRole("button", { name: "Retry" }).click()

  await expect(card.locator('[data-supersession-resolved="accept_newer"]')).toContainText("Accepted newer artifact as superseding correction")
  await expect(card.locator('[data-supersession-resolved="accept_newer"]')).toContainText("art-w47-a")
  await expect(card.locator('[data-fact-status="imported_unverified"]')).toHaveText("Imported (unverified)")

  expect(cell.calls).toHaveLength(2)
  expect(cell.calls[0]!.idempotencyKey).toBe(cell.calls[1]!.idempotencyKey)
  expect(cell.calls[1]!.input).toMatchObject({
    conflictId: "conflict-w47-south",
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
