import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import type { WorkspaceItem, WorkspacesDocument } from "../src/lib/workspaces-api.ts"

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

interface MockWorkspaceState {
  workspaces: WorkspaceItem[]
  cells: Map<string, unknown[]>
}

async function mountWorkspaceServer(page: Page, initial?: Partial<MockWorkspaceState>) {
  const RAW_ORG_ID = "MPu3nAkCMSqDEjamX5eb9IB6or836fCS"
  const state: MockWorkspaceState = {
    workspaces: initial?.workspaces ?? [
      { id: RAW_ORG_ID, name: "Core Analytics", slug: "core-analytics", role: "owner", active: true, hasCell: true },
      { id: "org_secondary_9876", name: "Shared Workspace", slug: "shared-workspace", role: "viewer", active: false, hasCell: true },
      { id: "org_empty_4321", name: "Fresh Workspace", slug: "fresh-workspace", role: "owner", active: false, hasCell: true },
    ],
    cells: initial?.cells ?? new Map<string, unknown[]>([
      [RAW_ORG_ID, POPULATED_DEFS],
      ["org_secondary_9876", POPULATED_DEFS],
      ["org_empty_4321", EMPTY_BASE_DEFS],
    ]),
  }

  const active = () => state.workspaces.find((w) => w.active) ?? null

  await page.route("**/api/me", (route) => {
    const cur = active()
    return route.fulfill({
      json: {
        user: { id: "u_e2e", name: "Ada Lovelace", email: "ada@example.com" },
        organization: cur ? { id: cur.id, name: cur.name, role: cur.role } : null,
        environmentId: cur?.hasCell ? "rehearsal2" : null,
      },
    })
  })

  await page.route("**/api/workspaces", async (route) => {
    if (route.request().method() === "GET") {
      const doc: WorkspacesDocument = { workspaces: state.workspaces, planLabel: "Pro plan" }
      return route.fulfill({ json: doc })
    }
    return route.fulfill({ status: 405, json: { code: "METHOD_NOT_ALLOWED", message: "GET only" } })
  })

  await page.route("**/api/auth/organization/set-active", async (route) => {
    if (route.request().method() !== "POST") return route.fulfill({ status: 405 })
    const body = JSON.parse(route.request().postData() ?? "{}") as { organizationId?: string }
    const target = state.workspaces.find((w) => w.id === body.organizationId)
    if (!target) return route.fulfill({ status: 403, json: { code: "USER_IS_NOT_A_MEMBER_OF_THE_ORGANIZATION", message: "User is not a member of this organization" } })
    state.workspaces = state.workspaces.map((w) => ({ ...w, active: w.id === target.id }))
    return route.fulfill({ json: { id: target.id, name: target.name, slug: target.slug } })
  })

  await page.route("**/api/host/v1/**", (route) => {
    const url = new URL(route.request().url())
    const orgId = url.searchParams.get("orgId") ?? ""
    if (url.pathname.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: state.cells.get(orgId) ?? [] } })
    if (url.pathname.endsWith("/v1/inbox")) return route.fulfill({ json: { scanLimit: 10000, sections: [], items: [] } })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })

  return { state, RAW_ORG_ID }
}

test("#1414: honest switcher separates real workspaces from Examples, hides lifecycle actions, never renders raw ids, and switches cleanly", async ({ page }) => {
  const pageErrors: string[] = []
  page.on("pageerror", (error) => pageErrors.push(error.message))
  await page.setViewportSize({ width: 1440, height: 900 })
  const { RAW_ORG_ID } = await mountWorkspaceServer(page)
  await page.goto("/")

  const trigger = page.locator("[data-scope]")
  await expect(trigger).toContainText("Core Analytics")
  await expect(trigger).toContainText("Live · Pro plan")
  // Switcher must never expose raw org id or environment id.
  await expect(trigger).not.toContainText(RAW_ORG_ID)
  await expect(trigger).not.toContainText("rehearsal2")

  // Open switcher: real workspaces appear under "Your workspaces", demo datasets separately under "Examples".
  await trigger.click()
  const menu = page.getByRole("menu")
  await expect(menu.getByText("Your workspaces")).toBeVisible()
  await expect(menu.getByText("Examples")).toBeVisible()
  await expect(menu.getByRole("menuitemradio", { name: /Core Analytics/ })).toBeVisible()
  await expect(menu.getByRole("menuitemradio", { name: /Shared Workspace/ })).toBeVisible()
  await expect(menu.getByRole("menuitemradio", { name: /Fresh Workspace/ })).toBeVisible()
  await expect(menu).not.toContainText(RAW_ORG_ID)
  await expect(menu).not.toContainText("rehearsal2")
  // Lifecycle actions (create, rename, archive, delete, request) are not offered in this PR.
  for (const label of ["New workspace", "Request a workspace", "Rename workspace", "Archive workspace", "Delete workspace", "Archived workspaces"]) {
    await expect(menu.getByRole("menuitem", { name: new RegExp(label) })).toHaveCount(0)
  }
  await shoot(page, "switcher")

  // Switching to Shared Workspace updates the active workspace cleanly.
  await menu.getByRole("menuitemradio", { name: /Shared Workspace/ }).click()
  await expect(trigger).toContainText("Shared Workspace")

  // Switching to an empty workspace lands on Home with the 4-step Get started checklist.
  await trigger.click()
  await page.getByRole("menuitemradio", { name: /Fresh Workspace/ }).click()
  await expect(trigger).toContainText("Fresh Workspace")
  await expect(page).toHaveURL(/\/$/)
  const main = page.locator("main")
  await expect(main.getByRole("heading", { name: "Get started" })).toBeVisible()
  await expect(main.getByRole("link", { name: /Add a source/ })).toHaveAttribute("href", "/sources")
  await expect(main.getByRole("link", { name: /Model it/ })).toHaveAttribute("href", "/transforms")
  await expect(main.getByRole("link", { name: /Rules/ })).toHaveAttribute("href", "/rules")
  await expect(main.getByRole("link", { name: /Dashboard/ })).toHaveAttribute("href", "/dashboards")
  await shoot(page, "empty-home")

  expect(pageErrors).toEqual([])
})

test("#1414: a slow connecting cell explains what it is waiting for and supports Retry now", async ({ page }) => {
  await page.route("**/api/me", (route) => route.fulfill({
    json: { user: { id: "u_e2e", name: "Ada", email: "ada@example.com" }, organization: { id: "org_slow", name: "Cold Workspace", role: "owner" }, environmentId: "prod" },
  }))
  await page.route("**/api/workspaces", (route) => route.fulfill({
    json: {
      workspaces: [{ id: "org_slow", name: "Cold Workspace", slug: "cold", role: "owner", active: true, hasCell: true }],
      planLabel: null,
    },
  }))
  let attempts = 0
  let releaseFirst: (() => void) | undefined
  await page.route("**/api/host/v1/**", async (route) => {
    const url = new URL(route.request().url())
    if (url.pathname.endsWith("/v1/definitions")) {
      attempts += 1
      if (attempts === 1) {
        await new Promise<void>((resolve) => { releaseFirst = resolve })
        return route.abort()
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
    await banner.getByRole("button", { name: "Retry now" }).click()
    // Prove Retry now actually dispatched a replacement probe before releasing the stalled first request.
    await expect.poll(() => attempts, { timeout: 5_000 }).toBeGreaterThanOrEqual(2)
    releaseFirst?.()
    await expect(page.locator("[data-scope]")).toContainText("Live · Owner")
    await expect(page.getByRole("heading", { name: "Get started" })).toBeVisible()
  } finally {
    releaseFirst?.()
  }
})
