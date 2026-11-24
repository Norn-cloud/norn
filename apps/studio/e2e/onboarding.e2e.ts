import { readFileSync } from "node:fs"
import { expect, test, type Page, type Request } from "@playwright/test"
import type { WorkspacesDocument } from "../src/lib/workspaces-api.ts"

const INVITE_ONLY_MESSAGE = "This alpha is invite-only; ask your admin for an invitation."
const SIGNED_UP_MESSAGE = "If this address can sign up, you can now sign in with your email and password."
const POPULATED_DEFS = JSON.parse(readFileSync(new URL("../../../fixtures/demo/explorer/three-systems.definitions.json", import.meta.url), "utf8")) as unknown[]
const EMPTY_BASE_DEFS = POPULATED_DEFS.filter((artifact) => {
  const id = (artifact as { artifactId?: string }).artifactId ?? ""
  return id.startsWith("core.") || id.startsWith("std.")
})

// SHOTS=<dir> saves key states in light and dark mode at 1440x900 for visual inspection and PR evidence.
const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(300)
    await page.screenshot({ path: `${shots}/onboarding-${name}-${scheme}.png` })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(({ }, info) => {
  test.skip(Boolean(process.env.E2E_BASE), `${info.title}: replays stubbed auth and workspace endpoints`)
})

async function signedOut(page: Page, config: { magicLink: boolean; inviteOnly: boolean }, options: { allowlist?: readonly string[] } = {}) {
  const registered = new Set<string>()
  await page.route("**/api/me", (route) => route.fulfill({ status: 401, json: { error: "Sign in first." } }))
  await page.route("**/api/auth/config", (route) => route.fulfill({ json: config }))
  await page.route("**/api/auth/sign-up/email", async (route) => {
    const body = JSON.parse(route.request().postData() ?? "{}") as { email?: string; name?: string }
    const email = (body.email ?? "").trim().toLowerCase()
    if (config.inviteOnly && !(options.allowlist ?? []).includes(email)) {
      return route.fulfill({ status: 403, json: { code: "INVITE_ONLY", message: INVITE_ONLY_MESSAGE } })
    }
    const existing = registered.has(email)
    registered.add(email)
    // Both a fresh sign-up and an accepted-duplicate return the same 200 shape with token: null.
    return route.fulfill({ status: 200, json: { token: null, user: { id: existing ? "u_existing" : "u_new", email, name: body.name ?? "", emailVerified: false } } })
  })
  await page.route("**/api/auth/sign-in/magic-link", (route) => route.fulfill({ status: 200, json: { status: true } }))
}

test("#1412: invite-only deployment hides Email link tab, refuses non-allowlisted sign-up, and uses conditional copy on new and duplicate sign-ups", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await signedOut(page, { magicLink: false, inviteOnly: true }, { allowlist: ["invited@example.com"] })
  await page.goto("/")
  await expect(page.getByRole("tab", { name: "Sign in" })).toBeVisible()
  await expect(page.getByRole("tab", { name: "Create account" })).toBeVisible()

  await page.getByRole("tab", { name: "Create account" }).click()
  await expect(page.getByText(/This alpha is invite-only/)).toBeVisible()
  // Config has loaded (invite-only notice is rendered): Email link tab must not exist even when hidden.
  await expect(page.getByRole("tab", { name: "Email link", includeHidden: true })).toHaveCount(0)

  // Non-allowlisted sign-up: renders server's refusal and never shows sign-up confirmation copy.
  await page.getByLabel("Name").fill("New Customer")
  await page.getByLabel("Email").fill("stranger@example.com")
  await page.getByLabel("Password").fill("a-very-long-password-1234")
  await page.getByRole("button", { name: "Create account" }).click()
  await expect(page.getByRole("alert")).toHaveText(INVITE_ONLY_MESSAGE)
  await expect(page.getByText(/Account ready/i)).toHaveCount(0)
  await expect(page.getByRole("tab", { name: "Create account" })).toHaveAttribute("aria-selected", "true")
  await shoot(page, "invite-only-refused")

  // Allowlisted sign-up succeeds with conditional, privacy-safe copy and moves to Sign in.
  await page.getByLabel("Email").fill("invited@example.com")
  await page.getByRole("button", { name: "Create account" }).click()
  await expect(page.getByRole("status")).toHaveText(SIGNED_UP_MESSAGE)
  await expect(page.getByText(/Account ready/i)).toHaveCount(0)
  await expect(page.getByRole("tab", { name: "Sign in" })).toHaveAttribute("aria-selected", "true")

  // Accepted-duplicate sign-up for the same allowlisted address shows the same conditional copy, never claiming a new account was created.
  await page.getByRole("tab", { name: "Create account" }).click()
  await page.getByLabel("Name").fill("Duplicate Attempt")
  await page.getByLabel("Email").fill("invited@example.com")
  await page.getByLabel("Password").fill("a-second-long-password-5678")
  await page.getByRole("button", { name: "Create account" }).click()
  await expect(page.getByRole("status")).toHaveText(SIGNED_UP_MESSAGE)
  await expect(page.getByText(/Account ready/i)).toHaveCount(0)
  await expect(page.getByRole("tab", { name: "Sign in" })).toHaveAttribute("aria-selected", "true")
})

