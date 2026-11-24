import { expect, mock, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRoute, createRouter } from "@tanstack/react-router"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { createElement } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { catalogOf } from "../src/lib/catalog.ts"
import type { Fetch } from "../src/lib/cell-read.ts"
import { CellUnavailable, EXPECTED_ENV_HEADER, EXPECTED_ORG_HEADER, readAtScope, StudioScopeError, type StudioScope } from "../src/lib/definitions.ts"
import { history } from "../src/lib/editor.ts"
import { readDocument } from "../src/lib/exact.ts"
import { facetsQuery } from "../src/lib/facets.ts"
import { EMPTY_QUERY, recordsQueryParams, recordsQueryPrefix } from "../src/lib/records-query.ts"
import { readRecordsAtScope, type RecordsRead } from "../src/lib/records-source.ts"
import { init, update } from "../src/lib/screen/call.ts"
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
  viewPreviewQueryKey,
  WORKSPACE_CHANGED_MESSAGE,
  type SavedView,
  type StoredView,
  type ViewSaved,
} from "../src/lib/views.ts"
import { WITHHELD_ROW } from "../src/grid/cells.tsx"
import { sessionKey } from "../src/actions/kept-drafts.ts"
import type { ViewsBarNotice } from "../src/grid/views-bar.tsx"

mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { invalidateActionReads } = await import("../src/actions/action-sheet.tsx")
const { RecordsGrid, visibleDrawerRow } = await import("../src/grid/records-grid.tsx")
const { deletedViewNotice, updateGridView } = await import("../src/grid/views-bar.tsx")
const { deletionReceipts, reloadConflictingView, scopedDeletedView, selectViewSpace, viewEditorDraft, ViewConflictBanner, ViewDetail, ViewEditor } = await import("../src/views.tsx")

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

