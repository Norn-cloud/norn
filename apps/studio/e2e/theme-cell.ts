import type { BrowserContext, Route } from "@playwright/test"
import { parseStudioTheme } from "../../../tools/norn-ts/src/builder/theme.ts"

/**
 * A stand-in for the cell's definitions route as it serves themes (themes as data): each workspace
 * keeps its own `workspace.theme` definition, `:install` admits a theme with the checker's rules (the
 * shared theme gate) and refuses with its findings, `:retire` brings back the default, and the read
 * serves what is installed. Two workspaces, and `/api/workspaces/{id}/activate` switches between them.
 * Every other host route answers 404, so the rest of Studio renders from its recording.
 */
export interface ThemeCellWorkspace {
  readonly id: string
  readonly name: string
  readonly environmentId: string
}

interface Installed { version: number; payload: unknown; retired: boolean }

export async function themeCell(context: BrowserContext, workspaces: readonly ThemeCellWorkspace[]) {
  let active = workspaces[0]!.id
  const themes = new Map<string, Installed>()
  const installs: { readonly orgId: string | null; readonly expectedVersion: number | undefined; readonly admitted: boolean }[] = []
  const current = () => workspaces.find((workspace) => workspace.id === active)!

  await context.route("**/api/me", (route) => {
    const workspace = current()
    return route.fulfill({ json: { user: { id: "u_theme", name: "Theme Author", email: "author@example.com" }, organization: { id: workspace.id, name: workspace.name, role: "owner" }, environmentId: workspace.environmentId } })
  })
  await context.route("**/api/workspaces**", (route) => {
    const path = new URL(route.request().url()).pathname.replace(/\/+$/, "")
    if (path === "/api/workspaces" && route.request().method() === "GET") {
      return route.fulfill({ json: { workspaces: workspaces.map((workspace) => ({ id: workspace.id, name: workspace.name, slug: workspace.id, role: "owner", active: workspace.id === active, hasCell: true })) } })
    }
    const activate = /^\/api\/workspaces\/([^/]+)\/activate$/.exec(path)
    if (activate && route.request().method() === "POST") {
      active = decodeURIComponent(activate[1]!)
      return route.fulfill({ json: { activeOrganizationId: active } })
    }
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
  await context.route("**/api/host/**", (route: Route) => {
    const request = route.request()
    const url = new URL(request.url())
    const orgId = url.searchParams.get("orgId")
    const tail = url.pathname.split("/v1/definitions")[1]
    if (orgId === null || tail === undefined) return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
    const stored = themes.get(orgId)
    if (tail === "/workspace.theme" && request.method() === "GET") {
      if (stored === undefined || stored.retired) return route.fulfill({ status: 404, json: { code: "definition.not_found", message: "No such definition." } })
      return route.fulfill({ json: { id: "workspace.theme", kind: "theme", version: stored.version, lifecycle: "active", payload: stored.payload } })
    }
    const body = JSON.parse(request.postData() ?? "null") as { kind?: string; id?: string; payload?: unknown; expectedVersion?: number } | null
    if (tail === ":install" && request.method() === "POST" && body?.kind === "theme") {
      if (body.expectedVersion !== undefined && body.expectedVersion !== stored?.version) {
        return route.fulfill({ status: 409, json: { code: "definition.version_conflict", message: "Someone installed a newer version." } })
      }
      const admitted = parseStudioTheme(body.payload)
      installs.push({ orgId, expectedVersion: body.expectedVersion, admitted: admitted.ok })
      if (!admitted.ok) {
        return route.fulfill({
          status: 422,
          json: { code: "definition.refused", message: "The checker refused this theme.", diagnostics: admitted.issues.map((issue) => ({ code: issue.code, message: `The checker: ${issue.message}`, span: { pointer: `/payload/${issue.path}` } })) },
        })
      }
      const version = (stored?.version ?? 0) + 1
      themes.set(orgId, { version, payload: admitted.theme, retired: false })
      return route.fulfill({ json: { installed: true, id: "workspace.theme", version, receipt: { receiptHash: "e".repeat(64), commitSeq: version, recordedAtMs: Date.now() } } })
    }
    if (tail === "/workspace.theme:retire" && request.method() === "POST") {
      if (stored === undefined || stored.retired || stored.version !== body?.expectedVersion) {
        return route.fulfill({ status: 409, json: { code: "definition.version_conflict", message: "Someone installed a newer version." } })
      }
      stored.retired = true
      stored.version += 1
      return route.fulfill({ json: { installed: true, id: "workspace.theme", version: stored.version, receipt: { receiptHash: "f".repeat(64), commitSeq: stored.version, recordedAtMs: Date.now() } } })
    }
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })

  return {
    installs,
    /** Serve a theme as installed, bypassing admission: a theme admitted under rules that later changed. */
    serveUnchecked(orgId: string, payload: unknown) {
      themes.set(orgId, { version: (themes.get(orgId)?.version ?? 0) + 1, payload, retired: false })
    },
  }
}
