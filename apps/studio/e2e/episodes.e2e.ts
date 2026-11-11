import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

// One test per state of the episode timeline (#1220 L-1). The object route is stubbed with the
// contract-checked fixture (test/learning-episode.test.ts checks it against the served envelope and
// the decoder), so these skip on a stage until the learning pack serves learning.Episode.
// SHOTS=<dir> saves every state in light and dark for review.

const fixture = JSON.parse(readFileSync(new URL("../test/fixtures/learning-episode.json", import.meta.url), "utf8")) as { readonly records: readonly { readonly id: string; readonly values: unknown }[] }
const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400)
    await page.screenshot({ path: `${shots}/episode-${name}-${scheme}.png`, fullPage: true })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the episode route stand-in`) })

type Serve = "fixture" | "unserved" | "malformed"
async function open(page: Page, id: string, serve: Serve = "fixture") {
  await session(page)
  await page.route("**/api/host/v1/objects/learning.Episode/**", (route) => {
    if (serve === "unserved") return route.fulfill({ status: 404, body: "not found" })
    const asked = decodeURIComponent(new URL(route.request().url()).pathname.split("/").pop() ?? "")
    const record = fixture.records.find((r) => r.id === asked)
    if (record === undefined) return route.fulfill({ status: 404, json: { code: "object.not_found", message: "No object with that id is readable here." } })
    return route.fulfill({ json: serve === "malformed" ? { ...record, values: { ...record.values as object, agreement: "accepted" } } : record })
  })
  await page.goto(`/episodes/${id}`)
}
const band = (page: Page, title: string) => page.getByRole("region", { name: title })

test("view: an overridden suggestion reads as the strongest signal, with the blocked rule's waiver and the recorded outcome", async ({ page }) => {
  await open(page, "ep_demo_overridden")
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Release the held shipment?")
  await expect(page.getByText("Release a credit hold · Ask manager")).toBeVisible()
  await expect(page.getByText("Measured", { exact: true })).toBeVisible()
  await expect(band(page, "What was known").getByRole("link")).toHaveText(["North branch customer", "Delivery 0042"])
  await expect(band(page, "What was suggested")).toContainText("Suggested Keep holding")
  await expect(band(page, "What was suggested")).toContainText("Active")
  await expect(band(page, "What was decided")).toContainText("Overrode the suggestion")
  await expect(band(page, "What was decided")).toContainText("Release it · a manager")
  await expect(band(page, "What was decided")).toContainText("Customer paid the overdue invoice by phone")
  const rules = band(page, "Rules that applied").getByRole("listitem")
  await expect(rules).toHaveCount(2)
  await expect(rules.nth(0)).toContainText("Blocked")
  await expect(rules.nth(0)).toContainText("Waived by a manager: Payment confirmed by phone")
  await expect(rules.nth(1)).toContainText("Allowed")
  await expect(band(page, "What happened")).toContainText("Recorded")
  await expect(band(page, "What happened")).toContainText("Invoice paid · within 14 days")
  await expect(band(page, "What happened")).toContainText("Days to pay2")
  const receipts = page.getByRole("complementary", { name: "Receipts" })
  await expect(receipts).toContainText("Not triaged yet")
  for (const label of ["Decision", "Suggestion", "Outcome", "Waiver · Hold shipments over the credit limit"]) await expect(receipts.getByText(label, { exact: true })).toBeVisible()
  await expect(receipts.getByRole("link", { name: "Open the run" })).toHaveAttribute("href", "/runs/run_restock_7")
  await shoot(page, "overridden")
})

test("view: technical shows the pins, generation and law ids; business keeps them out of the way", async ({ page }) => {
  await open(page, "ep_demo_overridden")
  await expect(page.getByText("demo.credit.hold_over_limit")).toHaveCount(0)
  await page.getByRole("radio", { name: "Technical" }).click()
  await expect(page.getByText("demo.credit.hold_over_limit")).toBeVisible()
  await expect(band(page, "What was known")).toContainText("generation 12 · as of commit 4806")
  await expect(page.getByRole("complementary", { name: "Receipts" })).toContainText("Decision · commit 4812")
  await shoot(page, "technical")
  await page.getByRole("radio", { name: "Business" }).click()
})

test("view: an accepted suggestion whose outcome is still open stays context only", async ({ page }) => {
  await open(page, "ep_demo_accepted")
  await expect(page.getByText("Context only", { exact: true })).toBeVisible()
  await expect(page.getByText("it can't become a curated case or a score")).toBeVisible()
  await expect(band(page, "What was suggested")).toContainText("Shadow")
  await expect(band(page, "What was decided")).toContainText("Accepted the suggestion")
  await expect(band(page, "What happened")).toContainText("Not measured yet")
  await expect(band(page, "What happened")).toContainText("No outcome has been recorded yet.")
  await expect(page.getByRole("complementary", { name: "Receipts" }).getByText("Outcome", { exact: true })).toHaveCount(0)
  await shoot(page, "open")
})

test("view: no suggestion, an outcome not measured in time, and an exception verdict from triage", async ({ page }) => {
  await open(page, "ep_demo_unsuggested")
  await expect(band(page, "What was known")).toContainText("No records were named")
  await expect(band(page, "What was suggested")).toContainText("Nothing was suggested")
  await expect(band(page, "What was decided")).toContainText("No suggestion")
  await expect(band(page, "Rules that applied")).toContainText("Would have blocked")
  await expect(band(page, "What happened")).toContainText("Not measured in time")
  const receipts = page.getByRole("complementary", { name: "Receipts" })
  await expect(receipts).toContainText("Exception")
  await expect(receipts).toContainText("Long-standing supplier")
  await expect(receipts.getByText("Suggestion", { exact: true })).toHaveCount(0)
  await shoot(page, "overdue")
})

test("while the cell does not serve episodes, the page says so and guesses nothing", async ({ page }) => {
  await open(page, "ep_demo_overridden", "unserved")
  await expect(page.getByText("This cell doesn't serve decision episodes yet.")).toBeVisible()
  await shoot(page, "unserved")
})

test("an episode this reader can't read reads as absent, with the cell's words", async ({ page }) => {
  await open(page, "ep_missing")
  await expect(page.getByText("No such episode")).toBeVisible()
  await expect(page.getByText("No object with that id is readable here.")).toBeVisible()
})

test("an episode that contradicts itself is refused, not drawn", async ({ page }) => {
  await open(page, "ep_demo_overridden", "malformed")
  await expect(page.getByText("Can't show this episode")).toBeVisible()
  await expect(page.getByText("/agreement is accepted but the choice is not the suggestion")).toBeVisible()
  await expect(page.getByRole("region", { name: "What was decided" })).toHaveCount(0)
  await shoot(page, "malformed")
})