test("an action receipt invalidates the saved-view records and facet counts for its record type", () => {
  const objectType = SAMPLE_VIEW.objectType
  const filter = SAMPLE_VIEW.filter ?? null
  const params = recordsQueryParams({ ...EMPTY_QUERY, filter }).toString()
  const keys = [
    [...recordsQueryPrefix(objectType, SCOPE_A), params],
    [...recordsQueryPrefix(objectType), params],
    facetsQuery({ objectType, filter, scope: SCOPE_A }).queryKey,
    facetsQuery({ objectType, filter: null, scope: SCOPE_A, property: "stage" }).queryKey,
    facetsQuery({ objectType, filter }).queryKey,
  ]
  const unrelated = ["records", "demo.Other", SCOPE_A.orgId, SCOPE_A.environmentId, params]
  const client = new QueryClient()
  for (const key of [...keys, unrelated]) client.setQueryData(key, [])

  const [sending] = update(init({ actionId: "demo.record.update", risky: false }, "key"), { kind: "submit", input: "{}" })
  const [, commands] = update(sending, { kind: "answered", result: { kind: "receipt", receipt: {
    outcome: "committed", objectType, objectId: "row-1", version: 2,
    receipt: { receiptHash: "receipt", commitSeq: 2, recordedAtMs: 1 },
  } } })
  for (const command of commands) {
    if (command.kind === "refetch") invalidateActionReads(client, objectType)
  }

  for (const key of keys) expect(client.getQueryState(key)?.isInvalidated).toBe(true)
  expect(client.getQueryState(unrelated)?.isInvalidated).toBe(false)
  client.clear()
})

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
      return new Response(JSON.stringify({ id: "studio.view.a", kind: "view", version: 1, lifecycle: "active", payload: SAMPLE_VIEW }), { status: 200 })
    }
    if (parsed.pathname.endsWith("/v1/definitions/studio.view.retired")) {
      return new Response(JSON.stringify({ id: "studio.view.retired", kind: "view", version: 2, lifecycle: "retired", payload: SAMPLE_VIEW }), { status: 200 })
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
  const hostCalls: { readonly path: string; readonly orgId: string | null; readonly environmentId: string | null; readonly expectedOrg: string | null; readonly expectedEnv: string | null }[] = []
  const fetcher: Fetch = async (input, init) => {
    const url = String(input)
    if (url === "/api/me") {
      return new Response(JSON.stringify({
        organization: activeScope ? { id: activeScope.orgId } : null,
        environmentId: activeScope ? activeScope.environmentId : null,
      }), { status: 200 })
    }
    const parsed = new URL(url, "https://studio.test")
    const headers = new Headers(init?.headers)
    hostCalls.push({
      path: parsed.pathname,
      orgId: parsed.searchParams.get("orgId"),
      environmentId: parsed.searchParams.get("environmentId"),
      expectedOrg: headers.get(EXPECTED_ORG_HEADER),
      expectedEnv: headers.get(EXPECTED_ENV_HEADER),
    })
    if (parsed.pathname.endsWith(":admit")) {
      return new Response(JSON.stringify({ admitted: true, restatement: "Saved view.", diagnostics: [] }), { status: 200 })
    }
    return new Response(JSON.stringify({ installed: true, id: "studio.view.v1", version: 2 }), { status: 200 })
  }
  const store = cellViews(fetcher)

  // Matching scope dispatches explicitly to SCOPE_A with S1 expected-scope headers on every write.
  const admitted = await store.admit("studio.view.v1", SAMPLE_VIEW, SCOPE_A)
  expect(admitted.admitted).toBe(true)
  const saved = await store.save("studio.view.v1", SAMPLE_VIEW, 1, SCOPE_A)
  expect(saved).toEqual({ saved: true, id: "studio.view.v1", version: 2 })
  const retired = await store.retire("studio.view.v1", 2, SCOPE_A)
  expect(retired).toEqual({ saved: true, id: "studio.view.v1", version: 2 })
  expect(hostCalls).toEqual([
    { path: "/api/host/v1/definitions:admit", orgId: "org_a", environmentId: "prod", expectedOrg: "org_a", expectedEnv: "prod" },
    { path: "/api/host/v1/definitions:install", orgId: "org_a", environmentId: "prod", expectedOrg: "org_a", expectedEnv: "prod" },
    { path: "/api/host/v1/definitions/studio.view.v1:retire", orgId: "org_a", environmentId: "prod", expectedOrg: "org_a", expectedEnv: "prod" },
  ])

  // Switching org, switching environment, or losing the cell refuses before any host request is sent.
  hostCalls.length = 0
  for (const next of [SCOPE_B, SCOPE_A_STAGING, null] as const) {
    activeScope = next
    await expect(store.save("studio.view.v1", SAMPLE_VIEW, 1, SCOPE_A)).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
    await expect(store.retire("studio.view.v1", 1, SCOPE_A)).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
    await expect(store.admit("studio.view.v1", SAMPLE_VIEW, SCOPE_A)).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
    await expect(store.currentRevision("studio.view.v1", SCOPE_A)).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
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
      return new Response(JSON.stringify({ id: "studio.view.v1", kind: "view", version: serverVersion, lifecycle: "active", payload: SAMPLE_VIEW }), { status: 200 })
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
  expect(first.conflict).toEqual({ kind: "updated", version: 2 })

  // 2. If another edit lands before overwrite (server moves to 3), overwrite with 2 re-conflicts and refreshes to 3.
  serverVersion = 3
  const raced = await saveViewDraft(store, {
    id: "studio.view.v1",
    existing: true,
    view: SAMPLE_VIEW,
    expectedVersion: first.conflict?.kind === "updated" ? first.conflict.version : undefined,
    scope: SCOPE_A,
  })
  expect(raced.result).toEqual({ saved: false, conflict: true })
  expect(raced.conflict).toEqual({ kind: "updated", version: 3 })

  // 3. Overwrite with confirmed version 3 succeeds with version 4, and never omits expectedVersion.
  const overwritten = await saveViewDraft(store, {
    id: "studio.view.v1",
    existing: true,
    view: SAMPLE_VIEW,
    expectedVersion: raced.conflict?.kind === "updated" ? raced.conflict.version : undefined,
    scope: SCOPE_A,
  })
  expect(overwritten.result).toEqual({ saved: true, id: "studio.view.v1", version: 4 })
  expect(overwritten.conflict).toBeNull()
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

test("conflict without a readable current revision leaves conflict unknown so overwrite is withheld", async () => {
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
  expect(outcome.conflict).toEqual({ kind: "unknown" })
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

test("bound preview and detail reads pin to the draft scope, refuse on switch between render and dispatch, and discard in-flight reads after a switch", async () => {
  const raw = readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/three-systems.definitions.json"), "utf8")
  const catalog = catalogOf(readDocument<readonly Artifact[]>(raw, "recorded").document)
  const concept = catalog.byName.get("canon.Item")
  if (concept === undefined) throw new Error("canon.Item is not in the recorded catalog")

  let activeScope: StudioScope | null = SCOPE_A
  const hostCalls: string[] = []
  const gate: { reached: (() => void) | null; release: (() => void) | null } = { reached: null, release: null }
  let blockNextHost = false

  const fetcher: Fetch = async (input) => {
    const url = String(input)
    if (url === "/api/me") {
      return new Response(JSON.stringify({
        organization: activeScope ? { id: activeScope.orgId } : null,
        environmentId: activeScope ? activeScope.environmentId : null,
      }), { status: 200 })
    }
    const parsed = new URL(url, "https://studio.test")
    const orgId = parsed.searchParams.get("orgId")
    const environmentId = parsed.searchParams.get("environmentId")
    hostCalls.push(`${orgId}/${environmentId}:${parsed.pathname}`)
    if (blockNextHost) {
      blockNextHost = false
      gate.reached?.()
      await new Promise<void>((resolve) => { gate.release = resolve })
    }
    const rowLabel = orgId === SCOPE_B.orgId ? "WORKSPACE_B_SECRET_ROW" : "WORKSPACE_A_ROW"
    return Response.json({
      objectType: concept.name,
      records: [{ objectType: concept.name, id: `${orgId}-1`, scope: `${orgId}/${environmentId}`, version: 1, values: { label: rowLabel, code: rowLabel }, withheld: false }],
      quarantineCount: 0,
      scannedThrough: null,
      query: { sort: [], filter: null },
      next: null,
      total: { count: 1, approximate: false },
    })
  }
  const store = cellViews(fetcher)
  const view: SavedView = { title: "Scoped preview", objectType: concept.name, columns: ["code"] }
  const previewKey = viewPreviewQueryKey(SCOPE_A, view)

  // 1. Matching scope reads explicitly from SCOPE_A.
  const ok = await store.preview(concept.name, { filter: null, sort: [], limit: 5 }, SCOPE_A)
  expect(ok.kind).toBe("page")
  expect(hostCalls).toEqual([`org_a/prod:/api/host/v1/objects/${concept.name}`])

  // 2. Scope switches from A to B between render and dispatch: 0 host calls, B's rows are never cached under A's key.
  hostCalls.length = 0
  activeScope = SCOPE_B
  const client = new QueryClient({ defaultOptions: { queries: { refetchOnMount: false, retryOnMount: false } } })
  await expect(client.fetchQuery({
    queryKey: previewKey,
    queryFn: ({ signal }) => store.preview(concept.name, { filter: null, sort: [], limit: 5 }, SCOPE_A, signal),
    retry: false,
  })).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  expect(hostCalls).toHaveLength(0)
  expect(client.getQueryData(previewKey)).toBeUndefined()
  expect(client.getQueryState(previewKey)?.error).toBeInstanceOf(StudioScopeError)

  // Rendering ViewEditor against that QueryClient (even while activeScope prop is still A) shows the workspace-changed state and never B's rows.
  const root = createRootRoute()
  const router = createRouter({
    routeTree: root.addChildren([createRoute({
      getParentRoute: () => root,
      path: "/",
      component: () => createElement(ViewEditor, {
        concepts: [concept],
        stored: { id: "studio.view.canon_item.a", version: 1, scope: SCOPE_A, view },
        activeScope: SCOPE_A,
        cellAvailable: true,
        store,
        onSaved: () => {},
      }),
    })]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  })
  await router.load()
  const html = renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(RouterProvider, { router })))
  expect(html).toContain(WORKSPACE_CHANGED_MESSAGE)
  expect(html).not.toContain("WORKSPACE_B_SECRET_ROW")

  // 3. Scope switches while an A fetch is already in flight: the resolved response is discarded and never cached.
  activeScope = SCOPE_A
  blockNextHost = true
  const hostReached = new Promise<void>((resolve) => { gate.reached = resolve })
  const inFlightClient = new QueryClient()
  const pending = inFlightClient.fetchQuery({
    queryKey: previewKey,
    queryFn: ({ signal }) => store.preview(concept.name, { filter: null, sort: [], limit: 5 }, SCOPE_A, signal),
    retry: false,
  })
  await hostReached
  activeScope = SCOPE_B
  gate.release?.()
  await expect(pending).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  expect(inFlightClient.getQueryData(previewKey)).toBeUndefined()

  // 4. RecordsGrid with explicit scope keys its cache by orgId/environmentId and hides rows on StudioScopeError.
  const gridClient = new QueryClient({ defaultOptions: { queries: { refetchOnMount: false, retryOnMount: false } } })
  const scopedGridKey = ["records", concept.name, SCOPE_A.orgId, SCOPE_A.environmentId, recordsQueryParams(EMPTY_QUERY).toString()] as const
  await expect(gridClient.fetchQuery({
    queryKey: scopedGridKey,
    queryFn: ({ signal }) => store.preview(concept.name, EMPTY_QUERY, SCOPE_A, signal),
    retry: false,
  })).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  expect(gridClient.getQueryData(scopedGridKey)).toBeUndefined()
  const gridRoot = createRootRoute()
  const gridRouter = createRouter({
    routeTree: gridRoot.addChildren([createRoute({
      getParentRoute: () => gridRoot,
      path: "/",
      component: () => createElement(RecordsGrid, {
        concept,
        scope: SCOPE_A,
        read: (t, q, s) => store.preview(t, q, SCOPE_A, s),
      }),
    })]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  })
  await gridRouter.load()
  const gridHtml = renderToStaticMarkup(createElement(QueryClientProvider, { client: gridClient }, createElement(RouterProvider, { router: gridRouter })))
  expect(gridHtml).toContain(WORKSPACE_CHANGED_MESSAGE)
  expect(gridHtml).not.toContain("WORKSPACE_B_SECRET_ROW")
})

test("conflict-after-delete reports deletion, hides dead Reload/Overwrite buttons, and supports Save as new view, Discard, and Reload-after-delete", async () => {
  const raw = readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/three-systems.definitions.json"), "utf8")
  const catalog = catalogOf(readDocument<readonly Artifact[]>(raw, "recorded").document)
  const concept = catalog.byName.get("canon.Item")
  if (concept === undefined) throw new Error("canon.Item is not in the recorded catalog")

  const installs: { readonly id: string; readonly expectedVersion?: number }[] = []
  const fetcher: Fetch = async (input, init) => {
    const url = String(input)
    if (url === "/api/me") {
      return Response.json({ organization: { id: SCOPE_A.orgId }, environmentId: SCOPE_A.environmentId })
    }
    const parsed = new URL(url, "https://studio.test")
    if (parsed.pathname.endsWith("/v1/definitions:install")) {
      const body = JSON.parse(String(init?.body ?? "{}")) as { id: string; expectedVersion?: number }
      installs.push({ id: body.id, expectedVersion: body.expectedVersion })
      if (body.id === "studio.view.deleted_by_other") {
        return Response.json({ code: "definition.version_conflict", message: "Version conflict." }, { status: 409 })
      }
      return Response.json({ installed: true, id: body.id, version: 1 })
    }
    if (parsed.pathname.endsWith("/v1/definitions/studio.view.deleted_by_other")) {
      return Response.json({ id: "studio.view.deleted_by_other", kind: "view", version: 2, lifecycle: "retired", payload: SAMPLE_VIEW })
    }
    return Response.json({ code: "definition.not_found" }, { status: 404 })
  }
  const store = cellViews(fetcher)

  // 1. Saving against a retired view classifies the conflict as deleted.
  const afterDelete = await saveViewDraft(store, {
    id: "studio.view.deleted_by_other",
    existing: true,
    view: { ...SAMPLE_VIEW, title: "My edited view" },
    expectedVersion: 1,
    scope: SCOPE_A,
  })
  expect(afterDelete.result).toEqual({ saved: false, conflict: true })
  expect(afterDelete.conflict).toEqual({ kind: "deleted", version: 2 })

  // 2. Conflict UI says it was deleted, offers Save as new view & Discard my changes, and omits dead Reload theirs / Overwrite buttons.
  const client = new QueryClient()
  const root = createRootRoute()
  const router = createRouter({
    routeTree: root.addChildren([createRoute({
      getParentRoute: () => root,
      path: "/",
      component: () => createElement(ViewEditor, {
        concepts: [concept],
        stored: { id: "studio.view.deleted_by_other", version: 1, scope: SCOPE_A, view: { ...SAMPLE_VIEW, objectType: concept.name, columns: [] } },
        activeScope: SCOPE_A,
        cellAvailable: true,
        store,
        initialConflict: afterDelete.conflict,
        onSaved: () => {},
        onReload: async () => "deleted" as const,
        onDiscard: () => {},
      }),
    })]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  })
  await router.load()
  const html = renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(RouterProvider, { router })))
  expect(html).toContain("This view was deleted by someone else.")
  expect(html).toContain("Save as new view")
  expect(html).toContain("Discard my changes")
  expect(html).not.toContain("Reload theirs")
  expect(html).not.toContain("Overwrite with mine")

  // 3. Choice A ("Save as new view"): saves under a fresh id with no expectedVersion, leaving the retired id untouched.
  const freshId = newViewId(SAMPLE_VIEW.objectType, "My edited view", () => "new12345")
  const savedNew = await saveViewDraft(store, {
    id: freshId,
    existing: false,
    view: { ...SAMPLE_VIEW, title: "My edited view" },
    expectedVersion: undefined,
    scope: SCOPE_A,
  })
  expect(savedNew.result).toEqual({ saved: true, id: freshId, version: 1 })
  expect(savedNew.conflict).toBeNull()
  expect(installs).toEqual([
    { id: "studio.view.deleted_by_other", expectedVersion: 1 },
    { id: freshId, expectedVersion: undefined },
  ])

  // 4. Choice B ("Reload theirs" when view was deleted after an edit conflict): never silently no-ops.
  const loaded: StoredView[] = []
  expect(await reloadConflictingView(
    async () => ({ isSuccess: true, data: null }),
    (next) => { loaded.push(next) },
  )).toBe("deleted")
  expect(loaded).toHaveLength(0)

  const updatedView: StoredView = { id: "studio.view.active", version: 2, scope: SCOPE_A, view: SAMPLE_VIEW }
  expect(await reloadConflictingView(
    async () => ({ isSuccess: true, data: updatedView }),
    (next) => { loaded.push(next) },
  )).toBe("reloaded")
  expect(loaded).toEqual([updatedView])

  expect(await reloadConflictingView(
    async () => ({ isSuccess: false, data: undefined }),
    () => {},
  )).toBe("failed")
})

test("ViewEditor live preview renders withheld records and distinguishes refusal from no matches", async () => {
  const raw = readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/three-systems.definitions.json"), "utf8")
  const catalog = catalogOf(readDocument<readonly Artifact[]>(raw, "recorded").document)
  const hidden = new Set(["id", "label", "display_name"])
  const concept = catalog.concepts.find((c) => c.fields.filter(({ id }) => !hidden.has(id)).length >= 2)
  if (concept === undefined) throw new Error("no recorded concept with >= 2 visible fields")
  const cols = concept.fields.filter(({ id }) => !hidden.has(id)).slice(0, 2).map(({ id }) => id)
  const view: SavedView = { title: "Withheld preview", objectType: concept.name, columns: cols }
  const page: RecordsRead = {
    kind: "page",
    read: readDocument<ObjectPage>(JSON.stringify({
      objectType: concept.name,
      records: [
        { objectType: concept.name, id: "rec-hidden", scope: "s", version: 1, values: null, withheld: true },
        { objectType: concept.name, id: "rec-partial", scope: "s", version: 1, values: { [cols[1]!]: "VISIBLE_PART" }, withheld: false, withheldProperties: [cols[0]!] },
        { objectType: concept.name, id: "rec-open", scope: "s", version: 1, values: { [cols[0]!]: "OPEN_A", [cols[1]!]: "OPEN_B" }, withheld: false },
      ],
      quarantineCount: 0,
      scannedThrough: null,
      query: { sort: [], filter: null },
      next: null,
      total: { count: 3, approximate: false },
    }), "live"),
    origin: "live",
    served: { sort: [], filter: null },
    next: undefined,
    total: { count: 3, approximate: false },
  }
  const client = new QueryClient()
  client.setQueryData(viewPreviewQueryKey(SCOPE_A, view), page)
  const root = createRootRoute()
  const router = createRouter({
    routeTree: root.addChildren([createRoute({
      getParentRoute: () => root,
      path: "/",
      component: () => createElement(ViewEditor, {
        concepts: [concept],
        stored: { id: "studio.view.withheld", version: 1, scope: SCOPE_A, view },
        activeScope: SCOPE_A,
        cellAvailable: true,
        onSaved: () => {},
      }),
    })]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  })
  await router.load()
  const html = renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(RouterProvider, { router })))
  expect(html).toContain('data-withheld="true"')
  expect(html).toContain("rec-hidden")
  expect(html).toContain(WITHHELD_ROW.replaceAll("'", "&#x27;"))
  expect(html).toContain("VISIBLE_PART")
  expect(html).toContain("OPEN_A")
  expect(html).toContain("OPEN_B")

  const refusal: RecordsRead = {
    kind: "refused", status: 422, code: "query.refused", message: "The cell refused this filter.", diagnostics: [],
  }
  client.setQueryData(viewPreviewQueryKey(SCOPE_A, view), refusal)
  const refused = renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(RouterProvider, { router })))
  expect(refused).toContain("The cell refused this filter.")
  expect(refused).not.toContain("No records currently match this filter.")
})

