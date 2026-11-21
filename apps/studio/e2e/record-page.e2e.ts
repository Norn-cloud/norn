import { expect, test, type Page } from "@playwright/test"
import { recordCell, VERIFY } from "./record-cell.ts"

// The record page, one test per row of its verb matrix: each value with the system it was read from,
// a marked value as the server projected it (and its reveal, with receipt, where the server allows),
// who may see or act by role as the cell decided, the rules the type keeps, and the last changes with
// who, when and why. SHOTS=<dir> saves the page in light and dark.

const PARTY = "/sources/canon.Party/demo/party/0001"
const shots = process.env.SHOTS

async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400)
    await page.screenshot({ path: `${shots}/studio-ui-record-${name}-${scheme}.png`, fullPage: true })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

const fact = (page: Page, id: string) => page.locator(`[data-fact="${id}"]`)
const role = (page: Page, name: string) => page.locator(`li[data-role="${name}"]`)
const verb = (page: Page, name: string, which: string) => role(page, name).locator(`[data-verb="${which}"]`)

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: replays recorded cell documents`) })
test.beforeEach(async ({ page }) => { await page.setViewportSize({ width: 1440, height: 1000 }) })

test("facts: each value carries the system it was read from, and the panel names every system", async ({ page }) => {
  await recordCell(page, "admin")
  await page.goto(PARTY)
  await expect(fact(page, "kind").locator('[data-source="ERPNext"]')).toBeVisible()
  await expect(page.getByLabel("Read from").locator('[data-source="ERPNext"]')).toBeVisible()
  await shoot(page, "party-admin")
})

test("facts: a role without Sources is told so, and no value claims a source", async ({ page }) => {
  await recordCell(page, "member")
  await page.goto(PARTY)
  await expect(fact(page, "kind")).toBeVisible()
  await expect(page.getByText("Your role does not see where these values came from.")).toBeVisible()
  await expect(page.locator("[data-fact] [data-source]")).toHaveCount(0)
})

test("markings: a marked value shows the server's projection, tagged hidden from your role, with no reveal", async ({ page }) => {
  await recordCell(page, "member")
  await page.goto(PARTY)
  const name = fact(page, "display_name")
  await expect(name.locator("[data-hidden-tag]")).toHaveText("Hidden from your role")
  await expect(name.locator('[data-masked="pseudonym"]')).toBeVisible()
  await expect(name.locator('[data-marking="pii.contact"]')).toBeVisible()
  await expect(name.locator("[data-reveal]")).toHaveCount(0)
  await shoot(page, "party-member")
})

test("markings: a role that may reveal reveals with a purpose, and the value comes back with its receipt", async ({ page }) => {
  const cell = await recordCell(page, "admin")
  await page.goto(PARTY)
  const name = fact(page, "display_name")
  await expect(name.locator("[data-hidden-tag]")).toHaveText("Masked · you may reveal")
  await name.locator("[data-reveal]").click()
  await page.getByLabel("Purpose").fill("KYC review for refund #4471")
  await page.locator("[data-confirm-reveal]").click()
  await expect(name.locator("[data-revealed]")).toContainText("Amira Haddad")
  await expect(name.locator("[data-revealed]")).toContainText("receipt #3")
  expect(cell.reveals).toEqual([expect.objectContaining({ property: "display_name", purpose: "KYC review for refund #4471" })])
})

/** Reveal the display name with a purpose, as an admin may. */
async function askReveal(page: Page) {
  const name = fact(page, "display_name")
  await name.locator("[data-reveal]").click()
  await page.getByLabel("Purpose").fill("KYC review for refund #4471")
  await page.locator("[data-confirm-reveal]").click()
  return name
}

test("markings: a revealed value is dropped when the cell's next read of the same version no longer allows a reveal", async ({ page }) => {
  const cell = await recordCell(page, "admin")
  await page.goto(PARTY)
  const name = await askReveal(page)
  await expect(name.locator("[data-revealed]")).toContainText("Amira Haddad")
  // The grant is revoked: the same version, now projected without a reveal.
  cell.become("member")
  await page.getByRole("button", { name: "Read again" }).click()
  await expect(name.locator("[data-hidden-tag]")).toHaveText("Hidden from your role")
  await expect(name.locator("[data-revealed]")).toHaveCount(0)
  await expect(name.locator('[data-masked="pseudonym"]')).toBeVisible()
  await expect(page.getByText("Amira Haddad")).toHaveCount(0)
})

test("markings: a reveal answered after the cell withdrew the reveal is never shown", async ({ page }) => {
  const cell = await recordCell(page, "admin")
  await page.goto(PARTY)
  const release = cell.holdReveal()
  const name = await askReveal(page)
  await expect.poll(() => cell.reveals.length).toBe(1)
  // While the answer is out, the cell's next read withdraws the reveal.
  cell.become("member")
  await page.keyboard.press("Escape")
  await page.getByRole("button", { name: "Read again" }).click()
  await expect(name.locator("[data-hidden-tag]")).toHaveText("Hidden from your role")
  // The late answer lands; the page waits past it and still shows only the projection.
  const answered = page.waitForResponse((response) => new URL(response.url()).pathname.endsWith("/reveal"))
  release()
  await answered
  await page.waitForTimeout(300)
  await expect(name.locator("[data-revealed]")).toHaveCount(0)
  await expect(page.getByText("Amira Haddad")).toHaveCount(0)
})

test("markings: a reveal asked under a projection, answered after it changed and changed back, is never shown", async ({ page }) => {
  const cell = await recordCell(page, "admin")
  await page.goto(PARTY)
  const release = cell.holdReveal()
  const name = await askReveal(page)
  await expect.poll(() => cell.reveals.length).toBe(1)
  // A → B: the cell's next read withdraws the reveal.
  cell.become("member")
  await page.keyboard.press("Escape")
  await page.getByRole("button", { name: "Read again" }).click()
  await expect(name.locator("[data-hidden-tag]")).toHaveText("Hidden from your role")
  // B → A: the read after allows it again, the same version and projection the reveal was asked under.
  cell.become("admin")
  await page.getByRole("button", { name: "Read again" }).click()
  await expect(name.locator("[data-hidden-tag]")).toHaveText("Masked · you may reveal")
  // The answer to the first ask lands: it belonged to the discarded instance, and is not shown.
  const answered = page.waitForResponse((response) => new URL(response.url()).pathname.endsWith("/reveal"))
  release()
  await answered
  await page.waitForTimeout(300)
  await expect(name.locator("[data-revealed]")).toHaveCount(0)
  await expect(page.getByText("Amira Haddad")).toHaveCount(0)
})

test("who can: each role's reads and actions are the cell's decisions, and your role is marked", async ({ page }) => {
  const cell = await recordCell(page, "member")
  await page.goto(PARTY)
  await expect(role(page, "member").getByText("You", { exact: true })).toBeVisible()
  await expect(role(page, "admin").getByText("You", { exact: true })).toHaveCount(0)
  const decided = async (name: string, which: string) => verb(page, name, which).getAttribute("data-decision")
  await expect(verb(page, "viewer", "see")).toHaveText("Names only")
  expect(await decided("viewer", "see")).toBe("partial")
  expect(await decided("member", "see")).toBe("permit")
  expect(await decided("member", "sources")).toBe("deny")
  expect(await decided("admin", "sources")).toBe("permit")
  // Every answer is the collection's, and says so: a marked fact is still projected per record.
  await expect(verb(page, "member", "see")).toHaveAttribute("title", "See: allowed for the collection")
  await expect(page.locator('[data-access-scope="collection"]')).toContainText("a marked fact can still be masked or withheld")
  const act = `act:${VERIFY.actionId}`
  await expect(verb(page, "admin", act)).toHaveText(VERIFY.title)
  expect([await decided("viewer", act), await decided("member", act), await decided("admin", act), await decided("owner", act)]).toEqual(["deny", "conditional", "permit", "permit"])
  // Asked of the cell for this type, never decided here.
  expect(cell.asked.some((search) => new URLSearchParams(search).get("objectType") === "canon.Party")).toBe(true)
})

test("who can: a refused access read shows the cell's refusal, not a guess", async ({ page }) => {
  await recordCell(page, "member", { status: 403, body: { code: "authz.denied", message: "Your role may not read this type's access." } })
  await page.goto(PARTY)
  await expect(page.getByText("Your role may not read this type's access.")).toBeVisible()
  await expect(page.locator("li[data-role]")).toHaveCount(0)
})

test("changes: the last changes say what wrote them, who, when and why; Every change opens the full history", async ({ page }) => {
  await recordCell(page, "admin")
  await page.goto(PARTY)
  const recent = page.getByRole("list", { name: "Recent changes" })
  const newest = recent.locator("li").first()
  await expect(newest).toContainText(VERIFY.title)
  await expect(newest).toContainText("user:sara")
  await expect(newest).toContainText("20 Nov 2026")
  await expect(newest).toContainText("changed kind")
  // The cell lists no call input and authorizes no reason field yet: no "why" is shown.
  await expect(recent).not.toContainText("Why")
  await expect(recent.locator("li").nth(1)).toContainText("Imported from ERPNext")
  await page.getByRole("button", { name: "Every change" }).click()
  await expect(page.locator("#record-history")).toBeInViewport()
})

test("changes: a member (no Sources, no call records) sees the history list in both panels", async ({ page }) => {
  await recordCell(page, "member")
  await page.goto(PARTY)
  const recent = page.getByRole("list", { name: "Recent changes" })
  await expect(recent.locator("li")).toHaveCount(2)
  await expect(recent.locator("li").first()).toContainText("changed kind")
  await expect(recent).not.toContainText("user:sara")
  await expect(recent).not.toContainText("Imported from")
  const history = page.locator("#record-history")
  await expect(history.locator("ol > li")).toHaveCount(2)
  // The header names the version the page shows, and the commit and time that wrote it, from the same cell.
  const chip = (label: string) => page.locator("section > header").first().locator('[data-slot="fact-chip"]', { hasText: label }).locator("span").last()
  await expect(chip("version")).toHaveText("2")
  await expect(chip("commit")).toHaveText("412")
  await expect(chip("as of")).toContainText("20 Nov")
  // A writer the cell does not name is said in short, with the reason in its tooltip.
  const hidden = history.locator("[data-hidden-writer]").first()
  await expect(hidden).toContainText("Change not shown to you")
  await expect(hidden).toHaveAttribute("title", "Written by a rule, an import, or a call not shown to you")
  await expect(page.getByText("Written by a rule, an import, or a call not shown to you", { exact: true })).toHaveCount(0)
  await expect(page.locator("[data-history-unavailable]")).toHaveCount(0)
  await expect(page.getByText("No such readable object.")).toHaveCount(0)
  await expect(page.getByText("The history could not be read.")).toHaveCount(0)
  await shoot(page, "party-member-history")
})

test("changes: a refused history reads the same plain state in both panels, never the refusal's text", async ({ page }) => {
  const cell = await recordCell(page, "member")
  cell.refuseHistory()
  await page.goto(PARTY)
  await expect(page.locator("[data-history-unavailable]")).toHaveCount(2)
  for (const panel of await page.locator("[data-history-unavailable]").all()) await expect(panel).toHaveText("History unavailable")
  await expect(page.getByText("No such readable object.")).toHaveCount(0)
})

test("rules: the rail lists the rules the type keeps, and All rules opens the rules page", async ({ page }) => {
  await recordCell(page, "admin")
  await page.goto("/sources/canon.Holding/demo/holding/owner/north/gold")
  await expect(page.getByRole("heading", { name: "Rules it keeps" })).toBeVisible()
  await shoot(page, "holding-admin")
  await page.getByRole("link", { name: "All rules" }).click()
  await expect(page).toHaveURL(/\/guarantees$/)
})
