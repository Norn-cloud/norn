import { expect, test, type Page } from "@playwright/test"
import { themeCell } from "./theme-cell.ts"

// Themes as data: a workspace's look is its `theme` definition. Applied through the definitions API
// (by an agent, or from the Appearance menu), it repaints every open page with no reload; the
// checker refuses what fails its rules, in its own words; and each workspace paints its own theme.

const SHOTS = process.env.SHOTS
const WORKSPACES = [
  { id: "org_alpha", name: "Core Analytics", environmentId: "env_alpha" },
  { id: "org_beta", name: "North Operations", environmentId: "env_beta" },
] as const

const PAPER = { version: 1, name: "Paper", baseSkin: "polar", variants: { light: { canvas: "#f4f2ed", shell: "#f4f2ed" }, dark: { canvas: "#121110", shell: "#121110" } } }
const GRAPHITE = { version: 1, name: "Graphite", baseSkin: "polar", variants: { light: { canvas: "#e4e4e7", shell: "#e4e4e7" }, dark: { canvas: "#08090c", shell: "#08090c" } } }

/** The page's painted ground: `body` is `bg-canvas`. */
const ground = (page: Page) => page.evaluate(() => getComputedStyle(document.body).backgroundColor)
const themeName = (page: Page) => page.evaluate(() => document.documentElement.dataset.customTheme ?? null)

/** What an agent does: install the workspace's theme through the definitions API, from outside this page. */
async function agentInstalls(page: Page, payload: unknown, expectedVersion?: number) {
  return page.evaluate(async ({ payload, expectedVersion }) => {
    const response = await fetch("/api/host/v1/definitions:install?orgId=org_alpha&environmentId=env_alpha", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ kind: "theme", id: "workspace.theme", payload, ...(expectedVersion === undefined ? {} : { expectedVersion }) }),
    })
    return { status: response.status, body: await response.json() as unknown }
  }, { payload, expectedVersion })
}

async function openEditor(page: Page) {
  await page.locator('[data-slot="chrome-dock-trigger"]').first().click()
  await page.locator('[data-slot="workspace-theme-trigger"]').click()
  const dialog = page.getByRole("dialog", { name: "Workspace theme" })
  await expect(dialog).toBeVisible()
  return dialog
}

test.beforeEach(async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.emulateMedia({ colorScheme: "light" })
})

test("a theme applied through the definitions API repaints an open page with no reload, and resetting it brings back the default", async ({ page, context }) => {
  const cell = await themeCell(context, WORKSPACES)
  await page.goto("/sources")
  await expect(page.getByRole("heading", { name: "Sources", level: 1 })).toBeVisible()
  // No theme served: the built-in Luma default paints.
  await expect.poll(() => ground(page)).toBe("rgb(255, 255, 255)")
  expect(await themeName(page)).toBeNull()
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/theme-live-before.png` })
  await page.evaluate(() => { (window as unknown as { notReloaded: boolean }).notReloaded = true })

  // Another tab (an agent) installs the theme; this page repaints from the feed.
  const agent = await context.newPage()
  await agent.goto("/sources")
  expect((await agentInstalls(agent, PAPER)).status).toBe(200)
  await agent.close()
  await expect.poll(() => ground(page), { timeout: 15_000 }).toBe("rgb(244, 242, 237)")
  expect(await themeName(page)).toBe("Paper")
  expect(await page.evaluate(() => (window as unknown as { notReloaded?: boolean }).notReloaded)).toBe(true)
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/theme-live-after.png` })

  // The reader's light/dark choice stays theirs: the theme's dark variant paints in dark.
  await page.emulateMedia({ colorScheme: "dark" })
  await expect.poll(() => ground(page)).toBe("rgb(18, 17, 16)")
  await page.emulateMedia({ colorScheme: "light" })

  // The editor opens on the workspace's theme; Reset to default retires it, and the default repaints.
  const dialog = await openEditor(page)
  await expect(dialog.getByRole("textbox", { name: "Theme JSON definition" })).toHaveValue(/"name": "Paper"/)
  await dialog.getByRole("button", { name: "Reset to default" }).click()
  await expect(dialog).toBeHidden()
  await expect.poll(() => ground(page)).toBe("rgb(255, 255, 255)")
  expect(await themeName(page)).toBeNull()

  // Use for this workspace installs the edited theme (a preset here) through the same apply path.
  const again = await openEditor(page)
  await again.getByRole("button", { name: "Paper Editorial" }).click()
  await again.getByRole("button", { name: "Use for this workspace" }).click()
  await expect(again).toBeHidden()
  await expect.poll(() => themeName(page)).toBe("Paper Editorial")
  await expect.poll(() => ground(page)).toBe("rgb(244, 242, 237)")
  expect(cell.installs.map(({ orgId, admitted }) => [orgId, admitted])).toEqual([["org_alpha", true], ["org_alpha", true]])
  // The theme was retired, so this install is a new one; the first replaced nothing either.
  expect(cell.installs.map(({ expectedVersion }) => expectedVersion)).toEqual([undefined, undefined])
  expect(await page.evaluate(() => (window as unknown as { notReloaded?: boolean }).notReloaded)).toBe(true)
})

