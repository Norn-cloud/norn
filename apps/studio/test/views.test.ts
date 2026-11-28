import { expect, mock, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRoute, createRouter } from "@tanstack/react-router"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { createElement, type ReactElement } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { catalogOf } from "../src/lib/catalog.ts"
import type { Fetch } from "../src/lib/cell-read.ts"
import { CellUnavailable, EXPECTED_ENV_HEADER, EXPECTED_ORG_HEADER, NO_DATA_CELL, readAtScope, StudioScopeError, type StudioScope } from "../src/lib/definitions.ts"
import { history } from "../src/lib/editor.ts"
import { readDocument } from "../src/lib/exact.ts"
import { facetsQuery } from "../src/lib/facets.ts"
import { EMPTY_QUERY, recordsQueryParams, recordsQueryPrefix } from "../src/lib/records-query.ts"
import type { RecordsRead } from "../src/lib/records-source.ts"
import { init, update } from "../src/lib/screen/call.ts"
import type { Artifact, ObjectPage } from "../src/lib/transport.ts"
import type { Workspace } from "../src/lib/workspace.tsx"
import {
  cellViews,
  cleanView,
  duplicateView,
  mapBounded,
  newViewId,
  saveViewDraft,
  sortKeys,
  sortText,
  UNREADABLE_VIEW,
  VIEW_READ_CONCURRENCY,
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

mock.module("../src/lib/transport.ts", () => ({ transport: {} }))
const { readRecordsAtScope } = await import("../src/lib/raw/records.ts")
const { workspaceOf } = await import("../src/lib/workspace.tsx")
const { invalidateActionReads } = await import("../src/actions/action-sheet.tsx")
const { RecordsGrid, readWithdrawn, visibleDrawerRow } = await import("../src/grid/records-grid.tsx")
const { deletedViewNotice, updateGridView } = await import("../src/grid/views-bar.tsx")
const { deletionReceiptFor, deletionReceipts, receiptSuperseded, reloadConflictingView, scopedDeletedView, selectViewSpace, viewEditorDraft, ViewConflictBanner, ViewDetail, ViewEditor } = await import("../src/views.tsx")

const SCOPE_A: StudioScope = { orgId: "org_a", environmentId: "prod" }
const SCOPE_B: StudioScope = { orgId: "org_b", environmentId: "prod" }
const SCOPE_A_STAGING: StudioScope = { orgId: "org_a", environmentId: "staging" }

function liveAt(scope: StudioScope, fetcher?: Fetch) {
  const workspace = workspaceOf({ status: "success", data: { kind: "live", scope, catalog: { document: [], numbers: new Map(), origin: "live" }, counts: {}, publishedQuestions: new Set<string>(), builtins: [] } }, fetcher)
  if (workspace.kind !== "live") throw new Error("expected a live workspace")
  return workspace
}
const LIVE_A = liveAt(SCOPE_A)
const DECIDED_A = LIVE_A.scope

const RECORDED_ARTIFACTS = readDocument<readonly Artifact[]>(
  readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/three-systems.definitions.json"), "utf8"),
  "recorded",
)
const RECORDED_CATALOG = catalogOf(RECORDED_ARTIFACTS.document)
const CANON_ITEM = RECORDED_CATALOG.byName.get("canon.Item")!
const HIDDEN_COLS = new Set(["id", "label", "display_name"])
const TWO_FIELD_CONCEPT = RECORDED_CATALOG.concepts.find((c) => c.fields.filter(({ id }) => !HIDDEN_COLS.has(id)).length >= 2)!

const SAMPLE_VIEW: SavedView = {
  title: "Items needing review",
  description: "Open items under threshold.",
  space: "Operations",
  objectType: "demo.Record",
  filter: { all: [{ property: "score", op: "lt", value: "60" }] },
  sort: "score:asc",
  columns: ["score", "stage"],
}

const meResponse = (scope: StudioScope | null) =>
  Response.json({ organization: scope ? { id: scope.orgId } : null, environmentId: scope ? scope.environmentId : null })

const recordsPage = (objectType: string, records: readonly Record<string, unknown>[]): RecordsRead => ({
  kind: "page",
  read: readDocument<ObjectPage>(JSON.stringify({
    objectType,
    records,
    quarantineCount: 0,
    scannedThrough: null,
    query: { sort: [], filter: null },
    next: null,
    total: { count: records.length, approximate: false },
  }), "live"),
  origin: "live",
  served: { sort: [], filter: null },
  next: undefined,
  total: { count: records.length, approximate: false },
})

async function renderRouted(
  element: () => ReactElement,
  options: { readonly client?: QueryClient; readonly path?: string; readonly initialEntry?: string } = {},
): Promise<string> {
  const client = options.client ?? new QueryClient()
  const root = createRootRoute()
  const router = createRouter({
    routeTree: root.addChildren([createRoute({ getParentRoute: () => root, path: options.path ?? "/", component: element })]),
    history: createMemoryHistory({ initialEntries: [options.initialEntry ?? "/"] }),
  })
  await router.load()
  return renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(RouterProvider, { router })))
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
  const [, commands] = update(sending, {
    kind: "answered",
    result: { kind: "receipt", receipt: { outcome: "committed", objectType, objectId: "row-1", version: 2, receipt: { receiptHash: "receipt", commitSeq: 2, recordedAtMs: 1 } } },
  })
  for (const command of commands) {
    if (command.kind === "refetch") invalidateActionReads(client, objectType)
  }

  for (const key of keys) expect(client.getQueryState(key)?.isInvalidated).toBe(true)
  expect(client.getQueryState(unrelated)?.isInvalidated).toBe(false)
  client.clear()
})

