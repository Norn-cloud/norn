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
  // #1520: pseudonym chip and its projection hint never truncate on desktop or narrow viewports.
  for (const width of [1440, 768]) {
    await page.setViewportSize({ width, height: 1000 })
    const overflow = await name.evaluate((row) => {
      const checks = Array.from(row.querySelectorAll<HTMLElement>('[data-masked="pseudonym"], [data-mask-hint]'))
      return checks.map((el) => ({ text: el.textContent ?? "", scrollW: el.scrollWidth, clientW: el.clientWidth }))
    })
    expect(overflow.length).toBeGreaterThanOrEqual(2)
    for (const item of overflow) {
      expect(item.scrollW, `truncated at ${width}px: ${item.text}`).toBeLessThanOrEqual(item.clientW + 1)
    }
  }
  await page.setViewportSize({ width: 1440, height: 1000 })
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
  await revealSettled(page, release)
  await expect(name.locator("[data-revealed]")).toHaveCount(0)
  await expect(page.getByText("Amira Haddad")).toHaveCount(0)
})

/**
 * Release a held reveal and wait until the page has read its answer to the end and painted twice
 * since: whatever the page does with a late answer, it has done by then.
 */
async function revealSettled(page: Page, release: () => void) {
  const finished = page.waitForEvent("requestfinished", (request) => new URL(request.url()).pathname.endsWith("/reveal"))
  release()
  await finished
  await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))))
}

const refocus = (page: Page) => page.evaluate(() => {
  for (const visibility of ["hidden", "visible"]) {
    Object.defineProperty(document, "visibilityState", { configurable: true, value: visibility })
    window.dispatchEvent(new Event("visibilitychange"))
  }
})

// #1700 R3: the record page's read and its reveals go to the workspace the record was read from.
test("markings: a record-page reveal is sent to the workspace the record was read from", async ({ page }) => {
  const cell = await recordCell(page, "admin")
  await page.goto(PARTY)
  const name = await askReveal(page)
  await expect(name.locator("[data-revealed]")).toContainText("Amira Haddad")
  expect(new Set(cell.readScopes)).toEqual(new Set(["org_demo/analytics"]))
  expect(cell.revealScopes).toEqual([{ at: "org_demo/analytics", expected: "org_demo/analytics" }])
})

// #1700 R3: a reveal still out when the session moves to another workspace is never shown; the page reads the record there.
test("markings: a reveal answered after a workspace switch is never shown, and the record is read in the new workspace", async ({ page }) => {
  const cell = await recordCell(page, "admin")
  let environmentId = "analytics"
  await page.route("**/api/me", (route) => route.fulfill({ json: { user: { id: "u_e2e", name: "E2E", email: "viewer@example.com" }, organization: { id: "org_demo", name: "Demo", role: "viewer" }, environmentId } }))
  await page.goto(PARTY)
  const release = cell.holdReveal()
  const name = await askReveal(page)
  await expect.poll(() => cell.reveals.length).toBe(1)
  environmentId = "staging"
  await refocus(page)
  await expect.poll(() => cell.readScopes.includes("org_demo/staging")).toBe(true)
  await expect(name.locator("[data-hidden-tag]")).toHaveText("Masked · you may reveal")
  await revealSettled(page, release)
  await expect(name.locator("[data-revealed]")).toHaveCount(0)
  await expect(page.getByText("Amira Haddad")).toHaveCount(0)
  expect(cell.revealScopes).toEqual([{ at: "org_demo/analytics", expected: "org_demo/analytics" }])
})

