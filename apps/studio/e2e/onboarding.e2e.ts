import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import type { WorkspaceItem, WorkspaceRequestItem, WorkspacesDocument } from "../src/lib/workspaces-api.ts"

const INVITE_ONLY_MESSAGE = "This alpha is invite-only; ask your admin for an invitation."
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
  await page.route("**/api/me", (route) => route.fulfill({ status: 401, json: { error: "Sign in first." } }))
  await page.route("**/api/auth/config", (route) => route.fulfill({ json: config }))
  await page.route("**/api/auth/sign-up/email", async (route) => {
    const body = JSON.parse(route.request().postData() ?? "{}") as { email?: string; name?: string }
    const email = (body.email ?? "").trim().toLowerCase()
    if (config.inviteOnly && !(options.allowlist ?? []).includes(email)) {
      return route.fulfill({ status: 403, json: { code: "INVITE_ONLY", message: INVITE_ONLY_MESSAGE } })
    }
    return route.fulfill({ status: 200, json: { token: null, user: { id: "u_new", email, name: body.name ?? "", emailVerified: false } } })
  })
  await page.route("**/api/auth/sign-in/magic-link", (route) => route.fulfill({ status: 200, json: { status: true } }))
}

test("#1412: invite-only deployment hides Email link tab and refuses non-allowlisted sign-up without claiming Account ready", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await signedOut(page, { magicLink: false, inviteOnly: true }, { allowlist: ["invited@example.com"] })
  await page.goto("/")
  await expect(page.getByRole("tab", { name: "Sign in" })).toBeVisible()
  await expect(page.getByRole("tab", { name: "Create account" })).toBeVisible()

  await page.getByRole("tab", { name: "Create account" }).click()
  await expect(page.getByText(/This alpha is invite-only/)).toBeVisible()
  // Config has loaded (invite-only notice is rendered): Email link tab must not exist even when hidden.
  await expect(page.getByRole("tab", { name: "Email link", includeHidden: true })).toHaveCount(0)

  // Non-allowlisted sign-up: renders server's refusal and NEVER says "Account ready".
  await page.getByLabel("Name").fill("New Customer")
  await page.getByLabel("Email").fill("stranger@example.com")
  await page.getByLabel("Password").fill("a-very-long-password-1234")
  await page.getByRole("button", { name: "Create account" }).click()
  await expect(page.getByRole("alert")).toHaveText(INVITE_ONLY_MESSAGE)
  await expect(page.getByText(/Account ready/i)).toHaveCount(0)
  await expect(page.getByRole("tab", { name: "Create account" })).toHaveAttribute("aria-selected", "true")
  await shoot(page, "invite-only-refused")

  // Allowlisted sign-up succeeds and moves cleanly to Sign in.
  await page.getByLabel("Email").fill("invited@example.com")
  await page.getByRole("button", { name: "Create account" }).click()
  await expect(page.getByRole("status")).toContainText("Account ready")
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
  archived: WorkspaceItem[]
  requests: WorkspaceRequestItem[]
  cells: Map<string, unknown[]>
}