test("views helpers normalize payloads, preserve space and column order, duplicate within length bound and map pointers", () => {
  const cleaned = cleanView({
    title: "  Items needing review  ",
    description: "  ",
    space: "  Operations  ",
    objectType: "demo.Record",
    filter: { all: [{ property: "score", op: "lt", value: "60" }] },
    sort: sortText([{ property: "score", direction: "asc" }]),
    columns: ["score", "stage"],
  })
  expect(cleaned).toMatchObject({ title: "Items needing review", space: "Operations", columns: ["score", "stage"] })
  expect(cleaned.description).toBeUndefined()
  expect(sortKeys(cleaned.sort)).toEqual([{ property: "score", direction: "asc" }])

  expect(duplicateView(cleaned)).toMatchObject({ title: "Items needing review (copy)", space: "Operations", columns: ["score", "stage"] })
  const longCopy = duplicateView({ ...cleaned, title: "A".repeat(120) })
  expect([...longCopy.title].length).toBeLessThanOrEqual(120)
  expect(longCopy.title.endsWith(" (copy)")).toBe(true)

  expect(newViewId("demo.Record", "Needs help!", () => "abcd1234")).toBe("studio.view.demo_record.needs_help_abcd1234")
  for (const [pointer, expected] of [
    ["/payload/columns/0", "columns"],
    ["/space", "space"],
    ["/filter/all/0/value", "filter"],
    ["/rowActions/0", null],
    ["/unknown", null],
  ] as const) {
    expect(viewField(pointer)).toBe(expected)
  }
})

test("views list and get pin one scope, attach loaded scope, skip retired defs, and propagate non-404 errors", async () => {
  let activeScope: StudioScope = SCOPE_A
  const seenScopes: string[] = []
  const store = cellViews(async (input) => {
    const url = String(input)
    if (url === "/api/me") return meResponse(activeScope)
    const parsed = new URL(url, "https://studio.test")
    seenScopes.push(`${parsed.searchParams.get("orgId")}/${parsed.searchParams.get("environmentId")}`)
    if (parsed.pathname.endsWith("/v1/definitions:list")) {
      activeScope = SCOPE_B
      return Response.json({ items: [{ id: "studio.view.a", updatedAtMs: 1 }, { id: "studio.view.retired", updatedAtMs: 2 }], cursor: null })
    }
    if (parsed.pathname.endsWith("/v1/definitions/studio.view.a")) {
      return Response.json({ id: "studio.view.a", kind: "view", version: 1, lifecycle: "active", payload: SAMPLE_VIEW })
    }
    if (parsed.pathname.endsWith("/v1/definitions/studio.view.retired")) {
      return Response.json({ id: "studio.view.retired", kind: "view", version: 2, lifecycle: "retired", payload: SAMPLE_VIEW })
    }
    return Response.json({ code: "host.unavailable", message: "down" }, { status: 503 })
  })
  const listed = await store.listAll()
  expect(listed).toHaveLength(1)
  expect(listed[0]!.scope).toEqual(SCOPE_A)
  expect(seenScopes).toEqual(["org_a/prod", "org_a/prod", "org_a/prod"])

  expect((await store.get("studio.view.a", SCOPE_A))?.scope).toEqual(SCOPE_A)
  expect(await store.get("studio.view.retired", SCOPE_A)).toBeUndefined()

  await expect(cellViews(async (input) => {
    const url = String(input)
    if (url === "/api/me") return meResponse(SCOPE_A)
    if (url.includes("/v1/definitions:list")) return Response.json({ items: [{ id: "studio.view.a" }], cursor: null })
    return Response.json({ code: "host.error", message: "service unavailable" }, { status: 503 })
  }).listAll()).rejects.toThrow()
})

