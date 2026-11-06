import { createHash } from "node:crypto"
import { expect, test } from "@playwright/test"
import { press, session } from "./canvas.ts"

test("question row: create, validate, sign, save, reopen, edit, undo, redo, remove and undo removal", async ({ page }) => {
  test.skip(Boolean(process.env.E2E_BASE), "this row uses a deterministic definitions cell stand-in")
  await session(page)
  page.on("dialog", (dialog) => void dialog.accept())
  const window = { start: "2026-07-06T00:00:00Z", end: "2026-10-26T00:00:00Z" }
  const catalog = { objectSet: "demo.Conversion", properties: [{ name: "occurred_at", kind: "timestamp", unit: null }], metrics: [{ name: "demo.yield", label: "Yield", type: "decimal" }], grains: ["week"], operators: ["eq"], example: { irVersion: "0.1", calendar: "utc", freshness: {} }, declarations: { metrics: [], objectType: null } }
  const tree = {
    basis: "From the catalogue", measures: [{ id: "demo.yield", label: "Yield", detail: "yield", recommended: true, value: {} }],
    groupBy: [{ id: "occurred_at", label: "occurred at", detail: "time", recommended: true, value: {} }],
    grains: { occurred_at: [{ id: "week", label: "Week", detail: "weekly", recommended: true, value: {} }] },
    conditions: [{ id: "every", label: "Every record", detail: "all", recommended: true, value: { filter: [] } }],
    filterable: [], windows: [{ id: "cell", label: "This window", detail: "dates", recommended: true, value: { window } }],
  }
  let stored: { id: string; version: number; payload: unknown; retired: boolean } | undefined
  let unavailable = true
  let mismatch = false
  let expectedHash = "a".repeat(64)
  const newWords = "The cell reads this question differently."
  const newHash = createHash("sha256").update(newWords).digest("hex")
  await page.route("**/api/questions/catalog", (route) => route.fulfill({ json: catalog }))
  await page.route("**/api/questions/recommendations", (route) => route.fulfill({ json: tree }))
  await page.route("**/api/questions/check", (route) => route.fulfill({ json: { kind: "question", admitted: true, findings: [], plan: null, restatement: { text: "The checked question.", sha256: "a".repeat(64) } } }))
  await page.route("**/api/host/v1/definitions**", async (route) => {
    const url = new URL(route.request().url())
    expect(url.searchParams.get("orgId")).toBe("org_demo")
    expect(url.searchParams.get("environmentId")).toBe("analytics")
    if (url.pathname.endsWith(":list")) return route.fulfill({ json: { items: stored && !stored.retired ? [{ id: stored.id, title: "Yield", version: stored.version, lifecycle: "active" }] : [], cursor: null } })
    if (url.pathname.endsWith(":install")) {
      const body = route.request().postDataJSON()
      expect(body.signoff.restatementSha256).toBe(expectedHash)
      if (unavailable) return route.fulfill({ status: 422, json: { code: "definition.signoff_unavailable", detail: "not available" } })
      if (mismatch) { mismatch = false; expectedHash = newHash; return route.fulfill({ status: 422, json: { code: "definition.signoff_mismatch", restatement: newWords, restatementSha256: newHash } }) }
      stored = { id: body.id, version: (stored?.version ?? 0) + 1, payload: body.payload, retired: false }
      return route.fulfill({ json: { installed: true, id: stored.id, version: stored.version } })
    }
    if (url.pathname.endsWith(":retire") && stored) {
      stored = { ...stored, version: stored.version + 1, retired: true }
      return route.fulfill({ json: { installed: true, id: stored.id, version: stored.version } })
    }
    if (stored) return route.fulfill({ json: { id: stored.id, kind: "question", version: stored.version, payload: stored.payload, signoff: { restatementSha256: "a".repeat(64) } } })
    return route.fulfill({ status: 404, json: { code: "definition.not_found" } })
  })
  await page.goto("/questions/build")
  await page.getByLabel("Question name").fill("Yield")
  await page.getByRole("button", { name: "This is what I asked for" }).click()
  await page.getByRole("button", { name: "Save question" }).click()
  await expect(page.getByRole("alert")).toContainText("cannot verify sign-off yet")
  await expect(page.getByText(/Saved · version/)).toHaveCount(0)
  unavailable = false
  mismatch = true
  await page.getByRole("button", { name: "Save question" }).click()
  await expect(page.getByText("The server reads your question as:")).toBeVisible()
  await expect(page.getByTestId("restatement")).toHaveText(newWords)
  await expect(page.getByRole("button", { name: "Save question" })).toBeDisabled()
  await page.getByRole("button", { name: "This is what I asked for" }).click()
  await page.getByRole("button", { name: "Save question" }).click()
  await expect(page.getByText("Saved · version 1")).toBeVisible()
  expectedHash = "a".repeat(64)
  await page.getByLabel("Question name").fill("Changed")
  await expect(page.getByRole("button", { name: "Save question" })).toBeDisabled()
  await press(page, "z")
  await expect(page.getByLabel("Question name")).toHaveValue("Yield")
  await expect(page.getByRole("button", { name: "Save question" })).toBeDisabled()
  await press(page, "Shift+z")
  await expect(page.getByLabel("Question name")).toHaveValue("Changed")
  await page.getByRole("button", { name: "Open", exact: true }).click()
  await expect(page.getByLabel("Question name")).toHaveValue("yield")
  await page.getByRole("button", { name: "This is what I asked for" }).click()
  await page.getByRole("button", { name: "Remove question" }).click()
  await expect(page.getByRole("button", { name: "Undo removal" })).toBeVisible()
  await page.getByRole("button", { name: "Undo removal" }).click()
  await expect(page.getByText("Saved · version 3")).toBeVisible()
})
