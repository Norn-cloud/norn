import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { CORRECT, RECORD, REQUESTS, refusalCell, type RefusalCell, type Reader } from "./refusal-cell.ts"

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

/**
 * Open the record, open the correction, set the total the law refuses, and send it (USD, its one
 * currency, is set for it). The total is typed with the spaces a paste brings; the form sends it
 * trimmed, as the cell decodes it. `before` runs once the record page has read everything.
 */
async function refuse(page: Page, reader: Reader, before?: (cell: RefusalCell) => void) {
  const cell = await refusalCell(page, reader)
  await page.goto(`/sources/${RECORD.objectType}/${RECORD.id}`)
  await page.getByRole("button", { name: actionTitle() }).click()
  before?.(cell)
  const form = page.getByRole("dialog")
  await form.getByLabel("Corrected total").fill(" 30.00 ")
  await form.getByLabel("Reason").fill("typo")
  await form.getByLabel("Evidence").fill(`${RECORD.id}~total`)
  await form.locator('button[type="submit"]').click()
  return { cell, form }
}

/** The request step's value for one label, exactly as it reads. */
const asked = async (panel: ReturnType<Page["locator"]>, label: string) =>
  panel.getByRole("region", { name: "The request" }).locator("dt", { hasText: new RegExp(`^${label}$`) }).locator("xpath=following-sibling::dd[1]").textContent()

test("disclosed: the request, each value the law compared with where it came from, the verdict, and who, when and why", async ({ page }) => {
  const { cell, form } = await refuse(page, "admin")
  const panel = form.locator('[data-refusal="disclosed"]')
  await expect(panel).toBeVisible()
  // The request: the action, its record and every value this form sent.
  const request = panel.getByRole("region", { name: "The request" })
  await expect(request).toContainText(actionTitle())
  await expect(request).toContainText(RECORD.id)
  await expect(request).toContainText("30.00")
  // Every member of the input as it was sent: the total trimmed as the cell decoded it, never as typed.
  expect(await asked(panel, "Corrected total")).toBe("30.00")
  expect(await asked(panel, "Reason")).toBe("typo")
  expect(await asked(panel, "Evidence")).toBe(`${RECORD.id}~total`)
  expect(await asked(panel, "Currency")).toBe("USD")
  // The check: the law's verdict and each value it compared.
  const check = panel.getByRole("region", { name: "The check" })
  await expect(check.locator('[data-verdict="fails"]')).toHaveText("Fails")
  await expect(check.locator('[data-checked="total"]')).toBeVisible()
  await expect(check.locator('[data-checked="lines.amount"]')).toBeVisible()
  // The refusal carries no provenance for the values it compared, so no value claims a source:
  // a record read now may not be what the law evaluated.
  await expect(check.locator("[data-checked-from], [data-source]")).toHaveCount(0)
  // The result: the verified caller as the cell named them, when the cell decided, and the law's sentence.
  const result = panel.getByRole("region", { name: "The result" })
  await expect(result).toContainText(principal("admin").subjectId)
  await expect(result).toContainText("Decided at")
  await expect(result.locator("time")).toBeVisible()
  await expect(result).toContainText("Nothing was written.")
  // The replay answered this call because its whole input is the one the cell's refusal was recorded for.
  expect(cell.calls).toEqual([REQUESTS.refusal])
  await shoot(page, "disclosed")
})

test("time label: a refusal without server decidedAt falls back to honestly labelled Received at", async ({ page }) => {
  await refusalCell(page, "admin")
  // Strip decidedAt to simulate an older server response that carries no refusal timestamp.
  const withoutTime = { ...retained("refusal.admin.json") }
  delete withoutTime.decidedAt
  await page.route(`**/api/host/v1/actions/${CORRECT}**`, (route) => {
    if (route.request().method() === "POST") return route.fulfill({ status: 422, json: withoutTime })
    return route.fallback()
  })
  await page.goto(`/sources/${RECORD.objectType}/${RECORD.id}`)
  await page.getByRole("button", { name: actionTitle() }).click()
  const form = page.getByRole("dialog")
  await form.getByLabel("Corrected total").fill("30.00")
  await form.getByLabel("Reason").fill("typo")
  await form.getByLabel("Evidence").fill(`${RECORD.id}~total`)
  await form.locator('button[type="submit"]').click()
  const result = form.locator('[data-refusal="disclosed"]').getByRole("region", { name: "The result" })
  await expect(result).toContainText("Received at")
  await expect(result).not.toContainText("Decided at")
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
  expect(cell.calls).toEqual([REQUESTS.refusal, REQUESTS.receipt])
  expect(value).toBe(REQUESTS.receipt.total)
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

test("redacted: after the grant is revoked, the panel reads nothing but the refusal, though the page still holds the law's finding and values", async ({ page }) => {
  // The admin's page has read the record, its finding (the law and the values it compared) and its
  // history; then the refusal-detail grant is revoked, and the cell answers the call redacted.
  const { cell, form } = await refuse(page, "admin", (replay) => replay.revokeDisclosure())
  const panel = form.locator('[data-refusal="redacted"]')
  await expect(panel).toBeVisible()
  const text = (await panel.textContent()) ?? ""
  expect(text).toContain("It does not say which law, or which values, to your role.")
  // Neither the law's sentence nor its name, nor any value it compares (each printed line, their sum), reaches the panel.
  for (const disclosed of ["sum of its lines", "total_matches_lines", "Open findings", "18.50", "12.00", "30.50"]) expect(text).not.toContain(disclosed)
  await expect(panel.locator("[data-checked], [data-verdict], [data-open-findings]")).toHaveCount(0)
  expect(cell.calls).toEqual([REQUESTS.refusal])
})
