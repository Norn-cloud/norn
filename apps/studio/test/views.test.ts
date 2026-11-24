import { expect, mock, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRoute, createRouter } from "@tanstack/react-router"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { createElement } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { catalogOf } from "../src/lib/catalog.ts"
import type { Fetch } from "../src/lib/cell-read.ts"
import { StudioScopeError, type StudioScope } from "../src/lib/definitions.ts"
import { readDocument } from "../src/lib/exact.ts"
import { EMPTY_QUERY, recordsQueryParams } from "../src/lib/records-query.ts"
import type { RecordsRead } from "../src/lib/records-source.ts"
import type { Artifact, ObjectPage } from "../src/lib/transport.ts"
import {
  cellViews,
  cleanView,
  duplicateView,
  newViewId,
  saveViewDraft,
  sortKeys,
  sortText,
  viewField,
  WORKSPACE_CHANGED_MESSAGE,
  type SavedView,
} from "../src/lib/views.ts"

mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { RecordsGrid } = await import("../src/grid/records-grid.tsx")
const { ViewEditor } = await import("../src/views.tsx")

const SCOPE_A: StudioScope = { orgId: "org_a", environmentId: "prod" }
const SCOPE_B: StudioScope = { orgId: "org_b", environmentId: "prod" }
const SCOPE_A_STAGING: StudioScope = { orgId: "org_a", environmentId: "staging" }

const SAMPLE_VIEW: SavedView = {
  title: "Items needing review",
  description: "Open items under threshold.",
  space: "Operations",
  objectType: "demo.Record",
  filter: { all: [{ property: "score", op: "lt", value: "60" }] },
  sort: "score:asc",
  columns: ["score", "stage"],
}

test("views helpers normalize payloads, preserve space and column order, duplicate within length bound and map pointers", () => {
  const draft: SavedView = {
    title: "  Items needing review  ",
    description: "  ",
    space: "  Operations  ",
    objectType: "demo.Record",
    filter: { all: [{ property: "score", op: "lt", value: "60" }] },
    sort: sortText([{ property: "score", direction: "asc" }]),
    columns: ["score", "stage"],
  }
  const cleaned = cleanView(draft)
  expect(cleaned.title).toBe("Items needing review")
  expect(cleaned.description).toBeUndefined()
  expect(cleaned.space).toBe("Operations")
  expect(cleaned.columns).toEqual(["score", "stage"])
  expect(sortKeys(cleaned.sort)).toEqual([{ property: "score", direction: "asc" }])

  const copy = duplicateView(cleaned)
  expect(copy.title).toBe("Items needing review (copy)")
  expect(copy.space).toBe("Operations")
  expect(copy.columns).toEqual(["score", "stage"])

  // A 120-char title keeps the " (copy)" suffix within the 120-char server limit.
  const longCopy = duplicateView({ ...cleaned, title: "A".repeat(120) })
  expect([...longCopy.title].length).toBeLessThanOrEqual(120)
  expect(longCopy.title.endsWith(" (copy)")).toBe(true)

  expect(newViewId("demo.Record", "Needs help!", () => "abcd1234")).toBe("studio.view.demo_record.needs_help_abcd1234")
  expect(viewField("/payload/columns/0")).toBe("columns")
  expect(viewField("/space")).toBe("space")
  expect(viewField("/filter/all/0/value")).toBe("filter")
  expect(viewField("/rowActions/0")).toBeNull()
  expect(viewField("/unknown")).toBeNull()
})

