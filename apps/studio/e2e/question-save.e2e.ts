import { createHash } from "node:crypto"
import { expect, test } from "@playwright/test"
import type { Page } from "@playwright/test"

const press = (page: Page, combo: string) => page.keyboard.press(`${process.platform === "darwin" ? "Meta" : "Control"}+${combo}`)
async function session(page: Page) {
  await page.route("**/api/me", (route) => route.fulfill({ json: { user: { id: "u_e2e", name: "E2E", email: "viewer@example.com" }, organization: { id: "org_demo", name: "Demo", role: "viewer" }, environmentId: "analytics" } }))
}

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
    conditions: [
      { id: "every", label: "Every record", detail: "all", recommended: true, value: { filter: [] } },
      { id: "gold", label: "metal is gold", detail: "only gold", recommended: false, value: { filter: [{ path: { property: "metal" }, operator: "eq", value: "gold" }] } },
    ],
    filterable: [], windows: [{ id: "cell", label: "This window", detail: "dates", recommended: true, value: { window } }],
  }
  let stored: { id: string; version: number; payload: unknown; retired: boolean; text: string; sha256: string } | undefined
  let unavailable = false
  let mismatch = false
  const firstWords = "The checked question."
  const filteredWords = "The filtered question."
  const digest = (value: string) => createHash("sha256").update(value).digest("hex")
  let returnedWords: string | undefined
  let currentWords = firstWords
  let expectedHash = digest(firstWords)
  const newWords = "The cell reads this question differently."
  const newHash = createHash("sha256").update(newWords).digest("hex")
  await page.route("**/api/questions/catalog", (route) => route.fulfill({ json: catalog }))
  await page.route("**/api/questions/recommendations", (route) => route.fulfill({ json: tree }))
  await page.route("**/api/questions/check", (route) => {
    const { question } = route.request().postDataJSON() as { question: { filter: unknown[] } }
    const filtered = question.filter.length > 0
    return route.fulfill({ json: { kind: "question", admitted: true, findings: [], plan: { grain: "week", question: {}, sums: [], metrics: [] }, restatement: { text: filtered ? "The filtered question." : "The checked question.", sha256: (filtered ? "b" : "a").repeat(64) } } })
  })
  await page.route("**/api/host/v1/definitions**", async (route) => {
    const url = new URL(route.request().url())
    expect(url.searchParams.get("orgId")).toBe("org_demo")
    expect(url.searchParams.get("environmentId")).toBe("analytics")
    if (url.pathname.endsWith(":list")) return route.fulfill({ json: { items: stored && !stored.retired ? [{ id: stored.id, title: "studio.yield_weekly", version: stored.version, lifecycle: "active" }] : [], cursor: null } })
    if (url.pathname.endsWith(":admit")) {
      const { payload } = route.request().postDataJSON() as { payload: { filter: unknown[] } }
      const text = payload.filter.length > 0 ? filteredWords : currentWords
      return route.fulfill({ json: { admitted: true, diagnostics: [], restatement: text, restatementSha256: digest(text) } })
    }
    if (url.pathname.endsWith(":install")) {
      const body = route.request().postDataJSON()
      expect(body.signoff.restatementSha256).toBe(expectedHash)
      expect(body.payload.name).toBe("studio.yield_weekly")
      if (unavailable) return route.fulfill({ status: 422, json: { code: "definition.signoff_unavailable", detail: "not available" } })
      if (mismatch) { mismatch = false; currentWords = newWords; expectedHash = newHash; return route.fulfill({ status: 422, json: { code: "definition.signoff_mismatch", restatement: newWords, restatementSha256: newHash } }) }
      stored = { id: body.id, version: (stored?.version ?? 0) + 1, payload: body.payload, retired: false, text: currentWords, sha256: body.signoff.restatementSha256 }
      return route.fulfill({ json: { installed: true, id: stored.id, version: stored.version } })
    }
    if (url.pathname.endsWith(":retire") && stored) {
      stored = { ...stored, version: stored.version + 1, retired: true }
      return route.fulfill({ json: { installed: true, id: stored.id, version: stored.version } })
    }
    if (stored) return route.fulfill({ json: { id: stored.id, kind: "question", version: stored.version, payload: stored.payload, restatement: returnedWords ?? stored.text, restatementSha256: digest(returnedWords ?? stored.text), signoff: { signer: "analyst@demo.norn.cloud", signedAtMs: 1794000000000, restatementSha256: stored.sha256 } } })
    return route.fulfill({ status: 404, json: { code: "definition.not_found" } })
  })
  await page.goto("/questions/build")
  await expect(page.getByLabel("Question name")).toHaveValue("Yield")
  await expect(page.getByRole("button", { name: "Save question" })).toBeDisabled()
  await expect(page.getByText("Sign off on the plain words first.")).toBeVisible()
  await expect(page.getByTestId("restatement")).toBeVisible()
  const order = await page.locator("main").innerText()
  expect(order.indexOf("Measure")).toBeLessThan(order.indexOf("In plain words"))
  expect(order.indexOf("This is what I asked for")).toBeLessThan(order.indexOf("Question name"))
  expect(order.indexOf("Save question")).toBeLessThan(order.indexOf("Run opens"))
  expect(order.indexOf("Run opens")).toBeLessThan(order.indexOf("Saved questions"))
  await page.getByRole("button", { name: "This is what I asked for" }).click()
  await page.getByLabel("Question name").fill("Yield weekly")
  await expect(page.getByText("Signed off: this is what you asked for")).toBeVisible()
  await expect(page.getByRole("button", { name: "Save question" })).toBeEnabled()
  // The first save signs the cell's admission words and succeeds without a mismatch.
  await page.getByRole("button", { name: "Save question" }).click()
  await expect(page.getByText("Saved · version 1")).toBeVisible()
  unavailable = true
  await page.getByRole("button", { name: "Save question" }).click()
  await expect(page.getByRole("alert")).toContainText("cannot verify sign-off yet")
  await expect(page.getByText("Saved · version 1")).toBeVisible()
  unavailable = false
  mismatch = true
  await page.getByRole("button", { name: "Save question" }).click()
  await expect(page.getByText("The server reads your question as:")).toBeVisible()
  await expect(page.getByTestId("restatement")).toHaveText(newWords)
  await expect(page.getByRole("button", { name: "Save question" })).toBeDisabled()
  await page.getByRole("button", { name: "This is what I asked for" }).click()
  await page.getByRole("button", { name: "Save question" }).click()
  await expect(page.getByText("Saved · version 2")).toBeVisible()
  await expect(page.getByText("Yield weekly · version 2")).toBeVisible()
  await expect(page.getByText("studio.yield_weekly", { exact: true })).toBeVisible()
  await page.getByRole("button", { name: "Open", exact: true }).click()
  await expect(page.getByTestId("restatement")).toHaveText(newWords)
  await expect(page.getByText(/Signed off by analyst@demo.norn.cloud on/)).toBeVisible()
  await expect(page.getByRole("button", { name: "Run", exact: true })).toBeEnabled()
  // A later cell restatement cannot inherit the old sign-off.
  returnedWords = currentWords = "The cell now reads the question differently."
  await page.getByRole("button", { name: "Open", exact: true }).click()
  await expect(page.getByTestId("restatement")).toHaveText(returnedWords)
  await expect(page.getByRole("button", { name: "This is what I asked for" })).toBeVisible()
  await expect(page.getByRole("button", { name: "Run", exact: true })).toHaveCount(0)
  returnedWords = undefined; currentWords = newWords
  await page.getByRole("button", { name: "Open", exact: true }).click()
  await expect(page.getByText(/Signed off by analyst@demo.norn.cloud on/)).toBeVisible()
  await page.getByLabel("Question name").fill("Changed")
  await expect(page.getByText(/Signed off by analyst@demo.norn.cloud on/)).toBeVisible()
  await expect(page.getByRole("button", { name: "Save question" })).toBeEnabled()
  await press(page, "z")
  await expect(page.getByLabel("Question name")).toHaveValue("yield weekly")
  await expect(page.getByText(/Signed off by analyst@demo.norn.cloud on/)).toBeVisible()
  await press(page, "Shift+z")
  await expect(page.getByLabel("Question name")).toHaveValue("Changed")
  await expect(page.getByText(/Signed off by analyst@demo.norn.cloud on/)).toBeVisible()
  await page.getByRole("button", { name: /metal is gold/ }).click()
  await expect(page.getByTestId("restatement")).toHaveText("The filtered question.")
  await expect(page.getByRole("button", { name: "This is what I asked for" })).toBeVisible()
  await expect(page.getByRole("button", { name: "Save question" })).toBeDisabled()
  await page.getByRole("button", { name: "Open", exact: true }).click()
  await expect(page.getByLabel("Question name")).toHaveValue("yield weekly")
  await expect(page.getByText(/Signed off by analyst@demo.norn.cloud on/)).toBeVisible()
  await page.getByRole("button", { name: "Remove question" }).click()
  await expect(page.getByRole("button", { name: "Undo removal" })).toBeVisible()
  await page.getByRole("button", { name: "Undo removal" }).click()
  await expect(page.getByText("Saved · version 4")).toBeVisible()
})


