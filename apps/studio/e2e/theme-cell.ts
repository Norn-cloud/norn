import type { BrowserContext, Route } from "@playwright/test"
import { THEME_KIND, WORKSPACE_THEME_ID, parseStudioTheme } from "../../../tools/norn-ts/src/builder/theme.ts"

/**
 * A stand-in for the cell's real kernel definition routes as they serve themes (themes as data):
 * - `GET /v1/definitions` serves `{ admitted: true, version, artifacts }`
 * - `POST /v1/cell/definitions:plan` and `:apply` admit `kind: "theme"` envelopes with the shared
 *   theme gate, enforce compare-and-set `expectedVersion` (`409 defs.stale`), and support resetting
 *   via `lifecycle: "deprecated"`.
 * Two workspaces, and `/api/workspaces/{id}/activate` switches between them.
 */
export interface ThemeCellWorkspace {
  readonly id: string
  readonly name: string
  readonly environmentId: string
}

interface StoredCell {
  version: number
  artifacts: Map<string, Record<string, unknown>>
}

export async function themeCell(context: BrowserContext, workspaces: readonly ThemeCellWorkspace[]) {
  let active = workspaces[0]!.id
  const cells = new Map<string, StoredCell>(
    workspaces.map((ws) => [ws.id, { version: 1, artifacts: new Map() }]),
  )
  const applies: { readonly orgId: string; readonly expectedVersion: number | undefined; readonly admitted: boolean; readonly lifecycle: string }[] = []
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
    const cell = orgId === null ? undefined : cells.get(orgId)
    if (orgId === null || cell === undefined) return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
    const hostPath = url.pathname.replace(/^\/api\/host/, "")

    if (hostPath === "/v1/definitions" && request.method() === "GET") {
      return route.fulfill({
        json: {
          admitted: true,
          version: cell.version,
          layoutRevision: 0,
          layout: { nodes: {}, edges: {} },
          layoutDiagnostics: null,
          receipt: { admitted: true },
          receiptSha256: "e".repeat(64),
          diagnostics: null,
          registry: [],
          baseArtifactIds: [],
          artifacts: [...cell.artifacts.values()],
          builtins: [],
          objectCounts: {},
        },
      })
    }

    if ((hostPath === "/v1/cell/definitions:plan" || hostPath === "/v1/cell/definitions:apply") && request.method() === "POST") {
      const isApply = hostPath === "/v1/cell/definitions:apply"
      const body = JSON.parse(request.postData() ?? "null") as { expectedVersion?: number; artifacts?: readonly Record<string, unknown>[] } | null
      const submitted = body?.artifacts ?? []
      if (submitted.length === 0) {
        return route.fulfill({ status: 400, json: { code: "request.invalid", message: "a definitions change names at least one artifact" } })
      }
      const findings: { code: string; severity: string; message: string; span: { pointer: string } }[] = []
      for (let i = 0; i < submitted.length; i += 1) {
        const artifact = submitted[i]!
        if (artifact.kind === THEME_KIND) {
          const admitted = parseStudioTheme(artifact.payload)
          if (!admitted.ok) {
            for (const issue of admitted.issues) {
              findings.push({
                code: issue.code,
                severity: "error",
                message: `The checker: ${issue.message}`,
                span: { pointer: `/artifacts/${i}/payload/${issue.path}` },
              })
            }
          }
        }
      }
      const firstLifecycle = typeof submitted[0]?.lifecycle === "string" ? submitted[0].lifecycle : "active"
      if (isApply) {
        applies.push({ orgId, expectedVersion: body?.expectedVersion, admitted: findings.length === 0, lifecycle: firstLifecycle })
      }
      if (findings.length > 0) {
        return route.fulfill({
          status: 422,
          json: { code: "defs.refused", message: "the submitted definitions are not admissible over the cell's active definitions", diagnostics: findings },
        })
      }
      if (isApply) {
        if (typeof body?.expectedVersion !== "number") {
          return route.fulfill({ status: 400, json: { code: "request.invalid", message: "an apply names the expectedVersion its plan was made against" } })
        }
        if (body.expectedVersion !== cell.version) {
          return route.fulfill({ status: 409, json: { code: "defs.stale", message: "the definitions changed since this revision was planned" } })
        }
        const previousVersion = cell.version
        for (const artifact of submitted) {
          const id = typeof artifact.artifactId === "string" ? artifact.artifactId : WORKSPACE_THEME_ID
          cell.artifacts.set(id, artifact)
        }
        cell.version += 1
        return route.fulfill({
          json: {
            outcome: "installed",
            definitionsVersion: cell.version,
            previousVersion,
            receiptSha256: "e".repeat(64),
            commitSeq: cell.version,
            plan: { baseVersion: previousVersion, nextVersion: cell.version, unchanged: false, added: [], changed: [], same: [], newObjectTypes: [], checkedObjects: 0, diagnostics: [], receiptSha256: "e".repeat(64) },
          },
        })
      }
      return route.fulfill({
        json: { baseVersion: cell.version, nextVersion: cell.version + 1, unchanged: false, added: [], changed: [], same: [], newObjectTypes: [], checkedObjects: 0, diagnostics: [], receiptSha256: "e".repeat(64) },
      })
    }

    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })

  return {
    applies,
    /** Serve a theme as installed, bypassing admission: a theme admitted under rules that later changed. */
    serveUnchecked(orgId: string, payload: unknown) {
      const cell = cells.get(orgId)
      if (!cell) return
      cell.version += 1
      cell.artifacts.set(WORKSPACE_THEME_ID, {
        irVersion: "0.1",
        artifactId: WORKSPACE_THEME_ID,
        kind: THEME_KIND,
        namespace: "workspace",
        name: "Unchecked",
        version: `0.${cell.version}.0`,
        lifecycle: "active",
        scope: { orgId, environmentId: workspaces.find((w) => w.id === orgId)?.environmentId ?? "env" },
        dependencies: [],
        provenance: { source: "authoring_sdk" },
        payload,
      })
    },
  }
}