test("views list and get pin one scope, attach loaded scope, skip retired defs, and propagate non-404 errors", async () => {
  let activeScope: StudioScope = SCOPE_A
  const seenScopes: string[] = []
  const fetcher: Fetch = async (input) => {
    const url = String(input)
    if (url === "/api/me") {
      return new Response(JSON.stringify({ organization: { id: activeScope.orgId }, environmentId: activeScope.environmentId }), { status: 200 })
    }
    const parsed = new URL(url, "https://studio.test")
    seenScopes.push(`${parsed.searchParams.get("orgId")}/${parsed.searchParams.get("environmentId")}`)
    if (parsed.pathname.endsWith("/v1/definitions:list")) {
      activeScope = SCOPE_B
      return new Response(JSON.stringify({
        items: [{ id: "studio.view.a", updatedAtMs: 1 }, { id: "studio.view.retired", updatedAtMs: 2 }],
        cursor: null,
      }), { status: 200 })
    }
    if (parsed.pathname.endsWith("/v1/definitions/studio.view.a")) {
      return new Response(JSON.stringify({ id: "studio.view.a", version: 1, lifecycle: "active", payload: SAMPLE_VIEW }), { status: 200 })
    }
    if (parsed.pathname.endsWith("/v1/definitions/studio.view.retired")) {
      return new Response(JSON.stringify({ id: "studio.view.retired", version: 2, lifecycle: "retired", payload: SAMPLE_VIEW }), { status: 200 })
    }
    return new Response(JSON.stringify({ code: "host.unavailable", message: "down" }), { status: 503 })
  }
  const store = cellViews(fetcher)
  const listed = await store.listAll()
  expect(listed).toHaveLength(1)
  expect(listed[0]!.scope).toEqual(SCOPE_A)
  expect(seenScopes).toEqual(["org_a/prod", "org_a/prod", "org_a/prod"])

  // Direct get with explicit pinnedScope uses that scope without reading /api/me.
  const loaded = await store.get("studio.view.a", SCOPE_A)
  expect(loaded?.scope).toEqual(SCOPE_A)
  expect(await store.get("studio.view.retired", SCOPE_A)).toBeUndefined()

  // A 503 on per-id definition GET throws instead of returning an empty list.
  const failingFetch: Fetch = async (input) => {
    const url = String(input)
    if (url === "/api/me") return new Response(JSON.stringify({ organization: { id: "org_a" }, environmentId: "prod" }), { status: 200 })
    if (url.includes("/v1/definitions:list")) return new Response(JSON.stringify({ items: [{ id: "studio.view.a" }], cursor: null }), { status: 200 })
    return new Response(JSON.stringify({ code: "host.error", message: "service unavailable" }), { status: 503 })
  }
  await expect(cellViews(failingFetch).listAll()).rejects.toThrow()
})

test("view writes send explicitly to their bound scope and refuse when the active workspace changed", async () => {
  let activeScope: StudioScope | null = SCOPE_A
  const hostCalls: { readonly path: string; readonly orgId: string | null; readonly environmentId: string | null }[] = []
  const fetcher: Fetch = async (input) => {
    const url = String(input)
    if (url === "/api/me") {
      return new Response(JSON.stringify({
        organization: activeScope ? { id: activeScope.orgId } : null,
        environmentId: activeScope ? activeScope.environmentId : null,
      }), { status: 200 })
    }
    const parsed = new URL(url, "https://studio.test")
    hostCalls.push({
      path: parsed.pathname,
      orgId: parsed.searchParams.get("orgId"),
      environmentId: parsed.searchParams.get("environmentId"),
    })
    if (parsed.pathname.endsWith(":admit")) {
      return new Response(JSON.stringify({ admitted: true, restatement: "Saved view.", diagnostics: [] }), { status: 200 })
    }
    return new Response(JSON.stringify({ installed: true, id: "studio.view.v1", version: 2 }), { status: 200 })
  }
  const store = cellViews(fetcher)

  // Matching scope dispatches explicitly to SCOPE_A.
  const admitted = await store.admit("studio.view.v1", SAMPLE_VIEW, SCOPE_A)
  expect(admitted.admitted).toBe(true)
  const saved = await store.save("studio.view.v1", SAMPLE_VIEW, 1, SCOPE_A)
  expect(saved).toEqual({ saved: true, id: "studio.view.v1", version: 2 })
  const retired = await store.retire("studio.view.v1", 2, SCOPE_A)
  expect(retired).toEqual({ saved: true, id: "studio.view.v1", version: 2 })
  expect(hostCalls).toEqual([
    { path: "/api/host/v1/definitions:admit", orgId: "org_a", environmentId: "prod" },
    { path: "/api/host/v1/definitions:install", orgId: "org_a", environmentId: "prod" },
    { path: "/api/host/v1/definitions/studio.view.v1:retire", orgId: "org_a", environmentId: "prod" },
  ])

  // Switching org, switching environment, or losing the cell refuses before any host request is sent.
  hostCalls.length = 0
  for (const next of [SCOPE_B, SCOPE_A_STAGING, null] as const) {
    activeScope = next
    await expect(store.save("studio.view.v1", SAMPLE_VIEW, 1, SCOPE_A)).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
    await expect(store.retire("studio.view.v1", 1, SCOPE_A)).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
    await expect(store.admit("studio.view.v1", SAMPLE_VIEW, SCOPE_A)).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
    await expect(store.currentVersion("studio.view.v1", SCOPE_A)).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  }
  expect(hostCalls).toHaveLength(0)
})

