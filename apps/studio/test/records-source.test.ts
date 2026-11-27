import { expect, mock, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { readDocument } from "../src/lib/exact.ts"
import type { StudioScope } from "../src/lib/definitions.ts"
import type { ObjectPage } from "../src/lib/transport.ts"
import served from "./fixtures/object-query.json"

// The recorded transport reads fixtures through Vite's import.meta.glob, which bun test lacks.
mock.module("../src/lib/transport.ts", () => ({ transport: {} }))
const { diagnosticsAt } = await import("../src/lib/records-source.ts")
const { readRecordedRecords, readRecordsAtScope, recordedPage } = await import("../src/lib/raw/records.ts")
const { workspaceOf } = await import("../src/lib/workspace.tsx")
const { EMPTY_QUERY } = await import("../src/lib/records-query.ts")
const { CellUnavailable } = await import("../src/lib/definitions.ts")

const page = (name: string) => readDocument<ObjectPage>(readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/objects", name), "utf8"), "recorded")

/** The workspace a live mode decided at `scope`: the only way to a DecidedScope. */
function decided(scope: StudioScope = { orgId: "org_demo", environmentId: "analytics" }) {
  const workspace = workspaceOf({ status: "success", data: { kind: "live", scope, catalog: { document: [], numbers: new Map(), origin: "live" }, counts: {}, publishedQuestions: new Set<string>(), builtins: [] } })
  if (workspace.kind !== "live") throw new Error("expected a live workspace")
  return workspace.scope
}

/** A cell answering the object route, recording every call it saw; it never answers `/api/me`. */
function cell(host: (url: URL) => Response) {
  const calls: URL[] = []
  const fetcher = async (input: string) => {
    const url = new URL(input, "https://studio.test")
    if (url.pathname === "/api/me") throw new Error("the raw read never reads the session's scope")
    calls.push(url)
    return host(url)
  }
  return { calls, fetcher }
}

test("the live route gets the decided scope and the query; its echo is what was applied", async () => {
  const live = { objectType: "canon.Item", records: [{ objectType: "canon.Item", id: "a", scope: "s", version: 1, values: null, withheld: true }], quarantineCount: 0, scannedThrough: null,
    query: { sort: [{ property: "code", direction: "desc" }], filter: null }, next: "c2", total: { count: 41, approximate: true } }
  const { calls, fetcher } = cell(() => Response.json(live))
  const read = await readRecordsAtScope("canon.Item", { ...EMPTY_QUERY, sort: [{ property: "code", direction: "desc" }] }, decided(), fetcher)
  const url = calls[0]
  expect(url?.pathname).toBe("/api/host/v1/objects/canon.Item")
  expect(url?.searchParams.get("orgId")).toBe("org_demo")
  expect(url?.searchParams.get("environmentId")).toBe("analytics")
  expect(url?.searchParams.get("sort")).toBe("code:desc")
  expect(read.kind === "page" && read.origin).toBe("live")
  expect(read.kind === "page" && read.served).toEqual({ sort: [{ property: "code", direction: "desc" }], filter: null })
  expect(read.kind === "page" && read.next).toBe("c2")
  expect(read.kind === "page" && read.total).toEqual({ count: 41, approximate: true })
})

// #1754: a workspace's cell that does not serve the route is that, never the recording in its place.
test("a route the cell does not serve is none, never the recording; a sorted or filtered query is refused", async () => {
  const { fetcher } = cell(() => Response.json({ code: "route.not_found", diagnostics: [null] }, { status: 404 }))
  expect(await readRecordsAtScope("canon.Item", EMPTY_QUERY, decided(), fetcher)).toEqual({ kind: "none" })
  const filtered = await readRecordsAtScope("canon.Item", { ...EMPTY_QUERY, filter: { all: [{ property: "code", op: "eq", value: "GOLD" }] } }, decided(), fetcher)
  expect(filtered).toMatchObject({ kind: "refused", code: "studio.query_unavailable" })
})

// #1700 S14 (closed by #1643): a structured 503 keeps its reason and incident id; a bare one is unavailable too.
test("a 503 is the cell unavailable, its reason and incident kept", async () => {
  const structured = cell(() => Response.json({ code: "cell.unopenable", message: "The cell is sealed for maintenance.", incidentId: "inc_42", retryable: false }, { status: 503 }))
  const error = await readRecordsAtScope("canon.Item", EMPTY_QUERY, decided(), structured.fetcher).then(() => null, (thrown: unknown) => thrown)
  expect(error).toBeInstanceOf(CellUnavailable)
  expect(error).toMatchObject({ status: 503, code: "cell.unopenable", incidentId: "inc_42", retryable: false, message: "The cell is sealed for maintenance." })
  const bare = cell(() => new Response(null, { status: 503 }))
  await expect(readRecordsAtScope("canon.Item", EMPTY_QUERY, decided(), bare.fetcher)).rejects.toBeInstanceOf(CellUnavailable)
})

test("a moved snapshot restarts; a bad cursor is a refusal with its pointer", async () => {
  const moved = cell(() => Response.json(served.snapshotMoved.json, { status: served.snapshotMoved.status }))
  expect(await readRecordsAtScope("canon.Item", { ...EMPTY_QUERY, after: "c" }, decided(), moved.fetcher)).toEqual({ kind: "restart" })
  const bad = cell(() => Response.json(served.cursorInvalid.json, { status: served.cursorInvalid.status }))
  const read = await readRecordsAtScope("canon.Item", { ...EMPTY_QUERY, after: "c" }, decided(), bad.fetcher)
  expect(read).toMatchObject({ kind: "refused", status: 400, code: "query.invalid", diagnostics: [{ code: "query.cursor_invalid", pointer: "/after" }] })
})

test("every diagnostic is kept and found by the part of the request it points at", async () => {
  const diagnostics = [
    { code: "query.value_invalid", message: "Not an exact decimal.", pointer: "/filter/all/1/value" },
    { code: "query.op_invalid", message: "Booleans only take is.", pointer: "/filter/all/10/op" },
    { code: "query.sort_invalid", message: "Records can't be sorted by notes.", pointer: "/sort/0" },
  ]
  const { fetcher } = cell(() => Response.json({ code: "query.invalid", message: "The cell refused this query.", diagnostics }, { status: 400 }))
  const read = await readRecordsAtScope("canon.Item", EMPTY_QUERY, decided(), fetcher)
  if (read.kind !== "refused") throw new Error(read.kind)
  expect(read.diagnostics).toHaveLength(3)
  expect(diagnosticsAt(read.diagnostics, "/filter/all/1").map(({ code }) => code)).toEqual(["query.value_invalid"])
  expect(diagnosticsAt(read.diagnostics, "/sort").map(({ pointer }) => pointer)).toEqual(["/sort/0"])
})

test("a recorded page says it is recorded and applied nothing; the recording is never asked to sort or filter", async () => {
  const first = recordedPage(page("canon.Item.page.json"))
  expect(first.kind === "page" && first.origin).toBe("recorded")
  expect(first.kind === "page" && first.served).toBeNull()
  expect(first.kind === "page" && first.next).toBe("demo/item/gold")
  expect(recordedPage(null)).toEqual({ kind: "none" })
  expect(await readRecordedRecords("canon.Item", { ...EMPTY_QUERY, sort: [{ property: "code", direction: "asc" }] })).toMatchObject({ kind: "refused", code: "studio.query_unavailable" })
})