// #1720 R3: a record page whose mode read failed reads nothing until the mode is decided; its Retry
// decides the mode again, and the record is then read and revealed at the workspace it came from.
test("markings: after a failed mode read, the page's Retry reads and reveals the record at its source workspace", async ({ page }) => {
  const cell = await recordCell(page, "admin")
  let down = true
  await page.route("**/api/host/v1/definitions**", (route) => down
    ? route.fulfill({ status: 503, json: { code: "cell.unopenable", message: "The workspace cell could not be opened.", retryable: false } })
    : route.fallback())
  await page.goto(PARTY)
  const unavailable = page.locator("[data-workspace-unavailable]")
  await expect(unavailable).toBeVisible()
  await shoot(page, "party-mode-unavailable")
  // Nothing was read while no mode was decided: no read without a workspace.
  expect(cell.readScopes).toEqual([])
  // The cell recovers; the page's own Retry decides the mode and then reads the record at its scope.
  down = false
  await unavailable.getByRole("button", { name: "Retry" }).click()
  const name = fact(page, "display_name")
  await expect(name.locator("[data-hidden-tag]")).toHaveText("Masked · you may reveal")
  await expect(page.locator('[data-connection="unavailable"]')).toHaveCount(0)
  await expect(page.locator("#record-history")).toBeVisible()
  await askReveal(page)
  await expect(name.locator("[data-revealed]")).toContainText("Amira Haddad")
  expect(new Set(cell.readScopes)).toEqual(new Set(["org_demo/analytics"]))
  expect(cell.revealScopes).toEqual([{ at: "org_demo/analytics", expected: "org_demo/analytics" }])
})

// #1740: the type page's grid drawer once read a live row through the mode-deciding transport and cached
// it under the key the record page read while its mode was undecided, so the full record could show it
// with no workspace. First load during a cell outage, the cell recovers, the type page's Retry, a drawer,
// then "Open full record" just as the session's workspace read fails: no live record shows and no reveal
// fires until the mode is decided again, and the reveal then goes to the workspace it was read from.
test("markings: a drawer's record opened in full while the workspace read fails shows and reveals nothing until the mode is decided", async ({ page }) => {
  const cell = await recordCell(page, "admin")
  let cellDown = true
  let sessionDown = false
  await page.route("**/api/host/v1/definitions**", (route) => cellDown
    ? route.fulfill({ status: 503, json: { code: "cell.unopenable", message: "The workspace cell could not be opened.", retryable: false } })
    : route.fallback())
  await page.route("**/api/me", (route) => sessionDown
    ? route.fulfill({ status: 503, json: { error: "The workspace host is unavailable.", retryable: false } })
    : route.fallback())
  await page.goto("/sources/canon.Party")
  const typeUnavailable = page.locator("[data-workspace-unavailable]")
  await expect(typeUnavailable).toBeVisible()
  // The cell recovers; the type page's own Retry reads the catalog again, and its grid shows the live records.
  cellDown = false
  await typeUnavailable.getByRole("button", { name: "Retry" }).click()
  const grid = page.getByRole("grid")
  await expect(grid.locator('[role="row"][aria-rowindex="2"]')).toBeVisible()
  await grid.focus()
  await page.keyboard.press("Enter")
  const drawer = page.getByRole("dialog")
  await expect(drawer.getByRole("heading", { name: "Linked records" })).toBeVisible()
  await expect.poll(() => cell.readScopes).toEqual(["org_demo/analytics"])
  // The session's workspace read fails as the full record opens: the page's mode is undecided.
  sessionDown = true
  await drawer.getByRole("link", { name: "Open full record" }).click()
  await expect(page).toHaveURL(/\/sources\/canon\.Party\/demo\/party\/0001$/)
  await expect(page.locator('[data-connection="unavailable"]')).toBeVisible()
  // Nothing the drawer read shows here without a workspace, and nothing can be revealed: only the page's
  // Workspace unavailable state, with its Retry.
  await expect(page.locator("[data-fact]")).toHaveCount(0)
  await expect(page.locator("[data-reveal]")).toHaveCount(0)
  const unavailable = page.locator("[data-workspace-unavailable]")
  await expect(unavailable).toBeVisible()
  await shoot(page, "party-full-record-mode-unavailable")
  expect(cell.reveals).toEqual([])
  // The session recovers; the page's Retry decides the mode, and the record is read and revealed at its workspace.
  sessionDown = false
  await unavailable.getByRole("button", { name: "Retry" }).click()
  const name = fact(page, "display_name")
  await expect(name.locator("[data-hidden-tag]")).toHaveText("Masked · you may reveal")
  await askReveal(page)
  await expect(name.locator("[data-revealed]")).toContainText("Amira Haddad")
  expect(new Set(cell.readScopes)).toEqual(new Set(["org_demo/analytics"]))
  expect(cell.revealScopes).toEqual([{ at: "org_demo/analytics", expected: "org_demo/analytics" }])
})