test("ViewDetailPage scopes deleted-view banner to the deleted view's id so navigating to another view renders it normally", async () => {
  const raw = readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/three-systems.definitions.json"), "utf8")
  const artifacts = readDocument<readonly Artifact[]>(raw, "recorded")
  const concept = catalogOf(artifacts.document).byName.get("canon.Item")
  if (concept === undefined) throw new Error("canon.Item is not in the recorded catalog")

  const removedA = { id: "studio.view.a", version: 2, scope: SCOPE_A, view: { ...SAMPLE_VIEW, title: "Deleted View A", objectType: concept.name } }
  const storedB: StoredView = { id: "studio.view.b", version: 1, scope: SCOPE_A, view: { ...SAMPLE_VIEW, title: "Active View B", objectType: concept.name, columns: [] } }

  expect(scopedDeletedView(removedA, "studio.view.a", SCOPE_A)).toEqual(removedA)
  expect(scopedDeletedView(removedA, "studio.view.b", SCOPE_A)).toBeUndefined()
  expect(scopedDeletedView(removedA, "studio.view.a", SCOPE_B)).toBeUndefined()

  // The receipt is kept above the route, as the reader who deleted it (no session cached here).
  deletionReceipts.clear()
  deletionReceipts.add({ ...removedA, reader: sessionKey(undefined) })
  const renderAt = async (routeId: string) => {
    const client = new QueryClient()
    client.setQueryData(["studio-scope"], SCOPE_A)
    client.setQueryData(["definitions"], artifacts)
    client.setQueryData(["view", "studio.view.b", SCOPE_A.orgId, SCOPE_A.environmentId], storedB)
    const root = createRootRoute()
    const router = createRouter({
      routeTree: root.addChildren([createRoute({
        getParentRoute: () => root,
        path: "/views/$id",
        component: () => createElement(ViewDetail),
      })]),
      history: createMemoryHistory({ initialEntries: [`/views/${routeId}`] }),
    })
    await router.load()
    return renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(RouterProvider, { router })))
  }

  // Navigating to /views/studio.view.b while removedA is in state shows View B, not A's deletion banner.
  const htmlB = await renderAt("studio.view.b")
  expect(htmlB).toContain("Active View B")
  expect(htmlB).not.toContain("Deleted “Deleted View A”")

  // Staying on /views/studio.view.a still shows A's deletion banner with Undo.
  const htmlA = await renderAt("studio.view.a")
  expect(htmlA).toContain("Deleted “Deleted View A”")
  expect(htmlA).toContain("Undo")
  deletionReceipts.clear()
})

