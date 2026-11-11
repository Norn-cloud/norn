import { PAGES } from "../../src/lib/pages.ts"
import type { E2E } from "./verbs.ts"

/** Every page, opened in a browser on the deployment: it must render, and never show the error screen once its data arrives. */
export async function everyPage({ page, base }: E2E): Promise<readonly string[]> {
  const broken: string[] = []
  for (const url of Object.values(PAGES)) {
    await page.goto(base + url)
    await page.getByRole("heading", { level: 1 }).first().waitFor({ timeout: 30000 }).catch(() => broken.push(`${url}: no heading`))
    // Live reads land after the first paint; a page that breaks on its data shows it here.
    await new Promise((resolve) => setTimeout(resolve, 2000))
    if (await page.getByText("Something went wrong").count() > 0) broken.push(`${url}: error screen`)
  }
  return broken
}
