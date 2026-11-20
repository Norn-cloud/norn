import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { CORRECT, RECORD, refusalCell, type Reader } from "./refusal-cell.ts"

// The refusal panel, one test per row of its verb matrix: a governed correction the cell refuses by
// law, read as the request, the check (each value it compared with where that value came from, and
// the verdict) and the result (who, when, why). Every document is the cell's own, recorded for an
// admin (who holds the refusal-detail grant) and a member (who gets the redacted refusal).
// SHOTS=<dir> saves each state in light and dark.

const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400)
    await page.getByRole("dialog").screenshot({ path: `${shots}/studio-ui-refusal-${name}-${scheme}.png` })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: replays recorded cell documents`) })
test.beforeEach(async ({ page }) => { await page.setViewportSize({ width: 1440, height: 1000 }) })

const retained = (name: string) => (JSON.parse(readFileSync(new URL(`../../../fixtures/refusal/${name}`, import.meta.url), "utf8")) as { body: Record<string, unknown> }).body
const principal = (reader: Reader) => retained(`access.${reader}.json`).principal as { subjectId: string }
const actionTitle = () => ((retained("actions.admin.json").items as { actionId: string; title: string }[]).find(({ actionId }) => actionId === CORRECT))!.title

/** Open the record, open the correction, set the total the law refuses, and send it (USD, its one currency, is set for it). */
async function refuse(page: Page, reader: Reader) {
  const cell = await refusalCell(page, reader)
  await page.goto(`/sources/${RECORD.objectType}/${RECORD.id}`)
  await page.getByRole("button", { name: actionTitle() }).click()
  const form = page.getByRole("dialog")
  await form.getByLabel("Corrected total").fill("30.00")
  await form.getByLabel("Reason").fill("typo")
  await form.getByLabel("Evidence").fill(`${RECORD.id}~total`)
  await form.locator('button[type="submit"]').click()
  return { cell, form }
}

test("disclosed: the request, each value the law compared with where it came from, the verdict, and who, when and why", async ({ page }) => {
  const { cell, form } = await refuse(page, "admin")
  const panel = form.locator('[data-refusal="disclosed"]')
  await expect(panel).toBeVisible()
  // The request: the action, its record and every value this form sent.
  const request = panel.getByRole("region", { name: "The request" })
  await expect(request).toContainText(actionTitle())
  await expect(request).toContainText(RECORD.id)
  await expect(request).toContainText("30.00")
  // The check: the law's verdict and each value it compared, with where that value came from.
  const check = panel.getByRole("region", { name: "The check" })
  await expect(check.locator('[data-verdict="fails"]')).toHaveText("Fails")
  await expect(check.locator('[data-checked="total"] [data-checked-from="request"]')).toHaveText("This request")
  // The sum of the printed lines: each line's amount was read from the scans, as its own provenance says.
  await expect(check.locator('[data-checked="lines.amount"] [data-source="Document scans"]')).toBeVisible()
  // The result: the verified caller as the cell named them, when the answer arrived, and the law's sentence.
  const result = panel.getByRole("region", { name: "The result" })
  await expect(result).toContainText(principal("admin").subjectId)
  await expect(result.locator("time")).toBeVisible()
  await expect(result).toContainText("Nothing was written.")
  expect(cell.calls).toEqual([expect.objectContaining({ id: RECORD.id, total: "30.00" })])
  await shoot(page, "disclosed")
})

test("disclosed: Use puts the value the record holds into the form; the refusal stays, still naming what was sent, until the next send", async ({ page }) => {
  const { cell, form } = await refuse(page, "admin")
  const use = form.getByRole("button", { name: /^Use / })
  await expect(use).toBeVisible()
  const value = (await use.textContent())!.replace(/^Use /, "")
  await use.click()
  await expect(form.getByLabel("Corrected total")).toHaveValue(value)
  const panel = form.locator('[data-refusal="disclosed"]')
  await expect(panel.getByRole("region", { name: "The request" })).toContainText("30.00")
  await expect(panel.getByRole("region", { name: "The request" })).not.toContainText(value)
  // The next send is a new call with the fixed value; the cell decides it again.
  await form.locator('button[type="submit"]').click()
  await expect.poll(() => cell.calls.length).toBe(2)
  expect(cell.calls[1]).toEqual(expect.objectContaining({ total: value }))
  await expect(form.locator("[data-refusal]")).toHaveCount(0)
})

test("redacted: without the refusal-detail grant the panel names no law and no value, and still shows the request and who, when and why", async ({ page }) => {
  const { form } = await refuse(page, "member")
  const panel = form.locator('[data-refusal="redacted"]')
  await expect(panel).toBeVisible()
  await expect(panel.getByRole("region", { name: "The request" })).toContainText(RECORD.id)
  await expect(panel.getByRole("region", { name: "The check" })).toContainText("It does not say which law, or which values, to your role.")
  await expect(panel.locator("[data-checked], [data-verdict]")).toHaveCount(0)
  await expect(panel.getByRole("button", { name: /^Use / })).toHaveCount(0)
  await expect(panel.getByRole("region", { name: "The result" })).toContainText(principal("member").subjectId)
  await shoot(page, "redacted")
})