test("ViewsBar keeps Undo available when restoration fails and clears it only after restoration succeeds", async () => {
  let activeScope: StudioScope = SCOPE_A
  let refuseNext = false
  const fetcher: Fetch = async (input) => {
    const url = String(input)
    if (url === "/api/me") {
      return Response.json({ organization: { id: activeScope.orgId }, environmentId: activeScope.environmentId })
    }
    if (refuseNext) {
      refuseNext = false
      return Response.json({ diagnostics: [{ code: "definition.refused", message: "Temporary refusal." }] }, { status: 422 })
    }
    return Response.json({ installed: true, id: "studio.view.v1", version: 3 })
  }
  const store = cellViews(fetcher)
  const gone: StoredView = { id: "studio.view.v1", version: 1, scope: SCOPE_A, view: SAMPLE_VIEW }
  let current: ViewsBarNotice | null = null
  const restored: StoredView[] = []

  current = deletedViewNotice({
    store,
    gone,
    retiredVersion: 2,
    setNotice: (next) => { current = typeof next === "function" ? next(current) : next },
    onRestored: (next) => { restored.push(next) },
  })
  expect(current.text).toBe("Deleted “Items needing review”.")
  expect(current.undo).toBeDefined()

  // 1. Rejection (workspace switched): error is shown, and Undo stays attached so the user can retry.
  activeScope = SCOPE_B
  await current.undo!()
  expect(current.text).toContain(WORKSPACE_CHANGED_MESSAGE)
  expect(current.undo).toBeDefined()
  expect(restored).toHaveLength(0)

  // 2. Non-saved refusal (422): error is shown, and Undo still stays attached.
  activeScope = SCOPE_A
  refuseNext = true
  await current.undo!()
  expect(current.text).toContain("Temporary refusal.")
  expect(current.undo).toBeDefined()
  expect(restored).toHaveLength(0)

  // 3. Retry once healthy: restores the view and clears the Undo callback.
  await current.undo!()
  expect(current.text).toBe("Restored “Items needing review”.")
  expect(current.undo).toBeUndefined()
  expect(restored).toEqual([{ id: "studio.view.v1", version: 3, scope: SCOPE_A, view: SAMPLE_VIEW }])
})