test("a low-contrast theme is refused by the checker in its own words, and the page keeps its theme", async ({ page, context }) => {
  const cell = await themeCell(context, WORKSPACES)
  await page.goto("/sources")
  await expect(page.getByRole("heading", { name: "Sources", level: 1 })).toBeVisible()
  expect((await agentInstalls(page, PAPER)).status).toBe(200)
  await expect.poll(() => themeName(page), { timeout: 15_000 }).toBe("Paper")
  const before = await ground(page)

  const dialog = await openEditor(page)
  await dialog.getByRole("textbox", { name: "Theme JSON definition" }).fill(JSON.stringify({
    version: 1,
    name: "Washed Out",
    baseSkin: "luma",
    colors: { "muted-foreground": "#c8c8c8" },
  }))
  await dialog.getByRole("button", { name: "Use for this workspace" }).click()
  const refusal = dialog.locator('[data-slot="theme-validation-errors"]')
  await expect(refusal).toHaveAttribute("data-refused-by", "cell")
  await expect(refusal).toContainText("The cell refused this theme")
  await expect(refusal).toContainText("The checker: light muted-foreground (on canvas) contrast is")
  await expect(refusal).toContainText("WCAG AA requires >= 4.5:1")
  expect(cell.installs.at(-1)).toEqual({ orgId: "org_alpha", expectedVersion: 1, admitted: false })

  // Nothing changed: the dialog stays open on the draft, and the page keeps its theme.
  await expect(dialog).toBeVisible()
  await page.keyboard.press("Escape")
  await expect(dialog).toBeHidden()
  expect(await themeName(page)).toBe("Paper")
  expect(await ground(page)).toBe(before)

  // A JSON that is not a theme at all never leaves Studio.
  const typo = await openEditor(page)
  await typo.getByRole("textbox", { name: "Theme JSON definition" }).fill(JSON.stringify({ version: 1, name: "Typo", colors: { "sidebar-width": "#000000" } }))
  await typo.getByRole("button", { name: "Use for this workspace" }).click()
  await expect(typo.locator('[data-slot="theme-validation-errors"]')).toHaveAttribute("data-refused-by", "studio")
  await expect(typo.locator('[data-slot="theme-validation-errors"]')).toContainText("sidebar-width")
  expect(cell.installs).toHaveLength(2)
})

test("switching workspace switches the theme, and a served theme that fails the checks never paints", async ({ page, context }) => {
  const cell = await themeCell(context, WORKSPACES)
  await page.goto("/sources")
  await expect(page.locator("[data-workspace-state]")).toContainText("Core Analytics")
  expect((await agentInstalls(page, PAPER)).status).toBe(200)
  await expect.poll(() => themeName(page), { timeout: 15_000 }).toBe("Paper")
  await expect.poll(() => ground(page)).toBe("rgb(244, 242, 237)")
  // North Operations has its own theme.
  expect((await page.evaluate(async (payload) => (await fetch("/api/host/v1/definitions:install?orgId=org_beta&environmentId=env_beta", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ kind: "theme", id: "workspace.theme", payload }) })).status, GRAPHITE))).toBe(200)

  expect(await page.evaluate(async () => (await fetch("/api/workspaces/org_beta/activate", { method: "POST" })).status)).toBe(200)
  await page.reload()
  await expect(page.locator("[data-workspace-state]")).toContainText("North Operations")
  await expect.poll(() => themeName(page), { timeout: 15_000 }).toBe("Graphite")
  await expect.poll(() => ground(page)).toBe("rgb(228, 228, 231)")

  expect(await page.evaluate(async () => (await fetch("/api/workspaces/org_alpha/activate", { method: "POST" })).status)).toBe(200)
  await page.reload()
  await expect(page.locator("[data-workspace-state]")).toContainText("Core Analytics")
  await expect.poll(() => themeName(page), { timeout: 15_000 }).toBe("Paper")

  // A theme admitted under rules that later changed is served but fails Studio's gate: the default
  // paints, and a note says why, in the gate's words.
  cell.serveUnchecked("org_alpha", { version: 1, name: "Blue Chrome", baseSkin: "luma", appearance: "light", colors: { action: "#1d4ed8", link: "#1d4ed8" } })
  const note = page.locator('[data-workspace-theme="invalid"]')
  await expect(note).toBeVisible({ timeout: 15_000 })
  await expect(note).toContainText("Workspace theme not applied")
  await expect(note).toContainText("a hue")
  expect(await themeName(page)).toBeNull()
  await expect.poll(() => ground(page)).toBe("rgb(255, 255, 255)")
})