const SESSION_DOWN = { status: 503, json: { error: "The workspace host is unavailable.", retryable: false } }
const scopeOf = (url: URL) => `${url.searchParams.get("orgId")}/${url.searchParams.get("environmentId")}`

// #1745: the record page read a record's findings under one key for every workspace and showed its
// Held back and rules-broken chips whatever its mode, so findings read before the session's workspace
// read failed stayed beside Workspace unavailable. They show only under a decided mode, read at its workspace.
test("findings: a held record's chips leave with the mode when the workspace read fails, and come back read at its workspace", async ({ page }) => {
  await recordCell(page, "admin")
  const findingsScopes: string[] = []
  let sessionDown = false
  await page.route((url) => url.pathname.endsWith("/findings"), (route) => {
    const url = new URL(route.request().url())
    findingsScopes.push(scopeOf(url))
    return route.fulfill({ json: { objectType: "canon.Party", id: "demo/party/0001", quarantined: true, evidence: [],
      findings: [{ id: "f-1", ruleRef: { kind: "builtin", id: "core.required", version: "1" }, severity: "error", message: "A required value is missing.", status: "open", firstSeen: "2026-11-18T09:12:00Z", lastSeen: "2026-11-18T09:12:00Z" }] } })
  })
  await page.route("**/api/me", (route) => sessionDown ? route.fulfill(SESSION_DOWN) : route.fallback())
  await page.goto(PARTY)
  const chips = page.locator("div:has(> h1)")
  await expect(chips.getByText("Held back", { exact: true })).toBeVisible()
  await expect(chips.getByText("1 rule broken", { exact: true })).toBeVisible()
  // The session's workspace read fails: the page's mode is undecided, and nothing it read shows as current.
  sessionDown = true
  await refocus(page)
  const unavailable = page.locator("[data-workspace-unavailable]")
  await expect(unavailable).toBeVisible()
  await expect(chips.getByText("Held back", { exact: true })).toHaveCount(0)
  await expect(chips.getByText("1 rule broken", { exact: true })).toHaveCount(0)
  await expect(page.getByText("Held out of authoritative reads")).toHaveCount(0)
  await shoot(page, "party-findings-mode-unavailable")
  // The session recovers; the page's Retry decides the mode, and the chips come back, read at the record's workspace.
  sessionDown = false
  await unavailable.getByRole("button", { name: "Retry" }).click()
  await expect(chips.getByText("Held back", { exact: true })).toBeVisible()
  await expect(chips.getByText("1 rule broken", { exact: true })).toBeVisible()
  expect(new Set(findingsScopes)).toEqual(new Set(["org_demo/analytics"]))
})