test("a workspace without a data cell cannot call definitions", async ({ page }) => {
  test.skip(Boolean(process.env.E2E_BASE), "this row uses a deterministic scope stand-in")
  await page.route("**/api/me", (route) => route.fulfill({ json: { user: { id: "u", name: "Member", email: "m@example.com" }, organization: { id: "unmapped", name: "Unmapped", role: "member" }, environmentId: null } }))
  const window = { start: "2026-07-06T00:00:00Z", end: "2026-10-26T00:00:00Z" }
  const catalog = { objectSet: "demo.Conversion", properties: [], metrics: [{ name: "demo.yield", label: "Yield", type: "decimal" }], grains: [], operators: [], example: { irVersion: "0.1", calendar: "utc", freshness: {} }, declarations: { metrics: [], objectType: null } }
  const choice = { id: "demo.yield", label: "Yield", detail: "yield", recommended: true, value: {} }
  const tree = { basis: "Catalogue", measures: [choice], groupBy: [], grains: {}, conditions: [], filterable: [], windows: [{ id: "cell", label: "This window", detail: "dates", recommended: true, value: { window } }] }
  let requests = 0
  await page.route("**/api/host/v1/definitions**", (route) => { requests += 1; return route.abort() })
  await page.route("**/api/questions/catalog", (route) => route.fulfill({ json: catalog }))
  await page.route("**/api/questions/recommendations", (route) => route.fulfill({ json: tree }))
  await page.route("**/api/questions/check", (route) => route.fulfill({ json: { kind: "question", admitted: true, findings: [], plan: { grain: "week", question: {}, sums: [], metrics: [] }, restatement: { text: "The checked question.", sha256: "a".repeat(64) } } }))
  await page.goto("/questions/build")
  await expect(page.getByText("This workspace has no data cell yet.")).toBeVisible()
  await expect(page.getByRole("button", { name: "This is what I asked for" })).toHaveCount(0)
  await expect(page.getByRole("button", { name: "Save question" })).toBeDisabled()
  expect(requests).toBe(0)
})