test("view writes send explicitly to their bound scope and refuse when the active workspace changed", async () => {
  let activeScope: StudioScope | null = SCOPE_A
  const hostCalls: { readonly path: string; readonly orgId: string | null; readonly environmentId: string | null; readonly expectedOrg: string | null; readonly expectedEnv: string | null }[] = []
  const store = cellViews(async (input, init) => {
    const url = String(input)
    if (url === "/api/me") return meResponse(activeScope)
    const parsed = new URL(url, "https://studio.test")
    const headers = new Headers(init?.headers)
    hostCalls.push({
      path: parsed.pathname,
      orgId: parsed.searchParams.get("orgId"),
      environmentId: parsed.searchParams.get("environmentId"),
      expectedOrg: headers.get(EXPECTED_ORG_HEADER),
      expectedEnv: headers.get(EXPECTED_ENV_HEADER),
    })
    return parsed.pathname.endsWith(":admit")
      ? Response.json({ admitted: true, restatement: "Saved view.", diagnostics: [] })
      : Response.json({ installed: true, id: "studio.view.v1", version: 2 })
  })

  expect((await store.admit("studio.view.v1", SAMPLE_VIEW, SCOPE_A)).admitted).toBe(true)
  expect(await store.save("studio.view.v1", SAMPLE_VIEW, 1, SCOPE_A)).toEqual({ saved: true, id: "studio.view.v1", version: 2 })
  expect(await store.retire("studio.view.v1", 2, SCOPE_A)).toEqual({ saved: true, id: "studio.view.v1", version: 2 })
  expect(hostCalls).toEqual([
    { path: "/api/host/v1/definitions:admit", orgId: "org_a", environmentId: "prod", expectedOrg: "org_a", expectedEnv: "prod" },
    { path: "/api/host/v1/definitions:install", orgId: "org_a", environmentId: "prod", expectedOrg: "org_a", expectedEnv: "prod" },
    { path: "/api/host/v1/definitions/studio.view.v1:retire", orgId: "org_a", environmentId: "prod", expectedOrg: "org_a", expectedEnv: "prod" },
  ])

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

  const store = cellViews(async (input, init) => {
    const url = String(input)
    if (url === "/api/me") return meResponse(activeScope)
    const parsed = new URL(url, "https://studio.test")
    if (parsed.pathname.endsWith("/v1/definitions:install")) {
      const body = JSON.parse(String(init?.body ?? "{}")) as { expectedVersion?: number; id: string }
      sentExpectedVersions.push(body.expectedVersion)
      // Matches crates/norn-api/src/definition_routes.rs:1376: omitting expectedVersion on an existing definition conflicts.
      if (body.expectedVersion !== serverVersion) {
        return Response.json({ code: "definition.version_conflict", message: "Version conflict." }, { status: 409 })
      }
      serverVersion += 1
      return Response.json({ installed: true, id: body.id, version: serverVersion })
    }
    if (parsed.pathname.endsWith("/v1/definitions/studio.view.v1")) {
      return Response.json({ id: "studio.view.v1", kind: "view", version: serverVersion, lifecycle: "active", payload: SAMPLE_VIEW })
    }
    return Response.json({ code: "not_found" }, { status: 404 })
  })

  const save = (expectedVersion: number | undefined) => saveViewDraft(store, {
    id: "studio.view.v1",
    existing: true,
    view: SAMPLE_VIEW,
    expectedVersion,
    scope: SCOPE_A,
  })

  const first = await save(1)
  expect(first).toMatchObject({ result: { saved: false, conflict: true }, conflict: { kind: "updated", version: 2 } })

  serverVersion = 3
  const raced = await save(first.conflict?.kind === "updated" ? first.conflict.version : undefined)
  expect(raced).toMatchObject({ result: { saved: false, conflict: true }, conflict: { kind: "updated", version: 3 } })

  const overwritten = await save(raced.conflict?.kind === "updated" ? raced.conflict.version : undefined)
  expect(overwritten).toMatchObject({ result: { saved: true, id: "studio.view.v1", version: 4 }, conflict: null })
  expect(sentExpectedVersions).toEqual([1, 2, 3])

  activeScope = SCOPE_B
  await expect(save(4)).rejects.toThrow(StudioScopeError)
  expect(sentExpectedVersions).toEqual([1, 2, 3])
})

test("conflict without a readable current revision leaves conflict unknown so overwrite is withheld", async () => {
  const outcome = await saveViewDraft(cellViews(async (input) => {
    const url = String(input)
    if (url === "/api/me") return meResponse(SCOPE_A)
    if (url.includes(":install")) return Response.json({ code: "definition.version_conflict", message: "Version conflict." }, { status: 409 })
    return Response.json({ code: "host.error", message: "unavailable" }, { status: 503 })
  }), { id: "studio.view.v1", existing: true, view: SAMPLE_VIEW, expectedVersion: 1, scope: SCOPE_A })
  expect(outcome).toMatchObject({ result: { saved: false, conflict: true }, conflict: { kind: "unknown" } })
})

test("ViewEditor alerts when active scope diverges from the loaded draft scope and renders no bulk/row action controls", async () => {
  const renderEditor = (activeScope: StudioScope) => renderRouted(() => createElement(ViewEditor, {
    concepts: [CANON_ITEM],
    stored: { id: "studio.view.canon_item.a", version: 1, scope: SCOPE_A, view: { ...SAMPLE_VIEW, objectType: CANON_ITEM.name, columns: [] } },
    activeScope,
    workspace: liveAt(activeScope),
    cellAvailable: true,
    onSaved: () => {},
  }))

  const same = await renderEditor(SCOPE_A)
  expect(same).not.toContain(WORKSPACE_CHANGED_MESSAGE)
  expect(same).not.toContain("Offered actions")
  expect(same).not.toContain("Bulk action")

  expect(await renderEditor(SCOPE_B)).toContain(WORKSPACE_CHANGED_MESSAGE)
})

