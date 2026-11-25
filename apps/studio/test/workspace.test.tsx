import { expect, mock, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { renderToStaticMarkup } from "react-dom/server"
import { CellUnavailable, studioScopeKey, StudioScopeError, WORKSPACE_CHANGED_MESSAGE, type StudioScope } from "../src/lib/definitions.ts"
import type { Mode } from "../src/lib/live-transport.ts"
import { EMPTY_QUERY } from "../src/lib/records-query.ts"

// The recorded transport reads fixtures through Vite's import.meta.glob, which bun test lacks.
mock.module("../src/lib/transport.ts", () => ({ transport: {} }))
const { FINDINGS_STALE_MS, readPage, untilDecided, useWorkspace, workspaceOf, WorkspaceProvider } = await import("../src/lib/workspace.tsx")
const { WorkspaceUndecided } = await import("../src/components/load-state.tsx")
const { ViewProvider } = await import("../src/lib/view.tsx")
type Workspace = import("../src/lib/workspace.tsx").Workspace

const A: StudioScope = { orgId: "org_a", environmentId: "prod" }
const B: StudioScope = { orgId: "org_b", environmentId: "prod" }
const liveMode = (scope: StudioScope): Mode => ({ kind: "live", scope, catalog: { document: [], numbers: new Map(), origin: "live" }, counts: {}, publishedQuestions: new Set() })
const signal = new AbortController().signal

/** A cell at `active()`'s workspace: `/api/me` names it, and every host call is recorded with the scope it was sent to. */
function cell(active: () => StudioScope, answer: (url: URL) => Response) {
  const sent: string[] = []
  let me = 0
  const fetcher = async (input: string) => {
    const url = new URL(input, "https://studio.test")
    if (url.pathname === "/api/me") {
      me += 1
      const { orgId, environmentId } = active()
      return Response.json({ user: { id: "u" }, organization: { id: orgId, role: "admin" }, environmentId })
    }
    sent.push(`${url.pathname.replace(/^\/api\/host/, "")} ${url.searchParams.get("orgId")}/${url.searchParams.get("environmentId")}`)
    return answer(url)
  }
  return { sent, fetcher, me: () => me }
}

const page = (records: readonly unknown[]) => Response.json({ objectType: "canon.Item", records, quarantineCount: 0, scannedThrough: null, query: { sort: [], filter: null }, next: null, total: { count: records.length, approximate: false } })

test("the provider's union is the mode answer's: deciding, unavailable, live with a decided scope and its reader, recorded with the recording's", () => {
  expect(workspaceOf({ status: "pending" })).toEqual({ kind: "deciding" })
  const error = new CellUnavailable(503, "The workspace cell could not be opened.")
  expect(workspaceOf({ status: "error", error })).toEqual({ kind: "unavailable", error })
  const recorded = workspaceOf({ status: "success", data: { kind: "recorded" } })
  expect(recorded.kind).toBe("recorded")
  expect(recorded.kind === "recorded" && recorded.reader.kind).toBe("recorded")
  expect(Object.keys(recorded)).toEqual(["kind", "reader"])
  const live = workspaceOf({ status: "success", data: liveMode(A) })
  if (live.kind !== "live") throw new Error("expected a live workspace")
  expect(Object.keys(live)).toEqual(["kind", "scope", "reader"])
  expect(live.scope).toEqual(A)
  // The decided scope is a copy of the answer's, frozen, and the reader is bound to it.
  expect(live.scope).not.toBe(A)
  expect(Object.isFrozen(live.scope)).toBe(true)
  expect(live.reader.kind).toBe("live")
  expect(live.reader.scope).toBe(live.scope)
  // One workspace per decided scope: a later answer for the same workspace keeps the page's reader.
  expect(workspaceOf({ status: "success", data: liveMode({ ...A }) })).toBe(live)
  const other = workspaceOf({ status: "success", data: liveMode(B) })
  expect(other).not.toBe(live)
  expect(other.kind === "live" && studioScopeKey(other.scope)).toBe(studioScopeKey(B))
})

test("outside a provider nothing is decided; a provider given a workspace provides it", () => {
  let seen: Workspace | undefined
  const Probe = () => { seen = useWorkspace(); return null }
  renderToStaticMarkup(<Probe />)
  expect(seen).toEqual({ kind: "deciding" })
  const live = workspaceOf({ status: "success", data: liveMode(A) })
  renderToStaticMarkup(<WorkspaceProvider workspace={live}><Probe /></WorkspaceProvider>)
  expect(seen).toBe(live)
})

test("a section before its workspace is decided shows its skeleton, or Workspace unavailable with Retry, and reads nothing", () => {
  const render = (workspace: Extract<Workspace, { readonly kind: "deciding" | "unavailable" }>) =>
    renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><ViewProvider><WorkspaceUndecided workspace={workspace} bone="records-table" loadingLabel="Reading the records" /></ViewProvider></QueryClientProvider>)
  const deciding = render({ kind: "deciding" })
  expect(deciding).toContain('data-bone="records-table"')
  expect(deciding).toContain('aria-label="Reading the records"')
  const failed = render({ kind: "unavailable", error: new CellUnavailable(503, "The workspace cell could not be opened.") })
  expect(failed).toContain("data-workspace-unavailable")
  expect(failed).toContain("Workspace unavailable")
  expect(failed).toContain(">Retry<")
})