test("overwrite after a version conflict sends the confirmed current revision as expectedVersion (never omitted)", async () => {
  let serverVersion = 2
  let activeScope: StudioScope = SCOPE_A
  const sentExpectedVersions: (number | undefined)[] = []

  const fetcher: Fetch = async (input, init) => {
    const url = String(input)
    if (url === "/api/me") {
      return new Response(JSON.stringify({ organization: { id: activeScope.orgId }, environmentId: activeScope.environmentId }), { status: 200 })
    }
    const parsed = new URL(url, "https://studio.test")
    if (parsed.pathname.endsWith("/v1/definitions:install")) {
      const body = JSON.parse(String(init?.body ?? "{}")) as { expectedVersion?: number; id: string }
      sentExpectedVersions.push(body.expectedVersion)
      // Matches crates/norn-api/src/definition_routes.rs:1376: omitting expectedVersion on an existing definition conflicts!
      if (body.expectedVersion !== serverVersion) {
        return new Response(JSON.stringify({ code: "definition.version_conflict", message: "Version conflict." }), { status: 409 })
      }
      serverVersion += 1
      return new Response(JSON.stringify({ installed: true, id: body.id, version: serverVersion }), { status: 200 })
    }
    if (parsed.pathname.endsWith("/v1/definitions/studio.view.v1")) {
      return new Response(JSON.stringify({ id: "studio.view.v1", version: serverVersion, lifecycle: "active", payload: SAMPLE_VIEW }), { status: 200 })
    }
    return new Response(JSON.stringify({ code: "not_found" }), { status: 404 })
  }
  const store = cellViews(fetcher)

  // 1. Stale save (expectedVersion: 1 while server is at 2) conflicts and confirms revision 2.
  const first = await saveViewDraft(store, {
    id: "studio.view.v1",
    existing: true,
    view: SAMPLE_VIEW,
    expectedVersion: 1,
    scope: SCOPE_A,
  })
  expect(first.result).toEqual({ saved: false, conflict: true })
  expect(first.conflictVersion).toBe(2)

  // 2. If another edit lands before overwrite (server moves to 3), overwrite with 2 re-conflicts and refreshes to 3.
  serverVersion = 3
  const raced = await saveViewDraft(store, {
    id: "studio.view.v1",
    existing: true,
    view: SAMPLE_VIEW,
    expectedVersion: first.conflictVersion!,
    scope: SCOPE_A,
  })
  expect(raced.result).toEqual({ saved: false, conflict: true })
  expect(raced.conflictVersion).toBe(3)

  // 3. Overwrite with confirmed version 3 succeeds with version 4, and never omits expectedVersion.
  const overwritten = await saveViewDraft(store, {
    id: "studio.view.v1",
    existing: true,
    view: SAMPLE_VIEW,
    expectedVersion: raced.conflictVersion!,
    scope: SCOPE_A,
  })
  expect(overwritten.result).toEqual({ saved: true, id: "studio.view.v1", version: 4 })
  expect(overwritten.conflictVersion).toBeNull()
  expect(sentExpectedVersions).toEqual([1, 2, 3])

  // 4. If the active workspace changed before overwrite, refuse with WORKSPACE_CHANGED_MESSAGE and send nothing.
  activeScope = SCOPE_B
  await expect(saveViewDraft(store, {
    id: "studio.view.v1",
    existing: true,
    view: SAMPLE_VIEW,
    expectedVersion: 4,
    scope: SCOPE_A,
  })).rejects.toThrow(StudioScopeError)
  expect(sentExpectedVersions).toEqual([1, 2, 3])
})