test("#1412: Email link tab appears when the server reports magicLink enabled", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await signedOut(page, { magicLink: true, inviteOnly: false })
  await page.goto("/")
  const tab = page.getByRole("tab", { name: "Email link" })
  await expect(tab).toBeVisible()
  await tab.click()
  await shoot(page, "magic-link-enabled")
  await page.getByLabel("Email").fill("ada@example.com")
  await page.getByRole("button", { name: "Send link" }).click()
  await expect(page.getByRole("status")).toContainText("sign-in link is on its way")
})

const RAW_ORG_ID = "MPu3nAkCMSqDEjamX5eb9IB6or836fCS"

async function mountWorkspaceServer(page: Page, cell: "populated" | "empty" | "absent" = "populated") {
  const current = {
    id: RAW_ORG_ID, name: cell === "empty" ? "Fresh Workspace" : "Core Analytics",
    slug: "core-analytics", role: "owner", active: true, hasCell: cell !== "absent",
  }
  const doc: WorkspacesDocument = {
    workspaces: [
      current,
      { id: "org_secondary_9876", name: "Shared Workspace", slug: "shared-workspace", role: "viewer", active: false, hasCell: true },
    ],
  }

  await page.route("**/api/me", (route) => route.fulfill({
    json: {
      user: { id: "u_e2e", name: "Ada Lovelace", email: "ada@example.com" },
      organization: { id: current.id, name: current.name, role: current.role },
      environmentId: current.hasCell ? "rehearsal2" : null,
    },
  }))
  await page.route("**/api/workspaces", (route) => route.fulfill({ json: doc }))
  await page.route("**/api/host/v1/**", (route) => {
    const { pathname } = new URL(route.request().url())
    if (pathname.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: cell === "empty" ? EMPTY_BASE_DEFS : POPULATED_DEFS } })
    if (pathname.endsWith("/v1/inbox")) return route.fulfill({ json: { scanLimit: 10000, sections: [], items: [] } })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
}

test("#1414: honest switcher separates real workspaces from Examples, hides lifecycle actions, never renders raw ids, and marks the current workspace", async ({ page }) => {
  const pageErrors: string[] = []
  page.on("pageerror", (error) => pageErrors.push(error.message))
  await page.setViewportSize({ width: 1440, height: 900 })
  await mountWorkspaceServer(page)
  await page.goto("/")

  const trigger = page.locator("[data-scope]")
  await expect(trigger).toContainText("Core Analytics")
  await expect(trigger).toContainText("Live · Owner")
  // Switcher must never expose raw org id or environment id.
  await expect(trigger).not.toContainText(RAW_ORG_ID)
  await expect(trigger).not.toContainText("rehearsal2")

  // Open switcher: real workspaces appear under "Your workspaces", demo datasets separately under "Examples".
  await trigger.click()
  const menu = page.getByRole("menu")
  await expect(menu.getByText("Your workspaces")).toBeVisible()
  await expect(menu.getByText("Examples")).toBeVisible()
  const current = menu.getByRole("menuitemradio", { name: /Core Analytics/ })
  await expect(current).toHaveAttribute("aria-checked", "true")
  const shared = menu.getByRole("menuitemradio", { name: /Shared Workspace/ })
  await expect(shared).toHaveAttribute("aria-checked", "false")
  await expect(shared).toBeDisabled()
  await expect(menu.getByText("Switching workspaces is coming soon.")).toBeVisible()
  await expect(menu.getByRole("menuitem", { name: /Olist e-commerce/ })).toHaveAttribute("href", "https://github.com/Norn-cloud/norn/issues/531")
  await expect(menu).not.toContainText(RAW_ORG_ID)
  await expect(menu).not.toContainText("rehearsal2")
  // Lifecycle actions (create, rename, archive, delete, request) are not offered in this PR.
  for (const label of ["New workspace", "Request a workspace", "Rename workspace", "Archive workspace", "Delete workspace", "Archived workspaces"]) {
    await expect(menu.getByRole("menuitem", { name: new RegExp(label) })).toHaveCount(0)
  }
  await shoot(page, "switcher")

  expect(pageErrors).toEqual([])
})