test("the drawer shows the opened record as the latest page serves it, and closes when that page withholds the read", () => {
  const row = (values: Record<string, unknown>, withheld = false, id = "a") => ({
    record: { objectType: "canon.Item", id, scope: "s", version: 1, values, withheld },
    at: "/records/0",
    numbers: new Map<string, string>(),
  })
  const first = row({ label: "Alpha" })
  expect(visibleDrawerRow("a", [first], null)).toBe(first)
  // A later read withholds the row: the drawer shows the withheld row, never the values first opened.
  const withheld = row({}, true)
  expect(visibleDrawerRow("a", [withheld], null)).toBe(withheld)
  // A later read changes a value: the drawer shows the new value.
  const changed = row({ label: "Alpha (renamed)" })
  expect(visibleDrawerRow("a", [changed], null)?.record.values).toEqual({ label: "Alpha (renamed)" })
  // The record is gone from the page, the read was refused, or the workspace changed: the drawer closes.
  expect(visibleDrawerRow("a", [row({ label: "Beta" }, false, "b")], null)).toBeNull()
  expect(visibleDrawerRow("a", [first], null, true)).toBeNull()
  expect(visibleDrawerRow("a", [first], new StudioScopeError(WORKSPACE_CHANGED_MESSAGE))).toBeNull()
  expect(visibleDrawerRow(null, [first], null)).toBeNull()
})

