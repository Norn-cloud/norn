import type { Page } from "@playwright/test"

/** Choose a view (Business, Technical) or a language (English, العربية) from the account menu, where they live. */
export async function chooseFromAccountMenu(page: Page, name: string) {
  await page.getByRole("button", { name: /Account$/ }).click()
  await page.getByRole("menuitemradio", { name, exact: true }).click()
}
