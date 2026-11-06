import { expect, mock, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { readDocument, type Read } from "../src/lib/exact.ts"
import type { ObjectPage } from "../src/lib/transport.ts"

// The recorded transport reads fixtures through Vite's import.meta.glob, which bun test lacks; recordings are passed in below.
mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { diagnosticsAt, readRecords } = await import("../src/lib/records-source.ts")
const { EMPTY_QUERY } = await import("../src/lib/records-query.ts")

const page = (name: string) => readDocument<ObjectPage>(readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/objects", name), "utf8"), "recorded")
const ME = { user: { id: "u" }, organization: { id: "org_demo", name: "Demo", role: "viewer" }, environmentId: "analytics" }

/** A fetch that answers /api/me and the object route, recording every host call it saw. */
function cell(host: (url: URL) => Response, me: unknown = ME, meStatus = 200) {
  const calls: URL[] = []
  const fetcher = async (input: string) => {
    const url = new URL(input, "https://studio.test")
    if (url.pathname === "/api/me") return Response.json(me, { status: meStatus })
    calls.push(url)
    return host(url)
  }
  const recorded = mock(async (_type: string, after?: string): Promise<Read<ObjectPage> | null> => after === undefined ? page("canon.Item.page.json") : page("canon.Item.after.demo~item~gold.page.json"))
  return { calls, deps: { fetcher, recorded } }
}

test("a workspace without a data cell makes no host call", async () => {
  const { calls, deps } = cell(() => { throw new Error("no call expected") }, { ...ME, environmentId: null })
  expect(await readRecords("canon.Item", EMPTY_QUERY, deps)).toEqual({ kind: "no-cell" })
  expect(calls).toHaveLength(0)
})

test("the live route gets the scope and the query; its echo is what was applied", async () => {
  const live = { objectType: "canon.Item", records: [{ objectType: "canon.Item", id: "a", scope: "s", version: 1, values: null, withheld: true }], quarantineCount: 0, scannedThrough: null,
    query: { sort: [{ property: "code", direction: "desc" }], filter: null }, next: "c2", total: { count: 41, approximate: true } }
  const { calls, deps } = cell(() => Response.json(live))
  const read = await readRecords("canon.Item", { ...EMPTY_QUERY, sort: [{ property: "code", direction: "desc" }] }, deps)
  const url = calls[0]
  expect(url?.pathname).toBe("/api/host/v1/objects/canon.Item")
  expect(url?.searchParams.get("orgId")).toBe("org_demo")
  expect(url?.searchParams.get("environmentId")).toBe("analytics")
  expect(url?.searchParams.get("sort")).toBe("code:desc")
  expect(read.kind === "page" && read.origin).toBe("live")
  expect(read.kind === "page" && read.served).toEqual({ sort: [{ property: "code", direction: "desc" }], filter: null })
  expect(read.kind === "page" && read.next).toBe("c2")
  expect(read.kind === "page" && read.total).toEqual({ count: 41, approximate: true })
  expect(deps.recorded).not.toHaveBeenCalled()
})

test("while the cell doesn't serve the route, the recording is read and says it applied nothing", async () => {
  const { deps } = cell(() => new Response("Not found", { status: 404 }))
  const first = await readRecords("canon.Item", EMPTY_QUERY, deps)
  expect(first.kind === "page" && first.origin).toBe("recorded")
  expect(first.kind === "page" && first.served).toBeNull()
  expect(first.kind === "page" && first.next).toBe("demo/item/gold")
  const second = await readRecords("canon.Item", { ...EMPTY_QUERY, after: "demo/item/gold" }, deps)
  expect(second.kind === "page" && second.next).toBeUndefined()
})

test("a sorted or filtered query is never answered from the recording", async () => {
  const { deps } = cell(() => new Response(null, { status: 503 }))
  const read = await readRecords("canon.Item", { ...EMPTY_QUERY, filter: { all: [{ property: "code", op: "eq", value: "GOLD" }] } }, deps)
  expect(read).toMatchObject({ kind: "refused", code: "studio.query_unavailable" })
  expect(deps.recorded).not.toHaveBeenCalled()
})

test("signed out (no session to read), only the recording is shown", async () => {
  const { calls, deps } = cell(() => { throw new Error("no call expected") }, { error: "Sign in first." }, 401)
  const read = await readRecords("canon.Item", EMPTY_QUERY, deps)
  expect(read.kind === "page" && read.origin).toBe("recorded")
  expect(calls).toHaveLength(0)
})

test("a moved snapshot restarts; a bad cursor is a refusal with its pointer", async () => {
  const moved = cell(() => Response.json({ code: "object.snapshot_moved", message: "moved" }, { status: 409 }))
  expect(await readRecords("canon.Item", { ...EMPTY_QUERY, after: "c" }, moved.deps)).toEqual({ kind: "restart" })
  const bad = cell(() => Response.json({ code: "query.cursor_invalid", message: "That page link is no longer valid.", diagnostics: [{ code: "query.cursor_invalid", message: "Unknown cursor.", pointer: "/after" }] }, { status: 400 }))
  const read = await readRecords("canon.Item", { ...EMPTY_QUERY, after: "c" }, bad.deps)
  expect(read).toMatchObject({ kind: "refused", status: 400, code: "query.cursor_invalid", diagnostics: [{ pointer: "/after" }] })
})

test("every diagnostic is kept and found by the part of the request it points at", async () => {
  const diagnostics = [
    { code: "query.value_invalid", message: "Not an exact decimal.", pointer: "/filter/all/1/value" },
    { code: "query.op_invalid", message: "Booleans only take is.", pointer: "/filter/all/10/op" },
    { code: "query.sort_invalid", message: "Records can't be sorted by notes.", pointer: "/sort/0" },
  ]
  const { deps } = cell(() => Response.json({ code: "query.invalid", message: "The cell refused this query.", diagnostics }, { status: 400 }))
  const read = await readRecords("canon.Item", EMPTY_QUERY, deps)
  if (read.kind !== "refused") throw new Error(read.kind)
  expect(read.diagnostics).toHaveLength(3)
  expect(diagnosticsAt(read.diagnostics, "/filter/all/1").map(({ code }) => code)).toEqual(["query.value_invalid"])
  expect(diagnosticsAt(read.diagnostics, "/sort").map(({ pointer }) => pointer)).toEqual(["/sort/0"])
})