test("RecordsGrid preserves a saved view's custom column order instead of reverting to catalog order", async () => {
  const visibleFields = TWO_FIELD_CONCEPT.fields.filter(({ id }) => !HIDDEN_COLS.has(id))
  const reversed = [visibleFields[1]!.id, visibleFields[0]!.id]
  const page = recordsPage(TWO_FIELD_CONCEPT.name, [
    { objectType: TWO_FIELD_CONCEPT.name, id: "item-1", scope: "s", version: 1, values: { [reversed[0]!]: "B", [reversed[1]!]: "A" }, withheld: false },
  ])
  const client = new QueryClient()
  const { reader } = liveAt(SCOPE_A)
  client.setQueryData(reader.records(TWO_FIELD_CONCEPT.name, EMPTY_QUERY).queryKey, page)
  const html = await renderRouted(() => createElement(RecordsGrid, { concept: TWO_FIELD_CONCEPT, initial: { columns: reversed }, reader }), { client })
  const firstIdx = html.indexOf(`>${reversed[0]!.replaceAll("_", " ")}<`)
  const secondIdx = html.indexOf(`>${reversed[1]!.replaceAll("_", " ")}<`)
  expect(firstIdx).toBeGreaterThan(-1)
  expect(secondIdx).toBeGreaterThan(firstIdx)
})

test("bound preview and detail reads pin to the draft scope, refuse on switch between render and dispatch, and discard in-flight reads after a switch", async () => {
  let activeScope: StudioScope | null = SCOPE_A
  const hostCalls: string[] = []
  const gate: { reached: (() => void) | null; release: (() => void) | null } = { reached: null, release: null }
  let blockNextHost = false

  const fetcher: Fetch = async (input) => {
    const url = String(input)
    if (url === "/api/me") return meResponse(activeScope)
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
      objectType: CANON_ITEM.name,
      records: [{ objectType: CANON_ITEM.name, id: `${orgId}-1`, scope: `${orgId}/${environmentId}`, version: 1, values: { label: rowLabel, code: rowLabel }, withheld: false }],
      quarantineCount: 0,
      scannedThrough: null,
      query: { sort: [], filter: null },
      next: null,
      total: { count: 1, approximate: false },
    })
  }
  const store = cellViews(fetcher)
  const view: SavedView = { title: "Scoped preview", objectType: CANON_ITEM.name, columns: ["code"] }
  const previewKey = viewPreviewQueryKey(SCOPE_A, view)

  expect((await store.preview(CANON_ITEM.name, { filter: null, sort: [], limit: 5 }, DECIDED_A)).kind).toBe("page")
  expect(hostCalls).toEqual([`org_a/prod:/api/host/v1/objects/${CANON_ITEM.name}`])

  hostCalls.length = 0
  activeScope = SCOPE_B
  const client = new QueryClient({ defaultOptions: { queries: { refetchOnMount: false, retryOnMount: false } } })
  await expect(client.fetchQuery({
    queryKey: previewKey,
    queryFn: ({ signal }) => store.preview(CANON_ITEM.name, { filter: null, sort: [], limit: 5 }, DECIDED_A, signal),
    retry: false,
  })).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  expect(hostCalls).toHaveLength(0)
  expect(client.getQueryData(previewKey)).toBeUndefined()
  expect(client.getQueryState(previewKey)?.error).toBeInstanceOf(StudioScopeError)

  const html = await renderRouted(() => createElement(ViewEditor, {
    concepts: [CANON_ITEM],
    stored: { id: "studio.view.canon_item.a", version: 1, scope: SCOPE_A, view },
    activeScope: SCOPE_A,
    workspace: LIVE_A,
    cellAvailable: true,
    store,
    onSaved: () => {},
  }), { client })
  expect(html).toContain(WORKSPACE_CHANGED_MESSAGE)
  expect(html).not.toContain("WORKSPACE_B_SECRET_ROW")

  activeScope = SCOPE_A
  blockNextHost = true
  const hostReached = new Promise<void>((resolve) => { gate.reached = resolve })
  const inFlightClient = new QueryClient()
  const pending = inFlightClient.fetchQuery({
    queryKey: previewKey,
    queryFn: ({ signal }) => store.preview(CANON_ITEM.name, { filter: null, sort: [], limit: 5 }, DECIDED_A, signal),
    retry: false,
  })
  await hostReached
  activeScope = SCOPE_B
  gate.release?.()
  await expect(pending).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  expect(inFlightClient.getQueryData(previewKey)).toBeUndefined()

  const gridClient = new QueryClient({ defaultOptions: { queries: { refetchOnMount: false, retryOnMount: false } } })
  const scopedGridKey = ["records", CANON_ITEM.name, SCOPE_A.orgId, SCOPE_A.environmentId, recordsQueryParams(EMPTY_QUERY).toString()] as const
  await expect(gridClient.fetchQuery({
    queryKey: scopedGridKey,
    queryFn: ({ signal }) => store.preview(CANON_ITEM.name, EMPTY_QUERY, DECIDED_A, signal),
    retry: false,
  })).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  expect(gridClient.getQueryData(scopedGridKey)).toBeUndefined()
  const gridHtml = await renderRouted(() => createElement(RecordsGrid, { concept: CANON_ITEM, reader: liveAt(SCOPE_A, fetcher).reader }), { client: gridClient })
  expect(gridHtml).toContain(WORKSPACE_CHANGED_MESSAGE)
  expect(gridHtml).not.toContain("WORKSPACE_B_SECRET_ROW")
})