// #1745: the type page showed its grid whenever the mode was not pending, so after a failed mode read it
// mounted a grid with no workspace, which reads the records at whatever workspace the session reports when
// it answers: rows the page's mode never decided, with Reveal pinned to the session. The session's
// workspace read fails while the type page is open; its definition stays, and its records wait for the mode.
test("records: a type page whose mode read fails shows Workspace unavailable for its records, never a grid without a workspace", async ({ page }) => {
  await recordCell(page, "admin")
  const pageReads: string[] = []
  page.on("request", (request) => {
    const url = new URL(request.url())
    if (url.pathname.endsWith("/v1/objects/canon.Party")) pageReads.push(scopeOf(url))
  })
  let sessionDown = false
  await page.route("**/api/me", (route) => sessionDown ? route.fulfill(SESSION_DOWN) : route.fallback())
  await page.goto("/sources/canon.Party")
  const grid = page.getByRole("grid")
  await expect(grid.locator('[role="row"][aria-rowindex="2"]')).toBeVisible()
  expect(new Set(pageReads)).toEqual(new Set(["org_demo/analytics"]))
  // The session's workspace read fails: the page's mode is undecided. The type's definition stays; its
  // records show Workspace unavailable with the mode's Retry, and no grid is mounted to read them.
  const readsBefore = pageReads.length
  sessionDown = true
  await refocus(page)
  const records = page.getByRole("region", { name: "Records" })
  const unavailable = records.locator("[data-workspace-unavailable]")
  await expect(unavailable).toBeVisible()
  await expect(grid).toHaveCount(0)
  await expect(records.getByText("not served by this cell")).toHaveCount(0)
  await expect(page.locator("[data-reveal]")).toHaveCount(0)
  expect(pageReads).toHaveLength(readsBefore)
  await shoot(page, "party-type-records-mode-unavailable")
  // The session recovers; the Records section's Retry decides the mode, and the grid reads at that workspace.
  sessionDown = false
  await unavailable.getByRole("button", { name: "Retry" }).click()
  await expect(grid.locator('[role="row"][aria-rowindex="2"]')).toBeVisible()
  expect(new Set(pageReads)).toEqual(new Set(["org_demo/analytics"]))
})