test("a detail definition read stays pinned before dispatch and discards a late result from its former workspace", async () => {
  let active = SCOPE_A
  const fetched: (string | null)[] = []
  const store = cellViews(async (input) => {
    const url = new URL(String(input), "https://studio.test")
    fetched.push(url.searchParams.get("orgId"))
    active = SCOPE_B
    return Response.json({ kind: "view", version: 1, payload: SAMPLE_VIEW })
  }, async () => active)
  const id = "studio.view.shared"
  await expect(store.getAtScope(id, SCOPE_A)).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  await expect(store.getAtScope(id, SCOPE_A)).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  expect(fetched).toEqual([SCOPE_A.orgId])
  expect((await store.getAtScope(id, SCOPE_B))?.scope).toEqual(SCOPE_B)
  expect(fetched).toEqual([SCOPE_A.orgId, SCOPE_B.orgId])
})

test.each(["saved", "refused"] as const)("an older Undo that is %s leaves the newer deletion notice intact", async (outcome) => {
  let current: ViewsBarNotice | null = null
  const pending = Promise.withResolvers<ViewSaved>()
  const remove = (id: string) => deletedViewNotice({
    store: { save: () => pending.promise },
    gone: { id, version: 1, scope: SCOPE_A, view: { ...SAMPLE_VIEW, title: id } },
    retiredVersion: 2,
    setNotice: (next) => { current = typeof next === "function" ? next(current) : next },
    onRestored: () => {},
  })
  current = remove("A")
  const undo = current.undo?.()
  const newer = remove("B")
  current = newer
  pending.resolve(outcome === "saved" ? { saved: true, id: "A", version: 3 } : { saved: false, findings: [] })
  await undo
  expect(current).toBe(newer)
  expect(current.undo).toBeDefined()
})

