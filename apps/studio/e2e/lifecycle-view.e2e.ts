import { expect, test, type Page } from "@playwright/test"

// The lifecycle view (UI-LIFECYCLE-2), on its contact-sheet specimens: counts from the cell's
// facets answer, a live answer replacing them in place, and a counterexample stepped by its
// witness. The specimens are the real element with Studio's tokens.

const specimen = async (page: Page, state: string, locale = "en") => {
  await page.goto(`/sheet.html?state=${encodeURIComponent(state)}&locale=${locale}`)
  return page.locator(`[data-state="${state}"] norn-lifecycle-view`)
}
const card = (view: ReturnType<Page["locator"]>, state: string) => view.locator(`[data-state="${state}"]`)

test("view: every declared state with the cell's count; a state no record is in reads 0", async ({ page }) => {
  const view = await specimen(page, "lifecycle-view/counts")
  await expect(card(view, "draft").locator("[data-count]")).toHaveText("12")
  await expect(card(view, "rejected").locator("[data-count]")).toHaveText("0")
  await expect(view.getByText("54 records")).toBeVisible()
  await expect(view.locator(".norn-life-arrows > path")).toHaveCount(7)
})

test("validate: past the scan bound counts are lower bounds and unseen rows are said; a refusal shows no counts", async ({ page }) => {
  let view = await specimen(page, "lifecycle-view/lower-bound")
  await expect(card(view, "draft").locator("[data-count]")).toHaveText("≥ 6,200")
  await expect(view.getByText("14 withheld from you · 2 held in quarantine")).toBeVisible()
  view = await specimen(page, "lifecycle-view/refused")
  await expect(view.getByText("You can't see these records' counts.")).toBeVisible()
  await expect(card(view, "draft").locator("[data-count]")).toHaveText("—")
})

test("live: a new answer on the socket changes the counts in place", async ({ page }) => {
  const view = await specimen(page, "lifecycle-view/live")
  await expect(card(view, "draft").locator("[data-count]")).toHaveText("11")
  await expect(card(view, "submitted").locator("[data-count]")).toHaveText("8")
})

test("stepper: a counterexample is walked step by step; taken transitions and the object's state follow", async ({ page }) => {
  const view = await specimen(page, "lifecycle-view/stepper")
  await expect(view.getByText("Step 3 of 4")).toBeVisible()
  await expect(card(view, "rejected")).toHaveAttribute("data-current", "")
  await expect(view.locator(".norn-life-arrows > path[data-active]")).toHaveCount(2)
  await view.getByRole("button", { name: "Next step" }).click()
  await expect(view.getByText("Step 4 of 4")).toBeVisible()
  await expect(card(view, "draft")).toHaveAttribute("data-current", "")
  await expect(view.getByRole("button", { name: "Next step" })).toBeDisabled()
  for (let step = 0; step < 4; step += 1) await view.getByRole("button", { name: "Previous step" }).click()
  await expect(view.getByText("Step 0 of 4")).toBeVisible()
  await expect(view.locator("[data-current]")).toHaveCount(0)
  // Closing the finding ends the walk.
  await view.getByRole("button", { name: "The page can show “Done” while the purchase order is still “Draft”, and nothing can finish it." }).click()
  await expect(view.getByText(/Step \d of 4/)).toBeHidden()
})

test("technical: the reader's sentence first, the code and the checker's own words under it; steps read as the definition names them", async ({ page }) => {
  let view = await specimen(page, "lifecycle-view/stepper")
  await expect(view.getByText("widget.lifecycle.stuck")).toHaveCount(0)
  await expect(view.locator('[data-step="3"]')).toContainText("Reject")
  view = await specimen(page, "lifecycle-view/technical")
  await expect(view.getByRole("button", { name: /The page can show “Done”/ })).toBeVisible()
  await expect(view.locator(".norn-life-technical")).toContainText("widget.lifecycle.stuck page state `done` with the object `draft`")
})

test("locale: Arabic runs right to left with Arabic digits and words", async ({ page }) => {
  const view = await specimen(page, "lifecycle-view/rtl", "ar")
  await expect(view.locator(".norn-life")).toHaveAttribute("dir", "rtl")
  await expect(card(view, "draft").locator("[data-count]")).toHaveText("١٢")
  await expect(view.getByText("الخطوة ٤ من ٤")).toBeVisible()
  await expect(view.getByRole("button", { name: "قد تعرض الصفحة «تم» بينما أمر شراء ما زال «مسودة»، ولا شيء يستطيع إنهاءه." })).toBeVisible()
})

test("not served: a cell without the facets route says so, never counting forever; Try again asks again", async ({ page }) => {
  const view = await specimen(page, "lifecycle-view/not-served")
  await expect(view.getByText(/This cell does not serve lifecycle counts yet\./)).toBeVisible()
  await expect(view.locator(".norn-life")).toHaveAttribute("data-machine-state", "unavailable")
  await expect(card(view, "draft").locator("[data-count]")).toHaveText("—")
  await view.getByRole("button", { name: "Try again" }).click()
  await expect(view.locator(".norn-life")).toHaveAttribute("data-machine-state", "unavailable")
})