test("conflict-after-delete reports deletion, hides dead Reload/Overwrite buttons, and supports Save as new view, Discard, and Reload-after-delete", async () => {
  const installs: { readonly id: string; readonly expectedVersion?: number }[] = []
  const store = cellViews(async (input, init) => {
    const url = String(input)
    if (url === "/api/me") return meResponse(SCOPE_A)
    const parsed = new URL(url, "https://studio.test")
    if (parsed.pathname.endsWith("/v1/definitions:install")) {
      const body = JSON.parse(String(init?.body ?? "{}")) as { id: string; expectedVersion?: number }
      installs.push({ id: body.id, expectedVersion: body.expectedVersion })
      return body.id === "studio.view.deleted_by_other"
        ? Response.json({ code: "definition.version_conflict", message: "Version conflict." }, { status: 409 })
        : Response.json({ installed: true, id: body.id, version: 1 })
    }
    if (parsed.pathname.endsWith("/v1/definitions/studio.view.deleted_by_other")) {
      return Response.json({ id: "studio.view.deleted_by_other", kind: "view", version: 2, lifecycle: "retired", payload: SAMPLE_VIEW })
    }
    return Response.json({ code: "definition.not_found" }, { status: 404 })
  })

  const afterDelete = await saveViewDraft(store, {
    id: "studio.view.deleted_by_other",
    existing: true,
    view: { ...SAMPLE_VIEW, title: "My edited view" },
    expectedVersion: 1,
    scope: SCOPE_A,
  })
  expect(afterDelete).toMatchObject({ result: { saved: false, conflict: true }, conflict: { kind: "deleted", version: 2 } })

  const html = await renderRouted(() => createElement(ViewEditor, {
    concepts: [CANON_ITEM],
    stored: { id: "studio.view.deleted_by_other", version: 1, scope: SCOPE_A, view: { ...SAMPLE_VIEW, objectType: CANON_ITEM.name, columns: [] } },
    activeScope: SCOPE_A,
    workspace: LIVE_A,
    cellAvailable: true,
    store,
    initialConflict: afterDelete.conflict,
    onSaved: () => {},
    onReload: async () => "deleted" as const,
    onDiscard: () => {},
  }))
  for (const expected of ["This view was deleted by someone else.", "Save as new view", "Discard my changes"]) expect(html).toContain(expected)
  expect(html).not.toContain("Reload theirs")
  expect(html).not.toContain("Overwrite with mine")

  const freshId = newViewId(SAMPLE_VIEW.objectType, "My edited view", () => "new12345")
  const savedNew = await saveViewDraft(store, {
    id: freshId,
    existing: false,
    view: { ...SAMPLE_VIEW, title: "My edited view" },
    expectedVersion: undefined,
    scope: SCOPE_A,
  })
  expect(savedNew).toMatchObject({ result: { saved: true, id: freshId, version: 1 }, conflict: null })
  expect(installs).toEqual([{ id: "studio.view.deleted_by_other", expectedVersion: 1 }, { id: freshId, expectedVersion: undefined }])

  const loaded: StoredView[] = []
  expect(await reloadConflictingView(async () => ({ isSuccess: true, data: null }), (next) => { loaded.push(next) })).toBe("deleted")
  expect(loaded).toHaveLength(0)

  const updatedView: StoredView = { id: "studio.view.active", version: 2, scope: SCOPE_A, view: SAMPLE_VIEW }
  expect(await reloadConflictingView(async () => ({ isSuccess: true, data: updatedView }), (next) => { loaded.push(next) })).toBe("reloaded")
  expect(loaded).toEqual([updatedView])

  expect(await reloadConflictingView(async () => ({ isSuccess: false, data: undefined }), () => {})).toBe("failed")
})

test("ViewEditor live preview renders withheld records and distinguishes refusal from no matches", async () => {
  const cols = TWO_FIELD_CONCEPT.fields.filter(({ id }) => !HIDDEN_COLS.has(id)).slice(0, 2).map(({ id }) => id)
  const view: SavedView = { title: "Withheld preview", objectType: TWO_FIELD_CONCEPT.name, columns: cols }
  const page = recordsPage(TWO_FIELD_CONCEPT.name, [
    { objectType: TWO_FIELD_CONCEPT.name, id: "rec-hidden", scope: "s", version: 1, values: null, withheld: true },
    { objectType: TWO_FIELD_CONCEPT.name, id: "rec-partial", scope: "s", version: 1, values: { [cols[1]!]: "VISIBLE_PART" }, withheld: false, withheldProperties: [cols[0]!] },
    { objectType: TWO_FIELD_CONCEPT.name, id: "rec-open", scope: "s", version: 1, values: { [cols[0]!]: "OPEN_A", [cols[1]!]: "OPEN_B" }, withheld: false },
  ])
  const client = new QueryClient()
  client.setQueryData(viewPreviewQueryKey(SCOPE_A, view), page)
  const renderEditor = () => renderRouted(() => createElement(ViewEditor, {
    concepts: [TWO_FIELD_CONCEPT],
    stored: { id: "studio.view.withheld", version: 1, scope: SCOPE_A, view },
    activeScope: SCOPE_A,
    workspace: LIVE_A,
    cellAvailable: true,
    onSaved: () => {},
  }), { client })

  const html = await renderEditor()
  for (const expected of ['data-withheld="true"', "rec-hidden", WITHHELD_ROW.replaceAll("'", "&#x27;"), "VISIBLE_PART", "OPEN_A", "OPEN_B"]) {
    expect(html).toContain(expected)
  }

  client.setQueryData(viewPreviewQueryKey(SCOPE_A, view), {
    kind: "refused", status: 422, code: "query.refused", message: "The cell refused this filter.", diagnostics: [],
  } satisfies RecordsRead)
  const refused = await renderEditor()
  expect(refused).toContain("The cell refused this filter.")
  expect(refused).not.toContain("No records currently match this filter.")
})

