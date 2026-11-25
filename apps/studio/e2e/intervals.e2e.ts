import { expect, test, type Page } from "@playwright/test"
import { intervalCell } from "./interval-cell.ts"
import { BOOK_EQUIPMENT, INTERVAL_ACTIONS, NOW } from "./interval-fixtures.ts"

// One test per row of the operator verbs on two unrelated `core.TimeInterval` worlds (see the PR):
// the Table | Timeline view, the form's date, time and record pickers, and refusals shown at the
// field their pointer names. They use a stand-in cell (interval-cell.ts), so they skip on a
// deployed stage. SHOTS=<dir> saves every state in light and dark for review.

const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400)
    await page.screenshot({ path: `${shots}/intervals-${name}-${scheme}.png` })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(async ({ page }, info) => {
  test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the interval stand-in cell`)
  await page.clock.setFixedTime(new Date(NOW))
})
test.use({ timezoneId: "UTC", locale: "en-GB" })

const timeline = (page: Page, title: string) => page.getByRole("region", { name: `${title} timeline` })
const bars = (page: Page, title: string) => timeline(page, title).getByRole("group", { name: "Bars" }).getByRole("button")
const sheet = (page: Page) => page.getByRole("dialog")

async function openTimeline(page: Page, type: string, title: string) {
  await page.goto(`/objects/${type}`)
  await page.getByRole("radio", { name: "Timeline" }).click()
  await expect(timeline(page, title)).toBeVisible()
}

test("view: only a core.TimeInterval type offers Timeline; its week shows the records the cell returns for it", async ({ page }) => {
  const cell = await intervalCell(page)
  await page.goto("/objects/facilities.Equipment")
  await expect(page.getByRole("grid", { name: "Equipment records" })).toBeVisible()
  await expect(page.getByRole("radio", { name: "Timeline" })).toHaveCount(0)
  await openTimeline(page, "facilities.EquipmentBooking", "Equipment booking")
  await expect(timeline(page, "Equipment booking").getByRole("heading")).toHaveText("23 Nov – 29 Nov 2026")
  await expect(bars(page, "Equipment booking")).toHaveCount(4)
  // The window is the cell's query: the overlap test as two timestamp clauses, in start order.
  const latestBookingQuery = () => cell.requests.filter((url) => url.pathname.endsWith("/v1/objects/facilities.EquipmentBooking")).at(-1)
  await expect.poll(() => {
    const asked = latestBookingQuery()
    return asked ? { filter: JSON.parse(asked.searchParams.get("filter") ?? "null"), sort: asked.searchParams.get("sort") } : null
  }).toEqual({
    filter: { all: [
      { property: "starts_at", op: "lt", value: "2026-11-30T00:00:00.000Z" },
      { property: "ends_at", op: "gt", value: "2026-11-23T00:00:00.000Z" },
    ] },
    sort: "starts_at:asc",
  })
  await expect(bars(page, "Equipment booking").filter({ hasText: "December service" })).toHaveCount(0)
  // A booking that began the week before is clipped at the window's start.
  await expect(bars(page, "Equipment booking").filter({ hasText: "Inspection" })).toHaveClass(/rounded-s-none/)
  // A record that names its zone reads its own times there.
  await expect(bars(page, "Equipment booking").filter({ hasText: "Warehouse move" })).toHaveAttribute("aria-label", /Warehouse move, Tue 24 Nov, 06:00 to Tue 24 Nov, 15:00 \(Asia\/Tokyo\)/)
  await shoot(page, "timeline-week")
})

test("lanes: a to-one link splits the bars into lanes; two bookings sharing time on one lane stack and are marked", async ({ page }) => {
  await intervalCell(page)
  await openTimeline(page, "facilities.EquipmentBooking", "Equipment booking")
  const lanes = page.getByLabel("Lanes")
  // Only to-one links are offered (accessories is to-many).
  await expect(lanes.locator("option")).toHaveText(["None", "By equipment", "By booked by"])
  await lanes.selectOption("equipment")
  const bars = timeline(page, "Equipment booking").getByRole("group", { name: "Bars" })
  await expect(bars.getByRole("group", { name: "equipment/1" })).toContainText("2 overlapping")
  await expect(bars.getByRole("group", { name: "equipment/1" }).locator("[data-overlaps]")).toHaveCount(2)
  await expect(bars.getByRole("group", { name: "equipment/2" }).getByRole("button")).toHaveCount(1)
  await expect(bars.getByRole("group", { name: "equipment/3" }).getByRole("button")).toHaveCount(1)
  await shoot(page, "timeline-lanes-overlap")
})

test("zoom and move: day, week and month windows; an empty window offers the nearest record either side", async ({ page }) => {
  await intervalCell(page)
  await openTimeline(page, "facilities.EquipmentBooking", "Equipment booking")
  const view = timeline(page, "Equipment booking")
  await view.getByRole("radio", { name: "Day" }).click()
  await expect(view.getByRole("heading")).toHaveText(/Monday,? 23 November 2026/)
  // The two cutter runs, the inspection that spans the day and the move that starts late that evening (UTC).
  await expect(bars(page, "Equipment booking")).toHaveCount(4)
  await view.getByRole("radio", { name: "Month" }).click()
  await expect(view.getByRole("heading")).toHaveText("November 2026")
  await expect(bars(page, "Equipment booking")).toHaveCount(4)
  await view.getByRole("button", { name: "Next month" }).click()
  await expect(view.getByRole("heading")).toHaveText("December 2026")
  await expect(bars(page, "Equipment booking")).toHaveCount(1)
  await view.getByRole("button", { name: "Next month" }).click()
  await expect(view.getByText("Nothing in this month")).toBeVisible()
  await shoot(page, "timeline-empty")
  await view.getByRole("button", { name: /Latest before/ }).click()
  await expect(view.getByRole("heading")).toHaveText("December 2026")
  await view.getByRole("button", { name: "Today" }).click()
  await expect(view.getByRole("heading")).toHaveText("November 2026")
})

test("open: arrows move between bars, Enter opens the record's drawer with only the actions the cell offers on it", async ({ page }) => {
  await intervalCell(page)
  await openTimeline(page, "facilities.EquipmentBooking", "Equipment booking")
  const first = bars(page, "Equipment booking").first()
  await first.focus()
  await expect(first).toHaveAttribute("aria-label", /^Inspection,/)
  await page.keyboard.press("ArrowRight")
  await expect(page.locator(":focus")).toHaveAttribute("aria-label", /^Prototype run,/)
  await page.keyboard.press("End")
  await expect(page.locator(":focus")).toHaveAttribute("aria-label", /^Warehouse move,/)
  await page.keyboard.press("Home")
  await page.keyboard.press("ArrowRight")
  await page.keyboard.press("Enter")
  const drawer = sheet(page)
  await expect(drawer.getByRole("heading", { name: "Prototype run" })).toBeVisible()
  await expect(drawer.getByRole("region", { name: "Actions" }).getByRole("button")).toHaveText(["Book equipment (edit)", "Cancel booking"])
  await shoot(page, "timeline-drawer")
  await drawer.getByRole("button", { name: "Cancel booking" }).click()
  await expect(page.getByLabel("Booking id")).toHaveValue("booking/1")
})

test("view: an unrelated type that is a TimeInterval through its own interface reads the same way", async ({ page }) => {
  await intervalCell(page)
  await openTimeline(page, "venues.RoomReservation", "Room reservation")
  await expect(bars(page, "Room reservation")).toHaveCount(2)
  await expect(bars(page, "Room reservation").filter({ hasText: "Podcast recording" })).toHaveAttribute("aria-label", /Thu 26 Nov, 09:00 to Thu 26 Nov, 11:00 \(America\/New_York\)/)
  await page.getByLabel("Lanes").selectOption("room")
  await expect(timeline(page, "Room reservation").getByRole("group", { name: "room/1" })).toContainText("Quarterly review")
  await shoot(page, "timeline-rooms")
  await page.getByRole("radio", { name: "Table" }).click()
  await expect(page.getByRole("grid", { name: "Room reservation records" })).toBeVisible()
})

test("picker: the search field and its list are named; arrows and Enter pick by id; Escape returns focus to the field", async ({ page }) => {
  await intervalCell(page)
  await page.goto("/objects/facilities.EquipmentBooking")
  await page.getByRole("button", { name: "New equipment booking" }).click()
  const form = sheet(page)
  const equipment = form.getByRole("combobox", { name: "Equipment", exact: true })
  await equipment.click()
  const search = page.getByRole("combobox", { name: "Search equipment records" })
  await expect(search).toBeFocused()
  // Focus lands in the search field with Studio's 2px ring outline, never a bare `outline-none`.
  await expect(search).toHaveCSS("outline-style", "solid")
  await expect(search).toHaveCSS("outline-width", "2px")
  const list = page.getByRole("listbox", { name: "Equipment records" })
  await expect(list.getByRole("option").first()).toBeVisible()
  // The list opens under the field, at least as wide as it, inside the sheet: nothing scrolls the sheet sideways.
  // The sheet is the outer dialog; the open list is a dialog inside it.
  // It opens from 95% scale, so it is measured once settled.
  const [field, panel] = await Promise.all([equipment.boundingBox(), form.first().boundingBox()])
  const popup = page.getByRole("dialog", { name: "Choose equipment" })
  await expect.poll(async () => (await popup.boundingBox())!.width).toBeGreaterThanOrEqual(field!.width - 1)
  const settled = (await popup.boundingBox())!
  expect(settled.x).toBeGreaterThanOrEqual(panel!.x - 1)
  expect(settled.x + settled.width).toBeLessThanOrEqual(panel!.x + panel!.width + 1)
  await search.fill("fork")
  await expect(list.getByRole("option")).toHaveText([/Forklift\s*equipment\/2/, /Use the id/])
  await shoot(page, "picker-named")
  await page.keyboard.press("Enter")
  await expect(list).toBeHidden()
  await expect(equipment).toContainText("Forklift")
  await expect(equipment).toBeFocused()
  // A required field offers no Clear; Escape closes the list and leaves the sheet and the pick alone.
  await equipment.click()
  await expect(list.getByRole("option", { name: /Forklift/ })).toBeVisible()
  await expect(list.getByRole("option", { name: "Clear selection" })).toHaveCount(0)
  await page.keyboard.press("Escape")
  await expect(list).toBeHidden()
  await expect(equipment).toBeFocused()
  await expect(form).toHaveCount(1)
  await expect(equipment).toContainText("Forklift")
})

// #1724 (Fable r1): on an optional field that has a value, a search highlights the match, never Clear.
test("picker: on an optional field with a value, a search then Enter picks the match; Clear is listed only unsearched", async ({ page }) => {
  // The same action with its equipment optional (the cell's schema decides; Studio reads `required`),
  // served through the cell's own routes so the catalog keeps the shape the client reads.
  const optional = { ...BOOK_EQUIPMENT, inputSchema: { ...BOOK_EQUIPMENT.inputSchema, required: BOOK_EQUIPMENT.inputSchema.required.filter((field) => field !== "equipment") } }
  await intervalCell(page, { actions: INTERVAL_ACTIONS.map((action) => action === BOOK_EQUIPMENT ? optional as unknown as typeof action : action) })
  await page.goto("/objects/facilities.EquipmentBooking")
  await page.getByRole("button", { name: "New equipment booking" }).click()
  const form = sheet(page)
  const equipment = form.getByRole("combobox", { name: "Equipment", exact: true })
  const list = page.getByRole("listbox", { name: "Equipment records" })
  const search = page.getByRole("combobox", { name: "Search equipment records" })
  // Give the field a value first: Spare unit 01.
  await equipment.click()
  await search.fill("Spare unit 01")
  await expect(list.getByRole("option").first()).toContainText("Spare unit 01")
  await page.keyboard.press("Enter")
  await expect(list).toBeHidden()
  await expect(equipment).toContainText("Spare unit 01")
  // Unsearched, the optional field with a value lists Clear first.
  await equipment.click()
  await expect(list.getByRole("option").first()).toHaveText(/Clear selection/)
  // Searched, Clear is gone and Enter picks the match.
  await search.fill("fork")
  await expect(list.getByRole("option")).toHaveText([/Forklift\s*equipment\/2/, /Use the id/])
  await expect(list.getByRole("option", { name: "Clear selection" })).toHaveCount(0)
  await shoot(page, "picker-optional-search")
  await page.keyboard.press("Enter")
  await expect(list).toBeHidden()
  await expect(equipment).toContainText("Forklift")
  // The X beside the field still clears it.
  await form.getByRole("button", { name: "Clear selection" }).click()
  await expect(equipment).toContainText("Choose equipment")
})

// #1724 (Fable r2): rendered proof for the picker's phone targets, its focus ring and reduced motion (DESIGN.md 16.2: 4, 5, 7).
test("picker below 640px: the trigger, Clear, search field and options are at least 44px; the trigger shows the 2px ring on keyboard focus", async ({ page }) => {
  const optional = { ...BOOK_EQUIPMENT, inputSchema: { ...BOOK_EQUIPMENT.inputSchema, required: BOOK_EQUIPMENT.inputSchema.required.filter((field) => field !== "equipment") } }
  await intervalCell(page, { actions: INTERVAL_ACTIONS.map((action) => action === BOOK_EQUIPMENT ? optional as unknown as typeof action : action) })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/objects/facilities.EquipmentBooking")
  await page.getByRole("button", { name: "New equipment booking" }).click()
  const form = sheet(page)
  const equipment = form.getByRole("combobox", { name: "Equipment", exact: true })
  const search = page.getByRole("combobox", { name: "Search equipment records" })
  const list = page.getByRole("listbox", { name: "Equipment records" })
  await equipment.click()
  await search.fill("Spare unit 01")
  await expect(list.getByRole("option").first()).toContainText("Spare unit 01")
  // Every target in the open list is at least 44px tall, measured once the list has settled from its 95% scale.
  await expect.poll(async () => (await search.boundingBox())!.height).toBeGreaterThanOrEqual(44)
  for (const option of await list.getByRole("option").all()) expect((await option.boundingBox())!.height).toBeGreaterThanOrEqual(44)
  await page.keyboard.press("Enter")
  await expect(list).toBeHidden()
  const [trigger, clear] = await Promise.all([equipment.boundingBox(), form.getByRole("button", { name: "Clear selection" }).boundingBox()])
  expect(trigger!.height).toBeGreaterThanOrEqual(44)
  expect(clear!.width).toBeGreaterThanOrEqual(44)
  expect(clear!.height).toBeGreaterThanOrEqual(44)
  // Onto the trigger from the keyboard: a solid 2px ring in --ring.
  await form.getByRole("button", { name: "Clear selection" }).focus()
  await page.keyboard.press("Shift+Tab")
  await expect(equipment).toBeFocused()
  await expect(equipment).toHaveCSS("outline-style", "solid")
  await expect(equipment).toHaveCSS("outline-width", "2px")
  const ring = await page.evaluate(() => {
    const probe = document.createElement("span")
    probe.style.color = "var(--ring)"
    document.body.append(probe)
    const color = getComputedStyle(probe).color
    probe.remove()
    return color
  })
  await expect(equipment).toHaveCSS("outline-color", ring)
  await shoot(page, "picker-phone-targets")
})

test("picker under reduced motion: the list opens without its scale and fade transition", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" })
  await intervalCell(page)
  await page.goto("/objects/facilities.EquipmentBooking")
  await page.getByRole("button", { name: "New equipment booking" }).click()
  await sheet(page).getByRole("combobox", { name: "Equipment", exact: true }).click()
  const popup = page.getByRole("dialog", { name: "Choose equipment" })
  await expect(popup).toBeVisible()
  await expect(popup).toHaveCSS("transition-property", "none")
  await expect(popup).toHaveCSS("scale", "none")
})

test.describe("in Cairo", () => {
  test.use({ timezoneId: "Africa/Cairo" })

  test("create: date, time and instant pickers send their formats; the record picker searches by title and keeps the id", async ({ page }) => {
    const cell = await intervalCell(page)
    await page.goto("/objects/facilities.EquipmentBooking")
    await page.getByRole("button", { name: "New equipment booking" }).click()
    const form = sheet(page)
    await form.getByLabel("Booking id").fill("booking/9")
    await form.getByLabel("Title").fill("Overnight cover")
    const equipment = form.getByRole("combobox", { name: "Equipment", exact: true })
    await equipment.click()
    const list = page.getByRole("listbox")
    // The first page, then the next one on request: every record stays reachable.
    await expect(list.getByRole("option")).toHaveCount(26)
    await page.getByRole("option", { name: "Show more" }).click()
    await expect(list.getByRole("option").filter({ hasText: "Spare unit 30" })).toBeVisible()
    await page.getByPlaceholder(/Search equipment/).fill("fork")
    await expect(list.getByRole("option")).toHaveText([/Forklift\s*equipment\/2/, /Use the id/])
    await shoot(page, "picker-search")
    await page.getByRole("option", { name: /Forklift/ }).click()
    await expect(equipment).toContainText("Forklift")
    await expect(equipment).toContainText("equipment/2")
    await form.getByLabel("Starts").fill("2026-11-27T09:30")
    await expect(form.getByText("sent as 2026-11-27T09:30:00+02:00")).toBeVisible()
    await form.getByLabel("Ends").fill("2026-11-27T17:00")
    await form.getByLabel("Inspect on").fill("2026-11-30")
    await form.getByLabel("Daily check at").fill("07:45")
    await shoot(page, "pickers")
    await form.getByRole("button", { name: "Book equipment" }).click()
    await expect(form.getByRole("status")).toHaveText(/booking\/9 created/)
    const sent = JSON.parse(cell.calls.at(-1)!.body) as { input: Record<string, unknown> }
    expect(sent.input).toEqual({ id: "booking/9", label: "Overnight cover", equipment: "equipment/2", starts_at: "2026-11-27T09:30:00+02:00", ends_at: "2026-11-27T17:00:00+02:00", inspect_on: "2026-11-30", daily_check: "07:45:00" })
  })

  test("validate: a refusal that points at fields shows on those rows; one without pointers stays on the form", async ({ page }) => {
    await intervalCell(page, { pointed: true })
    await page.goto("/objects/facilities.EquipmentBooking")
    await page.getByRole("button", { name: "New equipment booking" }).click()
    const form = sheet(page)
    await form.getByLabel("Booking id").fill("booking/10")
    await form.getByLabel("Title").fill("Backwards")
    await form.getByRole("combobox", { name: "Equipment", exact: true }).click()
    await page.getByPlaceholder(/Search equipment/).fill("equipment/999")
    await page.getByRole("option", { name: /Use the id/ }).click()
    await form.getByLabel("Starts").fill("2026-11-27T17:00")
    await form.getByLabel("Ends").fill("2026-11-27T09:00")
    await form.getByRole("button", { name: "Book equipment" }).click()
    await expect(form.getByRole("alert").first()).toContainText("Each problem is shown at its field below.")
    const ends = form.locator('[data-pointer="/ends_at"]')
    await expect(ends.getByRole("alert")).toHaveText("Ends must be after it starts.")
    await expect(ends.getByLabel("Ends")).toHaveAttribute("aria-invalid", "true")
    await expect(form.locator('[data-pointer="/equipment"]').getByRole("alert")).toHaveText("No such record.")
    await expect(form.locator('[data-pointer="/starts_at"]').getByRole("alert")).toHaveCount(0)
    await shoot(page, "refusal-pointed")
  })

  test("validate: the served refusal (no pointers) reads on the form and marks no field", async ({ page }) => {
    await intervalCell(page)
    await page.goto("/objects/venues.RoomReservation")
    await page.getByRole("button", { name: "New room reservation" }).click()
    const form = sheet(page)
    await form.getByLabel("Reservation id").fill("reservation/9")
    await form.getByLabel("Title").fill("Backwards")
    await form.getByRole("combobox", { name: "Room", exact: true }).click()
    await page.getByRole("option", { name: /Studio B/ }).click()
    await form.getByLabel("Starts").fill("2026-11-27T17:00")
    await form.getByLabel("Ends").fill("2026-11-27T09:00")
    await form.getByLabel("Set up on").fill("2026-11-26")
    await form.getByRole("button", { name: "Reserve room" }).click()
    await expect(form.getByRole("alert").first()).toContainText("The input doesn't fit this action")
    await expect(form.locator('[aria-invalid="true"]')).toHaveCount(0)
  })

  test("picker: a refusal on Show more keeps page 1 visible, surfaces the alert inline, and Retry re-requests the same cursor", async ({ page }) => {
    const cell = await intervalCell(page)
    let refusePage2 = true
    const page2Cursors: (string | null)[] = []
    await page.route("**/api/host/v1/objects/facilities.Equipment**", async (route) => {
      const url = new URL(route.request().url())
      const after = url.searchParams.get("after")
      if (after !== null) {
        page2Cursors.push(after)
        if (refusePage2) {
          refusePage2 = false
          return route.fulfill({ status: 403, json: { code: "authz.denied", message: "Access changed mid-page." } })
        }
      }
      return route.fallback()
    })
    await page.goto("/objects/facilities.EquipmentBooking")
    await page.getByRole("button", { name: "New equipment booking" }).click()
    const form = sheet(page)
    await form.getByRole("combobox", { name: "Equipment", exact: true }).click()
    const list = page.getByRole("listbox")
    await expect(list.getByRole("option", { name: /Forklift/ })).toBeVisible()
    const picker = page.getByRole("dialog", { name: "Choose equipment" })
    await page.getByRole("option", { name: "Show more" }).click()
    await expect(picker.getByRole("alert")).toHaveText("Access changed mid-page.")
    await expect(list.getByRole("option", { name: /Forklift/ })).toBeVisible()
    await page.getByRole("option", { name: "Retry" }).click()
    await expect(list.getByRole("option").filter({ hasText: "Spare unit 30" })).toBeVisible()
    await expect(picker.getByRole("alert")).toHaveCount(0)
    expect(page2Cursors).toEqual(["25", "25"])
    expect(cell.requests.length).toBeGreaterThan(0)
  })
})
