import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

/** Choose a view (Business, Technical) or a language (English, العربية) from the account menu, where they live. */
async function chooseFromAccountMenu(page: Page, name: string) {
  await page.getByRole("button", { name: /Account$/ }).click()
  await page.getByRole("menuitemradio", { name, exact: true }).click()
}


// LABEL-LOCALE-1 / MESSAGE-LOCALE-1: one row per locale and theme. Arabic mirrors the layout (dir=rtl),
// shows labels and catalog messages in Arabic, and keeps ids, codes and phone numbers isolated left-to-right.
// Screenshots go to test-results/locale for review.

const LOCALES = [
  { tag: "en", dir: "ltr", order: "Sales order", status: "Status", dependency: "required entity" },
  { tag: "ar", dir: "rtl", order: "أمر بيع", status: "الحالة", dependency: "ليس في مجموعة الأدوات" },
] as const

/** Wait for the state a screenshot needs: every running colour transition has finished. A looping animation (a spinner) never finishes and is not waited for. */
const settled = (page: Page) => page.evaluate(() => Promise.all(
  document.getAnimations()
    .filter((animation) => Number.isFinite(animation.effect?.getComputedTiming().endTime))
    .map((animation) => animation.finished.catch(() => undefined)),
).then(() => undefined))

async function shoot(page: Page, name: string) {
  await settled(page)
  for (const state of ["facts", "message", "problems"]) {
    await page.locator(`[data-state="locale/${state}"]`).screenshot({ path: `test-results/locale/${state}-${name}.png` })
  }
}

for (const locale of LOCALES) for (const scheme of ["light", "dark"] as const) {
  test(`locale ${locale.tag} ${scheme}: labels, messages and isolated ids`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme })
    await page.goto(`/sheet.html?locale=${locale.tag}`)
    await expect(page.locator("html")).toHaveAttribute("dir", locale.dir)
    await expect(page.locator("html")).toHaveAttribute("lang", locale.tag)
    await expect(page.locator("html")).toHaveClass(scheme === "dark" ? /dark/ : /^(?!.*dark).*$/)

    const facts = page.locator('[data-state="locale/facts"]')
    await expect(facts.getByRole("heading", { name: locale.order })).toBeVisible()
    const row = (id: string) => facts.getByRole("row").filter({ has: page.locator("bdi", { hasText: new RegExp(`^${id}$`) }) })
    await expect(row("status")).toContainText(locale.status)
    // An unlabelled fact keeps its id in words and says it is English.
    await expect(row("order_date").locator('span[lang="en"]')).toHaveText("order date")
    // A partially labelled enum shows every declared value: six labels in Arabic, and `on_hold`, which has none, as stored and marked English.
    if (locale.tag === "ar") {
      const values = row("status").locator("[data-values]")
      await expect(values).toHaveAttribute("data-values", "7")
      for (const label of ["مسودة", "مُقدَّم", "معتمد", "تمت فوترته", "مكتمل", "ملغى"]) await expect(values).toContainText(label)
      await expect(values.locator('bdi[lang="en"]')).toHaveText("on_hold")
    } else {
      await expect(row("status")).toContainText("One of: draft, submitted, on hold, approved, invoiced, completed, cancelled")
    }
    // Every id is an isolate in its own direction.
    const id = row("order_number").locator("bdi").first()
    await expect(id).toHaveText("order_number")
    await expect(id).toHaveAttribute("dir", "ltr")

    const message = page.locator('[data-state="locale/message"] [data-code="artifact_set.resolution.missing_dependency"]')
    await expect(message).toHaveAttribute("data-message-lang", locale.tag)
    await expect(message).toContainText(locale.dependency)
    await expect(message).toContainText("commerce.SalesOrder.entity")
    await expect(message.locator("bdi")).toHaveText("artifact_set.resolution.missing_dependency")
    // The phone number reads left to right, as one isolate, inside the Arabic sentence.
    const phone = page.locator('[data-state="locale/message"] [data-code="source_schema.validation.unknown_field"]')
    const text = await phone.locator("span[lang]").evaluate((node) => node.textContent ?? "")
    expect(text).toContain(locale.tag === "ar" ? "\u2068+20 100 123 4567\u2069" : "+20 100 123 4567")

    const problems = page.locator('[data-state="locale/problems"]')
    // The inspector's shortcut line keeps every hint left to right.
    await expect(problems.locator('kbd[dir="ltr"]', { hasText: "⌘⇧Z" })).toHaveCount(1)
    await expect(problems.locator('[data-code="lifecycle.validation.dead_end_state"]').first()).toHaveAttribute("data-message-lang", locale.tag)
    await shoot(page, `${locale.tag}-${scheme}`)
  })
}

test("the Studio language switch mirrors the app and is kept for the next visit", async ({ page }) => {
  test.skip(Boolean(process.env.E2E_BASE), "uses the stubbed session")
  await session(page)
  await page.goto("/sources")
  await expect(page.locator("html")).toHaveAttribute("dir", "ltr")
  await chooseFromAccountMenu(page, "العربية")
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl")
  await expect(page.locator("html")).toHaveAttribute("lang", "ar")
  // The recorded model carries the Arabic locale pack (registry/packs/locale-ar): titles read in Arabic.
  await expect(page.getByText("رصيد مخزون").first()).toBeVisible()
  await expect(page.getByText("Holding", { exact: true })).toHaveCount(0)
  // The Business summary is the type's Arabic description from the pack, not its English one-line name.
  await expect(page.getByText("رصيد مقسَّم حسب المالك والموقع").first()).toBeVisible()
  await expect(page.getByText("Owner and location partitioned holding")).toHaveCount(0)
  // Shortcut hints stay one left-to-right isolate: ⌘K, never K⌘.
  const hint = page.locator("kbd", { hasText: "⌘K" }).first()
  await expect(hint).toHaveAttribute("dir", "ltr")
  await expect(hint).toHaveText("⌘K")
  await page.reload()
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl")
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await settled(page)
    await page.screenshot({ path: `test-results/locale/app-ar-${scheme}.png` })
  }
  await chooseFromAccountMenu(page, "English")
  await expect(page.locator("html")).toHaveAttribute("dir", "ltr")
})