test("ViewEditor live preview shows no cached rows until a live workspace is decided: Workspace unavailable, or no data cell", async () => {
  const concept = RECORDED_CATALOG.concepts[0]!
  const view: SavedView = { title: "Undecided preview", objectType: concept.name, columns: [] }
  const page = recordsPage(concept.name, [{ objectType: concept.name, id: "CACHED_ROW", scope: "s", version: 1, values: { label: "CACHED_ROW" }, withheld: false }])
  const render = (workspace: Workspace) => {
    const client = new QueryClient()
    client.setQueryData(viewPreviewQueryKey(SCOPE_A, view), page)
    return renderRouted(() => createElement(ViewEditor, {
      concepts: [concept],
      stored: { id: "studio.view.undecided", version: 1, scope: SCOPE_A, view },
      activeScope: SCOPE_A,
      workspace,
      cellAvailable: true,
      onSaved: () => {},
    }), { client })
  }

  expect(await render(LIVE_A)).toContain("CACHED_ROW")

  const unavailable = await render({ kind: "unavailable", error: new CellUnavailable(503, "The workspace cell could not be opened.") })
  expect(unavailable).toContain("data-workspace-unavailable")
  expect(unavailable).toContain(">Retry<")
  expect(unavailable).not.toContain("CACHED_ROW")
  expect(unavailable).not.toContain("Checking matching records…")

  expect(await render({ kind: "deciding" })).not.toContain("CACHED_ROW")

  const recorded = await render(workspaceOf({ status: "success", data: { kind: "recorded" } }))
  expect(recorded).toContain(NO_DATA_CELL)
  expect(recorded).not.toContain("CACHED_ROW")
  expect(recorded).not.toContain("Checking matching records…")
})

test("ViewDetailPage scopes deleted-view banner to the deleted view's id so navigating to another view renders it normally", async () => {
  const removedA = { id: "studio.view.a", version: 2, scope: SCOPE_A, view: { ...SAMPLE_VIEW, title: "Deleted View A", objectType: CANON_ITEM.name } }
  const storedB: StoredView = { id: "studio.view.b", version: 1, scope: SCOPE_A, view: { ...SAMPLE_VIEW, title: "Active View B", objectType: CANON_ITEM.name, columns: [] } }

  expect(scopedDeletedView(removedA, "studio.view.a", SCOPE_A)).toEqual(removedA)
  expect(scopedDeletedView(removedA, "studio.view.b", SCOPE_A)).toBeUndefined()
  expect(scopedDeletedView(removedA, "studio.view.a", SCOPE_B)).toBeUndefined()

  deletionReceipts.clear()
  deletionReceipts.add({ ...removedA, reader: sessionKey(undefined) })
  const renderAt = (routeId: string) => {
    const client = new QueryClient()
    client.setQueryData(["studio-scope"], SCOPE_A)
    client.setQueryData(["definitions"], RECORDED_ARTIFACTS)
    client.setQueryData(["view", "studio.view.b", SCOPE_A.orgId, SCOPE_A.environmentId], storedB)
    return renderRouted(() => createElement(ViewDetail), { client, path: "/views/$id", initialEntry: `/views/${routeId}` })
  }

  const htmlB = await renderAt("studio.view.b")
  expect(htmlB).toContain("Active View B")
  expect(htmlB).not.toContain("Deleted “Deleted View A”")

  const htmlA = await renderAt("studio.view.a")
  expect(htmlA).toContain("Deleted “Deleted View A”")
  expect(htmlA).toContain("Undo")
  deletionReceipts.clear()
})

test("ViewsBar keeps Undo available when restoration fails and clears it only after restoration succeeds", async () => {
  let activeScope: StudioScope = SCOPE_A
  let refuseNext = false
  const store = cellViews(async (input) => {
    if (String(input) === "/api/me") return meResponse(activeScope)
    if (refuseNext) {
      refuseNext = false
      return Response.json({ diagnostics: [{ code: "definition.refused", message: "Temporary refusal." }] }, { status: 422 })
    }
    return Response.json({ installed: true, id: "studio.view.v1", version: 3 })
  })
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

  activeScope = SCOPE_B
  await current.undo!()
  expect(current.text).toContain(WORKSPACE_CHANGED_MESSAGE)
  expect(current.undo).toBeDefined()
  expect(restored).toHaveLength(0)

  activeScope = SCOPE_A
  refuseNext = true
  await current.undo!()
  expect(current.text).toContain("Temporary refusal.")
  expect(current.undo).toBeDefined()
  expect(restored).toHaveLength(0)

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
  const withheld = row({}, true)
  const changed = row({ label: "Alpha (renamed)" })
  expect(visibleDrawerRow("a", [first], null)).toBe(first)
  expect(visibleDrawerRow("a", [withheld], null)).toBe(withheld)
  expect(visibleDrawerRow("a", [changed], null)?.record.values).toEqual({ label: "Alpha (renamed)" })
  expect(visibleDrawerRow("a", [row({ label: "Beta" }, false, "b")], null)).toBeNull()
  expect(visibleDrawerRow("a", [first], null, true)).toBeNull()
  expect(visibleDrawerRow("a", [first], new StudioScopeError(WORKSPACE_CHANGED_MESSAGE))).toBeNull()
  expect(visibleDrawerRow(null, [first], null)).toBeNull()
})

