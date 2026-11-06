import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { cycleSort, EMPTY_QUERY, nextCursor, opsFor, recordsPath, recordsQueryParams, servedQuery, rowWithheld, snapshotMoved, totalOf, totalText } from "../src/lib/records-query.ts"

const recorded = (name: string) => JSON.parse(readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/objects", name), "utf8")) as unknown

test("an empty query asks for one default page and nothing else", () => {
  expect(recordsQueryParams(EMPTY_QUERY).toString()).toBe("limit=50")
})

test("sort, filter and cursor serialize to the proposed grammar", () => {
  const params = recordsQueryParams({
    sort: [{ property: "stock_cost", direction: "desc" }, { property: "code", direction: "asc" }],
    filter: { all: [{ property: "stock_cost", op: "between", value: ["10.000000", "20.500000"] }, { property: "status", op: "present" }] },
    limit: 100,
    after: "opaque~1",
  })
  expect(params.get("sort")).toBe("stock_cost:desc,code:asc")
  expect(JSON.parse(params.get("filter") ?? "")).toEqual({ all: [{ property: "stock_cost", op: "between", value: ["10.000000", "20.500000"] }, { property: "status", op: "present" }] })
  expect(params.get("after")).toBe("opaque~1")
  expect(params.get("limit")).toBe("100")
})

test("the type in the path is encoded; scope is left to the scoped host fetch", () => {
  const path = recordsPath("canon.Item", EMPTY_QUERY)
  expect(path).toBe("/v1/objects/canon.Item?limit=50")
  expect(path).not.toContain("orgId")
})

test("each declared kind offers only the contract's operators; structured kinds offer none", () => {
  expect(opsFor({ kind: "boolean" })).toEqual(["eq", "empty", "present"])
  expect(opsFor({ kind: "enum", values: ["a"] })).toEqual(["eq", "ne", "in", "empty", "present"])
  expect(opsFor({ kind: "decimal" })).toContain("between")
  expect(opsFor({ kind: "text" })).not.toContain("lt")
  expect(opsFor({ kind: "record" })).toEqual([])
  expect(opsFor({ kind: "json-ext" })).toEqual([])
  expect(opsFor({ kind: "unresolved", name: "x" })).toEqual([])
})

test("a recorded page has no query echo, so nothing reads as sorted or filtered on the server", () => {
  const page = recorded("canon.Item.page.json")
  expect(servedQuery(page)).toBeNull()
  expect(nextCursor(page)).toBe((page as { scannedThrough: string | null }).scannedThrough ?? undefined)
})

test("a page that echoes its query is the only source of what the server applied", () => {
  const page = { query: { sort: [{ property: "code", direction: "desc" }], filter: { all: [{ property: "code", op: "prefix", value: "G" }] } }, next: "c2", scannedThrough: "ignored" }
  expect(servedQuery(page)).toEqual({ sort: [{ property: "code", direction: "desc" }], filter: { all: [{ property: "code", op: "prefix", value: "G" }] } })
  expect(nextCursor(page)).toBe("c2")
  expect(servedQuery({ query: { sort: [{ property: "code", direction: "sideways" }] } })).toBeNull()
})

test("a row is withheld only when the server says so explicitly", () => {
  expect(rowWithheld({ withheld: true, values: null })).toBe(true)
  expect(rowWithheld({ withheld: false, values: {} })).toBe(false)
  expect(rowWithheld({ withheld: "true" })).toBe(false)
  expect(rowWithheld({})).toBe(false)
})

test("the total reads as the server sent it, approximate ones as about", () => {
  expect(totalOf({ total: { count: 1200, approximate: true } })).toEqual({ count: 1200, approximate: true })
  expect(totalText({ count: 1200, approximate: true })).toBe("About 1,200 records")
  expect(totalText({ count: 1, approximate: false })).toBe("1 record")
  expect(totalOf({ total: { count: 0, approximate: false } })).toEqual({ count: 0, approximate: false })
  expect(totalOf({ total: { count: -1, approximate: false } })).toBeNull()
  expect(totalOf({})).toBeNull()
})

test("only a 409 snapshot_moved restarts the query; a bad cursor is an error", () => {
  expect(snapshotMoved(409, "object.snapshot_moved")).toBe(true)
  expect(snapshotMoved(400, "query.cursor_invalid")).toBe(false)
  expect(snapshotMoved(409, "other")).toBe(false)
})

test("sort cycles asc, desc, off; adding keeps earlier keys and stops at three", () => {
  const one = cycleSort([], "a", false)
  expect(one).toEqual([{ property: "a", direction: "asc" }])
  expect(cycleSort(one, "a", false)).toEqual([{ property: "a", direction: "desc" }])
  expect(cycleSort([{ property: "a", direction: "desc" }], "a", false)).toEqual([])
  const many = ["a", "b", "c", "d"].reduce((sort, property) => cycleSort(sort, property, true), [] as ReturnType<typeof cycleSort>)
  expect(many.map(({ property }) => property)).toEqual(["a", "b", "c"])
  expect(cycleSort(many, "b", true).map(({ property, direction }) => `${property}:${direction}`)).toEqual(["a:asc", "b:desc", "c:asc"])
})