test.each([
  { saving: true, reloading: false, reloadLabel: "Reload theirs" },
  { saving: false, reloading: true, reloadLabel: "Reloading…" },
])("both conflict actions stay disabled during the pending operation", ({ saving, reloading, reloadLabel }) => {
  const html = renderToStaticMarkup(createElement(ViewConflictBanner, {
    conflict: { kind: "updated", version: 2 }, canCreate: true, canReload: true, canOverwrite: true, saving, reloading,
    reloadError: null, onReload: () => {}, onOverwrite: () => {}, onSaveAsNew: () => {},
  }))
  expect(html).toMatch(new RegExp(`<button[^>]*disabled=""[^>]*>${reloadLabel}</button>`))
  expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Overwrite with mine<\/button>/)
})

test("undoing a preset Space selection restores the custom Space control and name", () => {
  const start = history.start(viewEditorDraft({ ...SAMPLE_VIEW, space: "Claims" }))
  const changed = history.set(start, selectViewSpace(start.present, "Operations"))
  expect(changed.present).toMatchObject({ spaceMode: "preset", view: { space: "Operations" } })
  expect(history.undo(changed).present).toMatchObject({ spaceMode: "custom", view: { space: "Claims" } })
  expect(selectViewSpace(changed.present, "__custom__")).toMatchObject({ spaceMode: "custom", view: { space: "" } })
})

test("grid query edits preserve legacy columns while a displayed column change replaces them", () => {
  const visible = ["stage", "score"]
  const columns = Array.from({ length: 33 }, () => visible).flat()
  const view = { ...SAMPLE_VIEW, columns }
  const sort = [{ property: "stage", direction: "asc" as const }]
  const updated = updateGridView(view, { ...EMPTY_QUERY, sort, columns: visible, initialColumns: visible })
  expect(updated.columns).toBe(columns)
  expect(updated.sort).toBe(sortText(sort))
  const reordered = [...visible].reverse()
  expect(updateGridView(view, { ...EMPTY_QUERY, columns: reordered, initialColumns: visible }).columns).toEqual(reordered)
})

test("scoped reads thread AbortSignal to network fetches, reject abort during final probe, and preserve outages", async () => {
  // 1. getAtScope forwards AbortSignal to both scope probes and the definition GET (linked through fetchWithTimeout),
  //    aborting the in-flight GET when cancelled and rejecting if cancelled during the final probe.
  const scopeSignals: (AbortSignal | null | undefined)[] = []
  let getSignalAborted = false
  const abortDuringGet = new AbortController()
  const cancelStore = cellViews(
    async (_input, init) => new Promise<Response>((_resolve, reject) => {
      init?.signal?.addEventListener("abort", () => {
        getSignalAborted = true
        reject(init.signal?.reason ?? new DOMException("Aborted", "AbortError"))
      }, { once: true })
      abortDuringGet.abort(new DOMException("Cancelled", "AbortError"))
    }),
    async (signal) => { scopeSignals.push(signal); return SCOPE_A },
  )
  await expect(cancelStore.getAtScope("studio.view.v1", SCOPE_A, abortDuringGet.signal)).rejects.toThrow()
  expect(getSignalAborted).toBe(true)
  expect(scopeSignals).toEqual([abortDuringGet.signal])

  let probeCount = 0
  const controller = new AbortController()
  const seenProbeSignals: (AbortSignal | null | undefined)[] = []
  const store = cellViews(
    async () => Response.json({ id: "studio.view.v1", kind: "view", version: 1, lifecycle: "active", payload: SAMPLE_VIEW }),
    async (signal) => {
      seenProbeSignals.push(signal)
      probeCount += 1
      if (probeCount === 2) controller.abort(new DOMException("Cancelled", "AbortError"))
      return SCOPE_A
    },
  )
  await expect(store.getAtScope("studio.view.v1", SCOPE_A, controller.signal)).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  expect(seenProbeSignals).toEqual([controller.signal, controller.signal])

  // 2. readAtScope also rejects when signal aborts before the final probe resolves.
  const lateAbort = new AbortController()
  let scopeCalls = 0
  await expect(readAtScope(
    SCOPE_A,
    async () => {
      scopeCalls += 1
      if (scopeCalls === 2) lateAbort.abort()
      return SCOPE_A
    },
    async () => "ok",
    lateAbort.signal,
  )).rejects.toThrow(StudioScopeError)

  // 3. A 200 definition response whose body stream fails preserves the stream error instead of substituting null.
  const brokenStream = new ReadableStream<Uint8Array>({
    start(ctrl) { ctrl.error(new CellUnavailable(0, "Stream timed out", { timedOut: true, retryable: true, code: "cell.timeout" })) },
  })
  const streamStore = cellViews(
    async () => new Response(brokenStream, { status: 200, headers: { "content-type": "application/json" } }),
    async () => SCOPE_A,
  )
  const streamErr = await streamStore.get("studio.view.v1", SCOPE_A).catch((e: unknown) => e)
  expect(streamErr).toBeInstanceOf(CellUnavailable)
  expect((streamErr as CellUnavailable).timedOut).toBe(true)

  // 4. Scoped live records preview preserves structured 503 cell.unopenable instead of reporting unserved type/query.
  const unopenableFetch: Fetch = async () => Response.json(
    { code: "cell.unopenable", message: "Cell is temporarily sealed.", retryable: false },
    { status: 503 },
  )
  const previewErr = await readRecordsAtScope("demo.Record", EMPTY_QUERY, SCOPE_A, unopenableFetch).catch((e: unknown) => e)
  expect(previewErr).toBeInstanceOf(CellUnavailable)
  expect((previewErr as CellUnavailable).status).toBe(503)
  expect((previewErr as CellUnavailable).code).toBe("cell.unopenable")
  expect((previewErr as CellUnavailable).retryable).toBe(false)

  // 5. facetsQuery forwards the TanStack Query AbortSignal to /api/me and the facets fetch without misreporting FacetsTimedOut.
  const facetSignals: (AbortSignal | null | undefined)[] = []
  const facetAbort = new AbortController()
  const facetQuery = facetsQuery(
    { objectType: "demo.Record", filter: null, scope: SCOPE_A },
    async (input, init) => {
      facetSignals.push(init?.signal)
      if (String(input) === "/api/me") return Response.json({ organization: { id: SCOPE_A.orgId }, environmentId: SCOPE_A.environmentId })
      facetAbort.abort(new DOMException("Cancelled", "AbortError"))
      throw facetAbort.signal.reason
    },
  )
  const facetErr = await Promise.resolve(facetQuery.queryFn!({ signal: facetAbort.signal, queryKey: facetQuery.queryKey, meta: undefined, client: new QueryClient() } as never)).catch((e: unknown) => e)
  expect((facetErr as Error).name).toBe("AbortError")
  expect(facetSignals.length).toBeGreaterThanOrEqual(2)
  expect(facetSignals.every((s) => s !== undefined)).toBe(true)
})