test("conflict without a readable current revision leaves conflictVersion null so overwrite is withheld", async () => {
  const fetcher: Fetch = async (input) => {
    const url = String(input)
    if (url === "/api/me") {
      return new Response(JSON.stringify({ organization: { id: SCOPE_A.orgId }, environmentId: SCOPE_A.environmentId }), { status: 200 })
    }
    if (url.includes(":install")) {
      return new Response(JSON.stringify({ code: "definition.version_conflict", message: "Version conflict." }), { status: 409 })
    }
    return new Response(JSON.stringify({ code: "host.error", message: "unavailable" }), { status: 503 })
  }
  const outcome = await saveViewDraft(cellViews(fetcher), {
    id: "studio.view.v1",
    existing: true,
    view: SAMPLE_VIEW,
    expectedVersion: 1,
    scope: SCOPE_A,
  })
  expect(outcome.result).toEqual({ saved: false, conflict: true })
  expect(outcome.conflictVersion).toBeNull()
})

test("ViewEditor alerts when active scope diverges from the loaded draft scope and renders no bulk/row action controls", async () => {
  const raw = readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/three-systems.definitions.json"), "utf8")
  const catalog = catalogOf(readDocument<readonly Artifact[]>(raw, "recorded").document)
  const concept = catalog.byName.get("canon.Item")
  if (concept === undefined) throw new Error("canon.Item is not in the recorded catalog")

  const renderEditor = async (activeScope: StudioScope) => {
    const client = new QueryClient()
    const root = createRootRoute()
    const router = createRouter({
      routeTree: root.addChildren([createRoute({
        getParentRoute: () => root,
        path: "/",
        component: () => createElement(ViewEditor, {
          concepts: [concept],
          stored: { id: "studio.view.canon_item.a", version: 1, scope: SCOPE_A, view: { ...SAMPLE_VIEW, objectType: concept.name, columns: [] } },
          activeScope,
          cellAvailable: true,
          onSaved: () => {},
        }),
      })]),
      history: createMemoryHistory({ initialEntries: ["/"] }),
    })
    await router.load()
    return renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(RouterProvider, { router })))
  }

  const same = await renderEditor(SCOPE_A)
  expect(same).not.toContain(WORKSPACE_CHANGED_MESSAGE)
  expect(same).not.toContain("Offered actions")
  expect(same).not.toContain("Bulk action")

  const mismatched = await renderEditor(SCOPE_B)
  expect(mismatched).toContain(WORKSPACE_CHANGED_MESSAGE)
})

test("RecordsGrid preserves a saved view's custom column order instead of reverting to catalog order", async () => {
  const raw = readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/three-systems.definitions.json"), "utf8")
  const catalog = catalogOf(readDocument<readonly Artifact[]>(raw, "recorded").document)
  const hidden = new Set(["id", "label", "display_name"])
  const concept = catalog.concepts.find((c) => c.fields.filter(({ id }) => !hidden.has(id)).length >= 2)
  if (concept === undefined) throw new Error("no recorded concept with >= 2 visible fields")
  const visibleFields = concept.fields.filter(({ id }) => !hidden.has(id))
  const reversed = [visibleFields[1]!.id, visibleFields[0]!.id]
  const page: RecordsRead = {
    kind: "page",
    read: readDocument<ObjectPage>(JSON.stringify({
      objectType: concept.name,
      records: [{ objectType: concept.name, id: "item-1", scope: "s", version: 1, values: { [reversed[0]!]: "B", [reversed[1]!]: "A" }, withheld: false }],
      quarantineCount: 0,
      scannedThrough: null,
      query: { sort: [], filter: null },
      next: null,
      total: { count: 1, approximate: false },
    }), "live"),
    origin: "live",
    served: { sort: [], filter: null },
    next: undefined,
    total: { count: 1, approximate: false },
  }
  const client = new QueryClient()
  client.setQueryData(["records", concept.name, recordsQueryParams(EMPTY_QUERY).toString()], page)
  const root = createRootRoute()
  const router = createRouter({
    routeTree: root.addChildren([createRoute({
      getParentRoute: () => root,
      path: "/",
      component: () => createElement(RecordsGrid, { concept, initial: { columns: reversed }, read: async () => page }),
    })]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  })
  await router.load()
  const html = renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(RouterProvider, { router })))
  const firstLabel = reversed[0]!.replaceAll("_", " ")
  const secondLabel = reversed[1]!.replaceAll("_", " ")
  const firstIdx = html.indexOf(`>${firstLabel}<`)
  const secondIdx = html.indexOf(`>${secondLabel}<`)
  expect(firstIdx).toBeGreaterThan(-1)
  expect(secondIdx).toBeGreaterThan(firstIdx)
})