async function mountWorkspaceServer(
  page: Page,
  options: { creation: WorkspacesDocument["creation"]; initial?: Partial<MockWorkspaceState> },
) {
  const RAW_ORG_ID = "MPu3nAkCMSqDEjamX5eb9IB6or836fCS"
  const state: MockWorkspaceState = {
    workspaces: options.initial?.workspaces ?? [
      { id: RAW_ORG_ID, name: "Core Analytics", slug: "core-analytics", role: "owner", active: true, hasCell: true, archived: false, archivedAt: null, canRename: true, canArchive: true, canDelete: true },
      { id: "org_secondary_9876", name: "Shared Workspace", slug: "shared-workspace", role: "viewer", active: false, hasCell: true, archived: false, archivedAt: null, canRename: false, canArchive: false, canDelete: false },
    ],
    archived: options.initial?.archived ?? [],
    requests: options.initial?.requests ?? [],
    cells: options.initial?.cells ?? new Map<string, unknown[]>([
      [RAW_ORG_ID, POPULATED_DEFS],
      ["org_secondary_9876", POPULATED_DEFS],
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

  await page.route("**/api/workspaces**", async (route) => {
    const url = new URL(route.request().url())
    const method = route.request().method()
    const path = url.pathname.replace(/\/+$/, "")
    const body = route.request().postData() ? JSON.parse(route.request().postData()!) as Record<string, string> : {}

    if (path === "/api/workspaces" && method === "GET") {
      const doc: WorkspacesDocument = {
        workspaces: state.workspaces,
        archived: state.archived,
        creation: options.creation,
        requests: state.requests,
        planLabel: "Pro plan",
      }
      return route.fulfill({ json: doc })
    }
    if (path === "/api/workspaces" && method === "POST") {
      const id = `org_created_${state.workspaces.length + state.archived.length + 1}`
      for (const w of state.workspaces) Object.assign(w, { active: false })
      const created: WorkspaceItem = {
        id, name: body.name ?? "Untitled", slug: id, role: "owner", active: true, hasCell: true,
        archived: false, archivedAt: null, canRename: true, canArchive: true, canDelete: true,
      }
      state.workspaces = [...state.workspaces.map((w) => ({ ...w, active: false })), created]
      state.cells.set(id, EMPTY_BASE_DEFS)
      return route.fulfill({ json: { id, name: created.name } })
    }
    if (path === "/api/workspaces/requests" && method === "POST") {
      const item: WorkspaceRequestItem = {
        id: `req_${state.requests.length + 1}`,
        name: body.name ?? "",
        note: body.note ?? "",
        status: "pending",
        requesterName: "Ada Lovelace",
        requesterEmail: "ada@example.com",
        createdAt: new Date().toISOString(),
        canManage: true,
        canWithdraw: true,
      }
      state.requests = [item, ...state.requests]
      return route.fulfill({ status: 201, json: item })
    }
    const decideMatch = /^\/api\/workspaces\/requests\/([^/]+)\/decide$/.exec(path)
    if (decideMatch && method === "POST") {
      const id = decodeURIComponent(decideMatch[1]!)
      const decision = body.decision as "approved" | "dismissed"
      state.requests = state.requests.map((r) => r.id === id ? { ...r, status: decision, canManage: false, canWithdraw: false } : r)
      return route.fulfill({ json: { id, status: decision } })
    }
    const activateMatch = /^\/api\/workspaces\/([^/]+)\/activate$/.exec(path)
    if (activateMatch && method === "POST") {
      const id = decodeURIComponent(activateMatch[1]!)
      state.workspaces = state.workspaces.map((w) => ({ ...w, active: w.id === id }))
      return route.fulfill({ json: { activeOrganizationId: id } })
    }
    const archiveMatch = /^\/api\/workspaces\/([^/]+)\/archive$/.exec(path)
    if (archiveMatch && method === "POST") {
      const id = decodeURIComponent(archiveMatch[1]!)
      const target = state.workspaces.find((w) => w.id === id)
      if (!target) return route.fulfill({ status: 404, json: { code: "NOT_FOUND", message: "Not found" } })
      const remaining = state.workspaces.filter((w) => w.id !== id).map((w, i) => ({ ...w, active: i === 0 }))
      state.workspaces = remaining
      state.archived = [{ ...target, active: false, archived: true, archivedAt: new Date().toISOString(), canRename: false }, ...state.archived]
      return route.fulfill({ json: { id, archived: true, nextActiveOrganizationId: remaining[0]?.id ?? null } })
    }
    const restoreMatch = /^\/api\/workspaces\/([^/]+)\/restore$/.exec(path)
    if (restoreMatch && method === "POST") {
      const id = decodeURIComponent(restoreMatch[1]!)
      const target = state.archived.find((w) => w.id === id)
      if (!target) return route.fulfill({ status: 404, json: { code: "NOT_ARCHIVED", message: "Not archived" } })
      state.archived = state.archived.filter((w) => w.id !== id)
      state.workspaces = [...state.workspaces.map((w) => ({ ...w, active: false })), { ...target, active: true, archived: false, archivedAt: null, canRename: true }]
      return route.fulfill({ json: { id, archived: false } })
    }
    const singleMatch = /^\/api\/workspaces\/([^/]+)$/.exec(path)
    if (singleMatch && method === "PATCH") {
      const id = decodeURIComponent(singleMatch[1]!)
      state.workspaces = state.workspaces.map((w) => w.id === id ? { ...w, name: body.name ?? w.name } : w)
      return route.fulfill({ json: { id, name: body.name } })
    }
    if (singleMatch && method === "DELETE") {
      const id = decodeURIComponent(singleMatch[1]!)
      state.archived = state.archived.filter((w) => w.id !== id)
      const remaining = state.workspaces.filter((w) => w.id !== id)
      state.workspaces = remaining.map((w, i) => ({ ...w, active: remaining.some((x) => x.active) ? w.active : i === 0 }))
      return route.fulfill({ json: { id, deleted: true, nextActiveOrganizationId: state.workspaces.find((w) => w.active)?.id ?? null } })
    }
    return route.fulfill({ status: 404, json: { code: "NOT_FOUND", message: "No route" } })
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

test("#1414: switcher separates real workspaces from Examples, never renders raw ids, and supports create -> empty Home -> rename -> archive/undo -> delete", async ({ page }) => {
  const pageErrors: string[] = []
  page.on("pageerror", (error) => pageErrors.push(error.message))
  await page.setViewportSize({ width: 1440, height: 900 })
  const { RAW_ORG_ID } = await mountWorkspaceServer(page, { creation: { mode: "self-serve", allowed: true } })
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
  await expect(menu).not.toContainText(RAW_ORG_ID)
  await expect(menu).not.toContainText("rehearsal2")
  await shoot(page, "switcher")

  // Switching to a viewer workspace updates the active workspace and hides management actions per server flags.
  await menu.getByRole("menuitemradio", { name: /Shared Workspace/ }).click()
  await expect(trigger).toContainText("Shared Workspace")
  await trigger.click()
  await expect(page.getByRole("menuitem", { name: "Rename workspace" })).toHaveCount(0)
  await expect(page.getByRole("menuitem", { name: "Archive workspace" })).toHaveCount(0)
  await expect(page.getByRole("menuitem", { name: "Delete workspace" })).toHaveCount(0)
  // Switch back to Core Analytics.
  await page.getByRole("menuitemradio", { name: /Core Analytics/ }).click()
  await expect(trigger).toContainText("Core Analytics")

  // Create a new workspace from the switcher -> lands on empty Home with 4-step checklist.
  await trigger.click()
  await page.getByRole("menuitem", { name: "New workspace" }).click()
  const dialog = page.getByRole("dialog")
  await expect(dialog.getByRole("heading", { name: "New workspace" })).toBeVisible()
  await dialog.getByLabel("Workspace name").fill("North Operations")
  await dialog.getByRole("button", { name: "Create workspace" }).click()
  await expect(dialog).toBeHidden()

  await expect(trigger).toContainText("North Operations")
  await expect(page).toHaveURL(/\/$/)
  const main = page.locator("main")
  await expect(main.getByRole("heading", { name: "Get started" })).toBeVisible()
  await expect(main.getByRole("link", { name: /Add a source/ })).toBeVisible()
  await expect(main.getByRole("link", { name: /Model it/ })).toBeVisible()
  await expect(main.getByRole("link", { name: /Rules/ })).toBeVisible()
  await expect(main.getByRole("link", { name: /Dashboard/ })).toBeVisible()
  await shoot(page, "empty-home")

  // Rename the workspace.
  await trigger.click()
  await page.getByRole("menuitem", { name: "Rename workspace" }).click()
  const renameDialog = page.getByRole("dialog")
  await renameDialog.getByLabel("Workspace name").fill("North Ops Live")
  await renameDialog.getByRole("button", { name: "Save name" }).click()
  await expect(renameDialog).toBeHidden()
  await expect(trigger).toContainText("North Ops Live")

  // Archive the workspace -> Undo banner appears -> clicking Undo restores it!
  await trigger.click()
  await page.getByRole("menuitem", { name: "Archive workspace" }).click()
  await page.getByRole("dialog").getByRole("button", { name: "Archive workspace" }).click()
  await expect(trigger).toContainText("Core Analytics")
  const undoBanner = page.locator("[data-workspace-undo]")
  await expect(undoBanner).toContainText("North Ops Live")
  await shoot(page, "archive-undo")
  await undoBanner.getByRole("button", { name: "Undo" }).click()
  await expect(undoBanner).toHaveCount(0)
  await expect(trigger).toContainText("North Ops Live")

  // Archive again -> open Archived workspaces -> confirm permanent deletion (Cancel keeps it, Delete permanently removes it and clears Undo notice).
  await trigger.click()
  await page.getByRole("menuitem", { name: "Archive workspace" }).click()
  await page.getByRole("dialog").getByRole("button", { name: "Archive workspace" }).click()
  await expect(trigger).toContainText("Core Analytics")
  await expect(undoBanner).toContainText("North Ops Live")
  await trigger.click()
  await page.getByRole("menuitem", { name: /Archived workspaces/ }).click()
  const archivedDialog = page.getByRole("dialog")
  await expect(archivedDialog).toContainText("North Ops Live")
  await archivedDialog.getByRole("button", { name: "Delete" }).click()
  await expect(archivedDialog.getByRole("heading", { name: /Delete “North Ops Live” permanently/ })).toBeVisible()
  await archivedDialog.getByRole("button", { name: "Cancel" }).click()
  await expect(archivedDialog).toContainText("North Ops Live")
  await archivedDialog.getByRole("button", { name: "Delete" }).click()
  await archivedDialog.getByRole("button", { name: "Delete permanently" }).click()
  await expect(archivedDialog).toContainText("No archived workspaces.")
  await archivedDialog.getByRole("button", { name: "Close" }).click()
  await expect(undoBanner).toHaveCount(0)
  expect(pageErrors).toEqual([])
})

test("#1414: request-only deployment offers Request a workspace and lets admins review recorded requests", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await mountWorkspaceServer(page, { creation: { mode: "request" } })
  await page.goto("/")

  const trigger = page.locator("[data-scope]")
  await expect(trigger).toContainText("Core Analytics")
  await trigger.click()
  await expect(page.getByRole("menuitem", { name: "New workspace" })).toHaveCount(0)
  await page.getByRole("menuitem", { name: "Request a workspace" }).click()

  const dialog = page.getByRole("dialog")
  await dialog.getByLabel("Workspace name").fill("Regional Hub")
  await dialog.getByLabel("Note (optional)").fill("Need a clean cell for regional onboarding")
  await dialog.getByRole("button", { name: "Submit request" }).click()
  await expect(dialog.getByText("Request recorded")).toBeVisible()
  await expect(dialog.getByText("Regional Hub")).toBeVisible()
  await shoot(page, "request-workspace")

  await dialog.getByRole("button", { name: "Approve" }).click()
  await expect(dialog.getByText("approved")).toBeVisible()
})

test("#1414: a slow connecting cell explains what it is waiting for and supports Retry now", async ({ page }) => {
  await page.route("**/api/me", (route) => route.fulfill({
    json: { user: { id: "u_e2e", name: "Ada", email: "ada@example.com" }, organization: { id: "org_slow", name: "Cold Workspace", role: "owner" }, environmentId: "prod" },
  }))
  await page.route("**/api/workspaces", (route) => route.fulfill({
    json: {
      workspaces: [{ id: "org_slow", name: "Cold Workspace", slug: "cold", role: "owner", active: true, hasCell: true, archived: false, archivedAt: null, canRename: true, canArchive: true, canDelete: true }],
      archived: [], creation: { mode: "self-serve", allowed: true }, requests: [], planLabel: null,
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