// #1754 (replaces #1749's pin of the drawer's "links show once its workspace is confirmed"): a cell that
// serves records but no definitions decides the recording, and a decided recorded mode reads only the
// recording. The grid once read the session's live rows here, cached them with no workspace, and pinned
// Reveal to none. Both type pages now show the recorded rows; no live row is read, cached or revealed, and
// a row's drawer reads the recording's links.
test("records: a decided recorded mode reads only the recording, even from a cell that serves records", async ({ page }) => {
  const cell = await recordCell(page, "admin")
  // Every read of the type's records, one record, or the actions its rows offer.
  const liveReads: string[] = []
  page.on("request", (request) => {
    const url = new URL(request.url())
    if (url.pathname.startsWith("/api/host/v1/objects") || (url.pathname === "/api/host/v1/actions" && url.searchParams.get("objectKind") === "canon.Party")) liveReads.push(`${url.pathname}${url.search}`)
  })
  await page.route("**/api/host/v1/definitions**", (route) => route.fulfill({ status: 404, json: { code: "route.not_found", message: "No such route." } }))
  for (const path of ["/sources/canon.Party", "/objects/canon.Party"]) {
    await page.goto(path)
    const records = page.getByRole("region", { name: "Records" })
    const grid = records.getByRole("grid")
    await expect(grid.getByText("Custodian", { exact: true }).first()).toBeVisible()
    await expect(records.getByText("Recorded", { exact: true })).toBeVisible()
    await expect(grid.getByText(/Customer #/)).toHaveCount(0)
    await grid.focus()
    await page.keyboard.press("Enter")
    const drawer = page.getByRole("dialog")
    await expect(drawer.getByRole("heading", { name: "Linked records" })).toBeVisible()
    await expect(drawer.getByRole("link", { name: "demo/holding/owner/north/gold" })).toBeVisible()
    await expect(drawer.getByText(/once its workspace is confirmed/)).toHaveCount(0)
    await expect(drawer.locator("[data-reveal]")).toHaveCount(0)
    await expect(drawer.getByText("Recorded records take no actions.")).toBeVisible()
    if (path === "/objects/canon.Party") await shoot(page, "party-recorded-mode-drawer")
    await page.keyboard.press("Escape")
  }
  expect(liveReads).toEqual([])
  expect(cell.readScopes).toEqual([])
  expect(cell.reveals).toEqual([])
})

// #1754: `/objects/$type` bound its records, profile and quarantine to the session's scope, so after a failed
// mode read it kept the cached live rows beside the shell's Workspace unavailable. It binds the workspace
// the mode decided, as `/sources/$type` does: its Records section shows Workspace unavailable with the
// mode's Retry, no tab bound to the session is left, and nothing is read until Retry decides the mode.
test("records: /objects/$type whose mode read fails shows Workspace unavailable for its records, never the session's cached rows", async ({ page }) => {
  await recordCell(page, "admin")
  const pageReads: string[] = []
  page.on("request", (request) => {
    const url = new URL(request.url())
    if (url.pathname.endsWith("/v1/objects/canon.Party")) pageReads.push(scopeOf(url))
  })
  let sessionDown = false
  await page.route("**/api/me", (route) => sessionDown ? route.fulfill(SESSION_DOWN) : route.fallback())
  await page.goto("/objects/canon.Party")
  const records = page.getByRole("region", { name: "Records" })
  const grid = page.getByRole("grid")
  await expect(grid.locator('[role="row"][aria-rowindex="2"]')).toBeVisible()
  await expect(grid.getByText(/Customer #/).first()).toBeVisible()
  await expect(page.getByRole("tab", { name: "Profile" })).toBeVisible()
  expect(new Set(pageReads)).toEqual(new Set(["org_demo/analytics"]))
  const readsBefore = pageReads.length
  sessionDown = true
  await refocus(page)
  const unavailable = records.locator("[data-workspace-unavailable]")
  await expect(unavailable).toBeVisible()
  await expect(grid).toHaveCount(0)
  await expect(page.getByText(/Customer #/)).toHaveCount(0)
  await expect(page.getByRole("tab", { name: "Profile" })).toHaveCount(0)
  await expect(page.locator("[data-reveal]")).toHaveCount(0)
  expect(pageReads).toHaveLength(readsBefore)
  await shoot(page, "party-objects-records-mode-unavailable")
  // The session recovers; the Records section's Retry decides the mode, and the tabs read at that workspace.
  sessionDown = false
  await unavailable.getByRole("button", { name: "Retry" }).click()
  await expect(grid.locator('[role="row"][aria-rowindex="2"]')).toBeVisible()
  await expect(page.getByRole("tab", { name: "Profile" })).toBeVisible()
  expect(new Set(pageReads)).toEqual(new Set(["org_demo/analytics"]))
})

// #1754 item 4: each findings read is three requests (`/api/me` before and after the findings route). The
// record page's header and its Findings panel share one read; the panel mounting again (the Overview tab
// shown again) does not read it again while it is fresh. Read again still reads it.
test("findings: the record page reads a record's findings once, and showing the Overview again reads nothing", async ({ page }) => {
  await recordCell(page, "admin")
  const findingsReads: string[] = []
  page.on("request", (request) => {
    if (new URL(request.url()).pathname.endsWith("/findings")) findingsReads.push(request.url())
  })
  await page.goto(PARTY)
  await expect(page.getByText("No findings", { exact: true })).toBeVisible()
  await expect.poll(() => findingsReads.length).toBe(1)
  const onLoad = findingsReads.length
  for (const _ of [1, 2]) {
    await page.getByRole("tab", { name: "Lineage" }).click()
    await page.getByRole("tab", { name: "Overview" }).click()
    await expect(page.getByText("No findings", { exact: true })).toBeVisible()
  }
  const afterRemounts = findingsReads.length
  // Read again reads the findings again: its read is the signal that the page has sent every read it was
  // going to, since a remount's read would have gone out before it.
  await Promise.all([
    page.waitForResponse((response) => new URL(response.url()).pathname.endsWith("/findings")),
    page.getByRole("button", { name: "Read again" }).click(),
  ])
  test.info().annotations.push({ type: "findings reads", description: `load ${onLoad}, after two Overview remounts ${afterRemounts}, after Read again ${findingsReads.length}` })
  expect(findingsReads).toHaveLength(2)
})

/** The action that corrects the Party the stand-in holds back: the contact its required-value law asks for. */
const CORRECT_CONTACT = {
  actionId: VERIFY.actionId,
  title: VERIFY.title,
  description: "Record the contact the record is missing.",
  objectKind: "canon.Party",
  effect: "fire_transition",
  objectIdField: "id",
  toState: null,
  uiEffects: { irreversible: false, destructive: false, money: [] },
  inputSchema: {
    type: "object", additionalProperties: false, required: ["id", "email"],
    properties: { id: { type: "string", title: "Party id" }, email: { type: "string", title: "Contact email" } },
  },
}

// #1763 round 1: a record's findings stay fresh for 30 seconds (FINDINGS_STALE_MS), so the header and the
// Findings panel share one read. A correction that releases a quarantined record reads them again at once:
// inside that window the page must not keep showing the record held back, or a finding the cell settled.
test("findings: correcting a quarantined record reads its findings again, so the header and the Findings panel show it released at once", async ({ page }) => {
  await recordCell(page, "admin")
  let corrected = false
  const findingsReads: string[] = []
  const id = "demo/party/0001"
  const held = { objectType: "canon.Party", id, quarantined: true, evidence: [],
    findings: [{ id: "f-1", ruleRef: { kind: "builtin", id: "core.required", version: "1" }, severity: "error", message: "A required value is missing.", status: "open", firstSeen: "2026-11-18T09:12:00Z", lastSeen: "2026-11-18T09:12:00Z" }] }
  const released = { objectType: "canon.Party", id, quarantined: false, evidence: [], findings: [] }
  // While a law holds the record back, the cell serves it to no read: its object read is the opaque 404.
  await page.route((url) => decodeURIComponent(url.pathname).endsWith(`/v1/objects/canon.Party/${id}`), (route) => corrected
    ? route.fallback()
    : route.fulfill({ status: 404, json: { code: "object.not_found", message: "No such readable object." } }))
  await page.route((url) => url.pathname.endsWith("/findings"), (route) => {
    findingsReads.push(route.request().url())
    return route.fulfill({ json: corrected ? released : held })
  })
  // The cell offers the correction on the record, and its commit releases it.
  await page.route("**/api/host/v1/actions**", (route) => {
    if (route.request().method() === "GET") return route.fulfill({ json: { definitionsVersion: 1, items: [CORRECT_CONTACT] } })
    corrected = true
    return route.fulfill({ json: { outcome: "committed", objectType: "canon.Party", objectId: id, version: 3, receipt: { receiptHash: "c".repeat(64), commitSeq: 413, recordedAtMs: Date.now() } } })
  })
  await page.goto(PARTY)
  const header = page.locator("div:has(> h1)")
  const quarantine = page.getByRole("status").filter({ hasText: "Held in quarantine" })
  await expect(quarantine).toBeVisible()
  await expect(header.locator('[data-fact-status="quarantined"]')).toBeVisible()
  await expect(header.getByText("Held back", { exact: true })).toBeVisible()
  await expect(header.getByText("1 rule broken", { exact: true })).toBeVisible()
  await expect.poll(() => findingsReads.length).toBe(1)
  const read = Date.now()
  await shoot(page, "party-quarantined")

  await page.getByRole("button", { name: CORRECT_CONTACT.title }).click()
  const sheet = page.getByRole("dialog")
  await sheet.getByLabel("Contact email").fill("contact@example.com")
  await sheet.getByRole("button", { name: CORRECT_CONTACT.title }).click()
  await expect(sheet.getByRole("status")).toBeVisible()
  await sheet.getByRole("button", { name: "Done" }).click()

  // The cell serves the record now: the page shows it, and its findings as the cell answers them after the
  // correction, in the header and in the Findings panel, without waiting for the earlier read to go stale.
  await expect(quarantine).toHaveCount(0)
  await expect(page.getByRole("tab", { name: "Overview" })).toBeVisible()
  await expect(page.getByText("No findings", { exact: true })).toBeVisible()
  await expect(header.locator('[data-fact-status="quarantined"]')).toHaveCount(0)
  await expect(header.getByText("Held back", { exact: true })).toHaveCount(0)
  await expect(header.getByText("1 rule broken", { exact: true })).toHaveCount(0)
  expect(findingsReads).toHaveLength(2)
  // The row proves the fix only inside the window: past it, the panel's mount alone would read them again.
  expect(Date.now() - read).toBeLessThan(30_000)
  await shoot(page, "party-corrected")
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
  await revealSettled(page, release)
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
  await expect(page).toHaveURL(/\/rules$/)
})