test("an authorization refusal on a refetch withdraws the cached page and closes the drawer", () => {
  const row = { record: { objectType: "canon.Item", id: "a", scope: "s", version: 1, values: { label: "Alpha" }, withheld: false }, at: "/records/0", numbers: new Map<string, string>() }
  for (const status of [401, 403]) {
    const refused = new CellUnavailable(status)
    expect(readWithdrawn(refused)).toBe(true)
    expect(visibleDrawerRow("a", [row], refused)).toBeNull()
  }
  expect(readWithdrawn(new StudioScopeError(WORKSPACE_CHANGED_MESSAGE))).toBe(true)
  for (const status of [0, 500, 503]) {
    expect(readWithdrawn(new CellUnavailable(status))).toBe(false)
    expect(visibleDrawerRow("a", [row], new CellUnavailable(status))).toBe(row)
  }
  expect(readWithdrawn(null)).toBe(false)
})

test("a detail definition read stays pinned before dispatch and discards a late result from its former workspace", async () => {
  let active = SCOPE_A
  const fetched: (string | null)[] = []
  const store = cellViews(async (input) => {
    fetched.push(new URL(String(input), "https://studio.test").searchParams.get("orgId"))
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

  const brokenStream = new ReadableStream<Uint8Array>({
    start(ctrl) { ctrl.error(new CellUnavailable(0, "Stream timed out", { timedOut: true, retryable: true, code: "cell.timeout" })) },
  })
  const streamErr = await cellViews(
    async () => new Response(brokenStream, { status: 200, headers: { "content-type": "application/json" } }),
    async () => SCOPE_A,
  ).get("studio.view.v1", SCOPE_A).catch((e: unknown) => e)
  expect(streamErr).toBeInstanceOf(CellUnavailable)
  expect((streamErr as CellUnavailable).timedOut).toBe(true)

  const previewErr = await readRecordsAtScope("demo.Record", EMPTY_QUERY, DECIDED_A, async () =>
    Response.json({ code: "cell.unopenable", message: "Cell is temporarily sealed.", retryable: false }, { status: 503 }),
  ).catch((e: unknown) => e)
  expect(previewErr).toBeInstanceOf(CellUnavailable)
  expect(previewErr).toMatchObject({ status: 503, code: "cell.unopenable", retryable: false })

  const facetSignals: (AbortSignal | null | undefined)[] = []
  const facetAbort = new AbortController()
  const facetQuery = facetsQuery({ objectType: "demo.Record", filter: null, scope: SCOPE_A }, async (input, init) => {
    facetSignals.push(init?.signal)
    if (String(input) === "/api/me") return meResponse(SCOPE_A)
    facetAbort.abort(new DOMException("Cancelled", "AbortError"))
    throw facetAbort.signal.reason
  })
  const facetErr = await Promise.resolve(facetQuery.queryFn!({ signal: facetAbort.signal, queryKey: facetQuery.queryKey, meta: undefined, client: new QueryClient() } as never)).catch((e: unknown) => e)
  expect((facetErr as Error).name).toBe("AbortError")
  expect(facetSignals.length).toBeGreaterThanOrEqual(2)
  expect(facetSignals.every((s) => s !== undefined)).toBe(true)
})

test("a definition of another kind is not a view, so the detail never wires its controls to it", async () => {
  const store = cellViews(async () => Response.json({ id: "ops.automation.a", kind: "automation", version: 1, lifecycle: "active", payload: { title: "Restock", objectType: "demo.Record" } }))
  expect(await store.get("ops.automation.a", SCOPE_A)).toBeUndefined()
})

test.each([
  ["an empty body", ""],
  ["a body that is not JSON", "<html>gateway</html>"],
  ["a body without a kind", JSON.stringify({ id: "studio.view.a", version: 1, lifecycle: "active", payload: SAMPLE_VIEW })],
  ["a kind that is not a string", JSON.stringify({ id: "studio.view.a", kind: 7, version: 1, lifecycle: "active", payload: SAMPLE_VIEW })],
  ["another kind without a version", JSON.stringify({ id: "studio.view.a", kind: "automation", lifecycle: "active" })],
])('a 200 with %s is an unavailable read, never "No view"', async (_case, body) => {
  const store = cellViews(async () => new Response(body, { status: 200 }))
  await expect(store.get("studio.view.a", SCOPE_A)).rejects.toBeInstanceOf(CellUnavailable)
  await expect(store.get("studio.view.a", SCOPE_A)).rejects.toThrow(UNREADABLE_VIEW)
})

test("listAll reads every detail with at most VIEW_READ_CONCURRENCY in flight, in the cell's order", async () => {
  const total = 40
  let inFlight = 0
  let peak = 0
  const listed = await cellViews(async (input) => {
    const url = new URL(String(input), "https://studio.test")
    if (url.pathname.endsWith("/v1/definitions:list")) {
      return Response.json({ items: Array.from({ length: total }, (_, index) => ({ id: `studio.view.${index}` })), cursor: null })
    }
    inFlight += 1
    peak = Math.max(peak, inFlight)
    const id = decodeURIComponent(url.pathname.split("/v1/definitions/")[1] ?? "")
    await new Promise((resolve) => setTimeout(resolve, total - Number(id.split(".").at(-1))))
    inFlight -= 1
    return Response.json({ id, kind: "view", version: 1, lifecycle: "active", payload: { ...SAMPLE_VIEW, title: id } })
  }).listAll(SCOPE_A)
  expect(listed.map(({ id }) => id)).toEqual(Array.from({ length: total }, (_, index) => `studio.view.${index}`))
  expect(peak).toBe(VIEW_READ_CONCURRENCY)
})

test("a failed detail read stops further reads and fails the listing", async () => {
  const started: number[] = []
  const failed = await mapBounded([0, 1, 2, 3, 4, 5, 6, 7], 2, async (index) => {
    started.push(index)
    await new Promise((resolve) => setTimeout(resolve, 1))
    if (index === 1) throw new CellUnavailable(503)
    return index
  }).catch((error: unknown) => error)
  expect(failed).toBeInstanceOf(CellUnavailable)
  expect(started.length).toBeLessThan(8)
  expect(await mapBounded([], 6, async (value: number) => value)).toEqual([])
  for (const limit of [0, -1, Number.NaN]) await expect(mapBounded([1, 2], limit, async (value) => value)).rejects.toBeInstanceOf(RangeError)
})

test("listAll follows the cursor to the end and refuses a listing that repeats a page", async () => {
  const pages = 12
  const listCalls: (string | null)[] = []
  const fetcher = (repeat: boolean): Fetch => async (input) => {
    const url = new URL(String(input), "https://studio.test")
    if (url.pathname.endsWith("/v1/definitions:list")) {
      const cursor = url.searchParams.get("cursor")
      listCalls.push(cursor)
      const index = cursor === null ? 0 : Number(cursor.slice(1))
      return Response.json({ items: [{ id: `studio.view.${index}` }], cursor: repeat ? "p1" : index + 1 < pages ? `p${index + 1}` : null })
    }
    const id = decodeURIComponent(url.pathname.split("/v1/definitions/")[1] ?? "")
    return Response.json({ id, kind: "view", version: 1, lifecycle: "active", payload: { ...SAMPLE_VIEW, title: id } })
  }
  expect(await cellViews(fetcher(false)).listAll(SCOPE_A)).toHaveLength(pages)
  expect(listCalls).toHaveLength(pages)
  listCalls.length = 0
  await expect(cellViews(fetcher(true)).listAll(SCOPE_A)).rejects.toBeInstanceOf(CellUnavailable)
  expect(listCalls).toEqual([null, "p1"])
})

test("a deletion answered after the reader or workspace changed leaves no receipt for the next reader", () => {
  const gone = { id: "studio.view.a", version: 3, scope: SCOPE_A, view: SAMPLE_VIEW }
  const alice = { reader: "alice-admin", scope: SCOPE_A }
  expect(deletionReceiptFor(gone, alice, alice)).toEqual({ ...gone, reader: "alice-admin" })
  for (const next of [
    { reader: "alice-viewer", scope: SCOPE_A },
    { reader: "bob", scope: SCOPE_A },
    { reader: "alice-admin", scope: SCOPE_A_STAGING },
    { reader: "alice-admin", scope: null },
  ]) {
    expect(deletionReceiptFor(gone, alice, next)).toBeUndefined()
  }
  expect(deletionReceiptFor({ ...gone, scope: SCOPE_B }, alice, alice)).toBeUndefined()
  expect(deletionReceiptFor(undefined, alice, alice)).toBeUndefined()
})

test("a newer active revision of a deleted view supersedes its receipt", () => {
  const receipt = { id: "studio.view.a", version: 3, scope: SCOPE_A, view: SAMPLE_VIEW, reader: "alice" }
  const served = (version: number, scope = SCOPE_A, id = "studio.view.a") => ({ id, version, scope, view: SAMPLE_VIEW })
  expect(receiptSuperseded(receipt, served(4))).toBe(true)
  for (const candidate of [served(2), served(3), served(9, SCOPE_B), served(9, SCOPE_A, "studio.view.b"), null, undefined]) {
    expect(receiptSuperseded(receipt, candidate)).toBe(false)
  }
})

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
  deletionReceipts.remove({ reader: "alice", scope: SCOPE_A, id: "studio.view.a" })
  deletionReceipts.add({ ...gone("studio.view.b", SCOPE_A), version: 4 })
  expect(deletionReceipts.of("alice", SCOPE_A).map(({ id, version }) => [id, version])).toEqual([["studio.view.b", 4]])
  deletionReceipts.clear()
})