test("a live reader keys every read by its workspace and sends it there, checking /api/me before and after", async () => {
  let active = A
  const { sent, fetcher, me } = cell(() => active, (url) => url.pathname.endsWith("/findings")
    ? Response.json({ objectType: "canon.Item", id: "i/1", quarantined: false, findings: [], evidence: [] })
    : page([{ objectType: "canon.Item", id: "i/1", scope: "s", version: 1, values: { label: `of ${active.orgId}` }, withheld: false }]))
  const live = workspaceOf({ status: "success", data: liveMode(A) }, fetcher)
  if (live.kind !== "live") throw new Error("expected a live workspace")
  const { reader } = live
  const records = reader.records("canon.Item", EMPTY_QUERY)
  expect(records.queryKey).toEqual(["records", "canon.Item", "org_a", "prod", "limit=50"])
  expect(reader.object("canon.Item", "i/1").queryKey).toEqual(["object", "canon.Item", "i/1", "org_a", "prod"])
  const findings = reader.findings("canon.Item", "i/1")
  expect(findings.queryKey).toEqual(["findings", "canon.Item", "i/1", "org_a", "prod"])
  // The record page's header and its Findings panel share one read; the panel mounting does not read it again (#1754).
  expect(findings.staleTime).toBe(FINDINGS_STALE_MS)
  const read = await records.queryFn({ signal })
  expect(read.kind === "page" && read.origin).toBe("live")
  expect(sent).toEqual(["/v1/objects/canon.Item org_a/prod"])
  expect(me()).toBe(2)
  expect((await findings.queryFn({ signal })).document.quarantined).toBe(false)
  expect(sent).toEqual(["/v1/objects/canon.Item org_a/prod", "/v1/objects/canon.Item/i%2F1/findings org_a/prod"])
  // The session moves to another workspace: every read is refused before it is sent.
  active = B
  sent.length = 0
  await expect(records.queryFn({ signal })).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  await expect(reader.object("canon.Item", "i/1").queryFn({ signal })).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  await expect(findings.queryFn({ signal })).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  expect(sent).toEqual([])
  // A plain page read (the Timeline's, a record picker's) is the same read.
  active = A
  expect((await readPage(reader)("canon.Item", EMPTY_QUERY)).kind).toBe("page")
})

// #1754: a decided recorded mode reads only the recording, even from a session whose cell serves records.
test("the recorded reader never reaches a cell, under keys no live answer reaches, and keeps no findings", async () => {
  const realFetch = globalThis.fetch
  const asked: string[] = []
  globalThis.fetch = (async (input: string | URL | Request) => { asked.push(String(input)); return page([]) }) as typeof fetch
  try {
    const recorded = workspaceOf({ status: "success", data: { kind: "recorded" } })
    if (recorded.kind !== "recorded") throw new Error("expected the recorded workspace")
    const { reader } = recorded
    expect(reader.records("canon.Item", EMPTY_QUERY).queryKey).toEqual(["records", "canon.Item", "recorded", "limit=50"])
    expect(reader.object("canon.Item", "i/1").queryKey).toEqual(["object", "recorded", "canon.Item", "i/1"])
    expect("findings" in reader).toBe(false)
    expect("scope" in reader).toBe(false)
    // The recording never sorts or filters: such a query is refused, and nothing is asked of a cell.
    const sorted = await reader.records("canon.Item", { ...EMPTY_QUERY, sort: [{ property: "code", direction: "asc" }] }).queryFn({ signal })
    expect(sorted).toMatchObject({ kind: "refused", code: "studio.query_unavailable" })
    expect(asked).toEqual([])
  } finally {
    globalThis.fetch = realFetch
  }
})

test("a read asked before its workspace is decided waits under a key of its own and reads nothing", async () => {
  const waiting = untilDecided(undefined, ["object", "canon.Item", "i/1"])
  expect(waiting.enabled).toBe(false)
  expect(waiting.queryKey).toEqual(["undecided", "object", "canon.Item", "i/1"])
  await expect(waiting.queryFn({ signal })).rejects.toBeInstanceOf(StudioScopeError)
  const live = workspaceOf({ status: "success", data: liveMode(A) })
  if (live.kind !== "live") throw new Error("expected a live workspace")
  const decided = untilDecided(live.reader.object("canon.Item", "i/1"), ["object", "canon.Item", "i/1"])
  expect(decided.enabled).toBe(true)
  expect(decided.queryKey).toEqual(live.reader.object("canon.Item", "i/1").queryKey)
})