test("#1414: a workspace without a cell stays current while Studio shows recorded demo data", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await mountWorkspaceServer(page, "absent")
  await page.goto("/")

  const trigger = page.locator("[data-scope]")
  await expect(trigger).toContainText("Core Analytics")
  await expect(trigger).toContainText("Recorded demo data")
  await trigger.click()

  const menu = page.getByRole("menu")
  const current = menu.getByRole("menuitemradio", { name: /Core Analytics/ })
  await expect(current).toHaveAttribute("aria-checked", "true")
  await expect(current).toContainText("No cell yet")
  await expect(menu.getByRole("menuitemradio", { name: "Recorded demo" })).toBeVisible()
  await expect(menu.getByRole("menuitemradio", { name: "Recorded demo" })).toHaveAttribute("aria-checked", "false")
  await shoot(page, "switcher-no-cell")
})

test("#1414: an empty workspace shows the Get started checklist on Home", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await mountWorkspaceServer(page, "empty")
  await page.goto("/")
  await expect(page.locator("[data-scope]")).toContainText("Fresh Workspace")
  const main = page.locator("main")
  await expect(main.getByRole("heading", { name: "Get started" })).toBeVisible()
  await expect(main.getByRole("link", { name: /Add a source/ })).toHaveAttribute("href", "/sources")
  await expect(main.getByRole("link", { name: /Model it/ })).toHaveAttribute("href", "/transforms")
  await expect(main.getByRole("link", { name: /Rules/ })).toHaveAttribute("href", "/rules")
  await expect(main.getByRole("link", { name: /Dashboard/ })).toHaveAttribute("href", "/dashboards")
  await shoot(page, "empty-home")
})

test("#1414: a slow connecting cell explains what it is waiting for and supports Retry now", async ({ page }) => {
  await page.route("**/api/me", (route) => route.fulfill({
    json: { user: { id: "u_e2e", name: "Ada", email: "ada@example.com" }, organization: { id: "org_slow", name: "Cold Workspace", role: "owner" }, environmentId: "prod" },
  }))
  await page.route("**/api/workspaces", (route) => route.fulfill({
    json: {
      workspaces: [{ id: "org_slow", name: "Cold Workspace", slug: "cold", role: "owner", active: true, hasCell: true }],
    },
  }))
  let firstRequest: Request | undefined
  let releaseFirst: (() => void) | undefined
  await page.route("**/api/host/v1/**", async (route) => {
    const url = new URL(route.request().url())
    if (url.pathname.endsWith("/v1/definitions")) {
      if (firstRequest === undefined) {
        firstRequest = route.request()
        await new Promise<void>((resolve) => { releaseFirst = resolve })
        // Retry must cancel this request while the mock still holds its response.
        if (firstRequest.failure() !== null) return
      }
      return route.fulfill({ json: { artifacts: EMPTY_BASE_DEFS } })
    }
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })

  try {
    await page.goto("/")
    await expect(page.locator("[data-scope]")).toContainText("Cold Workspace")
    const banner = page.locator('[data-connection="connecting"]')
    await expect(banner).toBeVisible({ timeout: 8_000 })
    await expect(banner).toContainText("Connecting to your workspace cell…")
    const [cancelled] = await Promise.all([
      page.waitForEvent("requestfailed", { predicate: (request) => request === firstRequest, timeout: 5_000 }),
      banner.getByRole("button", { name: "Retry now" }).click(),
    ])
    expect(cancelled.failure()?.errorText).toMatch(/abort|cancel/i)
    await expect(page.locator("[data-scope]")).toContainText("Live · Owner")
    await expect(page.getByRole("heading", { name: "Get started" })).toBeVisible()
  } finally {
    releaseFirst?.()
  }
})