// #1700 S6: only a `view` definition opens as a view; another kind under that id is not one.
test("a definition of another kind is not a view, so the detail never wires its controls to it", async () => {
  const store = cellViews(async () => Response.json({ id: "ops.automation.a", kind: "automation", version: 1, lifecycle: "active", payload: { title: "Restock", objectType: "demo.Record" } }))
  expect(await store.get("ops.automation.a", SCOPE_A)).toBeUndefined()
})

// #1700 S8: the list follows the cell's cursor past ten pages; a repeated cursor is an error, not success.
test("listAll follows the cursor to the end and refuses a listing that repeats a page", async () => {
  const pages = 12
  const listCalls: (string | null)[] = []
  const fetcher = (repeat: boolean): Fetch => async (input) => {
    const url = new URL(String(input), "https://studio.test")
    if (url.pathname.endsWith("/v1/definitions:list")) {
      const cursor = url.searchParams.get("cursor")
      listCalls.push(cursor)
      const index = cursor === null ? 0 : Number(cursor.slice(1))
      const next = repeat ? "p1" : index + 1 < pages ? `p${index + 1}` : null
      return Response.json({ items: [{ id: `studio.view.${index}` }], cursor: next })
    }
    const id = decodeURIComponent(url.pathname.split("/v1/definitions/")[1] ?? "")
    return Response.json({ id, kind: "view", version: 1, lifecycle: "active", payload: { ...SAMPLE_VIEW, title: id } })
  }
  const listed = await cellViews(fetcher(false)).listAll(SCOPE_A)
  expect(listed).toHaveLength(pages)
  expect(listCalls).toHaveLength(pages)
  listCalls.length = 0
  await expect(cellViews(fetcher(true)).listAll(SCOPE_A)).rejects.toBeInstanceOf(CellUnavailable)
  expect(listCalls).toEqual([null, "p1", "p1"])
})

// #1700 S7: each deletion keeps its own receipt, by reader, workspace and view id.
test("a second deletion keeps the first one's receipt, and receipts never cross readers or workspaces", () => {
  deletionReceipts.clear()
  const gone = (id: string, scope: StudioScope, reader = "alice") => ({ id, version: 2, scope, view: { ...SAMPLE_VIEW, title: id }, reader })
  deletionReceipts.add(gone("studio.view.a", SCOPE_A))
  deletionReceipts.add(gone("studio.view.b", SCOPE_A))
  deletionReceipts.add(gone("studio.view.c", SCOPE_B))
  deletionReceipts.add(gone("studio.view.d", SCOPE_A, "bob"))
  expect(deletionReceipts.of("alice", SCOPE_A).map(({ id }) => id)).toEqual(["studio.view.a", "studio.view.b"])
  expect(deletionReceipts.of("alice", SCOPE_B).map(({ id }) => id)).toEqual(["studio.view.c"])
  expect(deletionReceipts.of("bob", SCOPE_A).map(({ id }) => id)).toEqual(["studio.view.d"])
  // Restoring one leaves the others, and a second deletion of the same view replaces only its own receipt.
  deletionReceipts.remove({ reader: "alice", scope: SCOPE_A, id: "studio.view.a" })
  deletionReceipts.add({ ...gone("studio.view.b", SCOPE_A), version: 4 })
  expect(deletionReceipts.of("alice", SCOPE_A).map(({ id, version }) => [id, version])).toEqual([["studio.view.b", 4]])
  deletionReceipts.clear()
})
