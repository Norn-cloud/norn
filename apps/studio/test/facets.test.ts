import { expect, test } from "bun:test"
import { QueryClient } from "@tanstack/react-query"
import { StudioScopeError } from "../src/lib/definitions.ts"
import { chosen, facetsQuery, isFacets, toggled, without } from "../src/lib/facets.ts"
import { newViewId, sortKeys, sortText, viewPrefix, WORKSPACE_CHANGED_MESSAGE } from "../src/lib/views.ts"

const filter = { all: [{ property: "location", op: "contains" as const, value: "north" }] }

test("choosing facet values narrows by one value, then several, then none, leaving other clauses alone", () => {
  const one = toggled(filter, "status", "open")
  expect(one).toEqual({ all: [...filter.all, { property: "status", op: "eq", value: "open" }] })
  const two = toggled(one, "status", "closed")
  expect(two).toEqual({ all: [...filter.all, { property: "status", op: "in", value: ["open", "closed"] }] })
  expect(chosen(two, "status")).toEqual(["open", "closed"])
  expect(toggled(two, "status", "open")).toEqual({ all: [...filter.all, { property: "status", op: "eq", value: "closed" }] })
  expect(toggled(toggled(two, "status", "open"), "status", "closed")).toEqual(filter)
  expect(toggled(null, "status", "open")).toEqual({ all: [{ property: "status", op: "eq", value: "open" }] })
  expect(toggled({ all: [{ property: "status", op: "eq", value: "open" }] }, "status", "open")).toBeNull()
})

test("a yes/no facet holds one value at a time; a property's own counts leave out its own clause", () => {
  const yes = toggled(null, "active", true)
  expect(toggled(yes, "active", false)).toEqual({ all: [{ property: "active", op: "eq", value: false }] })
  const both = { all: [...filter.all, { property: "status", op: "in" as const, value: ["open"] }] }
  expect(without(both, "status")).toEqual(filter)
  expect(without(both, "location")).toEqual(both)
  expect(without({ all: [{ property: "status", op: "eq", value: "open" }] }, "status")).toBeNull()
})

test("a view's id starts with its type's prefix, its sort round-trips through the grid's text", () => {
  expect(viewPrefix("canon.OrderLine")).toBe("studio.view.canon_orderline.")
  const id = newViewId("canon.OrderLine", "Open lines, north!", () => "abc12345")
  expect(id).toBe("studio.view.canon_orderline.open_lines_north_abc12345")
  expect(id.startsWith(viewPrefix("canon.OrderLine"))).toBe(true)
  expect(newViewId("canon.OrderLine", "!!!", () => "x")).toBe("studio.view.canon_orderline.view_x")
  const keys = [{ property: "due_at", direction: "desc" as const }, { property: "status", direction: "asc" as const }]
  expect(sortText(keys)).toBe("due_at:desc,status:asc")
  expect(sortKeys(sortText(keys))).toEqual(keys)
  expect(sortText([])).toBeUndefined()
  expect(sortKeys("bad,x:sideways,y:asc")).toEqual([{ property: "y", direction: "asc" }])
})

test("only a facets document is counted from; anything else is not one", () => {
  expect(isFacets({ rows: 3, withheldRows: 0, lowerBound: false, facets: [{ property: "status", values: [{ value: "open", count: 3 }], otherRows: 0, nulls: 0 }] })).toBe(true)
  expect(isFacets({ objectType: "canon.Holding", records: [], total: { count: 0, approximate: false } })).toBe(false)
  expect(isFacets({ rows: 3, approximate: true, facets: [] })).toBe(false)
  expect(isFacets(null)).toBe(false)
})

const scopeA = { orgId: "org-a", environmentId: "prod" }
const scopeB = { orgId: "org-a", environmentId: "test" }
const sharedType = "canon.Item"
const account = ({ orgId, environmentId }: typeof scopeA) => Response.json({ organization: { id: orgId }, environmentId })
const counts = (value: string) => Response.json({
  rows: 1, withheldRows: 0, lowerBound: false,
  facets: [{ property: "status", values: [{ value, count: 1 }], otherRows: 0, nulls: 0 }],
})

test("saved-view facets refuse dispatch when the session has already switched", async () => {
  const client = new QueryClient()
  const hostCalls: string[] = []
  const options = facetsQuery({ objectType: sharedType, filter, scope: scopeA }, async (url) => {
    if (url === "/api/me") return account(scopeB)
    hostCalls.push(url)
    return counts("B")
  })
  await expect(client.fetchQuery(options)).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  expect(hostCalls).toEqual([])
  expect(client.getQueryData(options.queryKey)).toBeUndefined()
  expect(client.getQueryState(options.queryKey)?.error).toBeInstanceOf(StudioScopeError)
})

test("saved-view facets discard a late property response after a workspace switch", async () => {
  const client = new QueryClient()
  let markRequested: (url: URL) => void = () => {}
  const requested = new Promise<URL>((resolve) => { markRequested = resolve })
  let respond: (value: Response) => void = () => {}
  const response = new Promise<Response>((resolve) => { respond = resolve })
  let active = scopeA
  const options = facetsQuery({ objectType: sharedType, filter, scope: scopeA, property: "status" }, async (url) => {
    if (url === "/api/me") return account(active)
    markRequested(new URL(url, "https://studio.test"))
    return response
  })
  const pending = client.fetchQuery(options)
  const url = await Promise.race([requested, pending.then(() => { throw new Error("Facet read finished before dispatch") })])
  active = scopeB
  respond(counts("A"))
  await expect(pending).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  expect(url.pathname).toBe("/api/host/v1/objects/canon.Item:facets")
  expect(url.searchParams.get("orgId")).toBe(scopeA.orgId)
  expect(url.searchParams.get("environmentId")).toBe(scopeA.environmentId)
  expect(url.searchParams.get("properties")).toBe("status")
  expect(client.getQueryData(options.queryKey)).toBeUndefined()
})

test("facet caches separate matching types and filters by scope for both query kinds", async () => {
  const client = new QueryClient()
  let active = scopeA
  const fetcher = async (url: string) => {
    if (url === "/api/me") return account(active)
    const environmentId = new URL(url, "https://studio.test").searchParams.get("environmentId")
    return counts(environmentId === scopeA.environmentId ? "A" : "B")
  }
  for (const property of [undefined, "status"]) {
    const options = { objectType: sharedType, filter, property }
    const a = facetsQuery({ ...options, scope: scopeA }, fetcher)
    const b = facetsQuery({ ...options, scope: scopeB }, fetcher)
    const ordinary = facetsQuery(options, fetcher)
    active = scopeB
    const bResult = await client.fetchQuery(b)
    expect(client.getQueryData(a.queryKey)).toBeUndefined()
    expect(client.getQueryData(ordinary.queryKey)).toBeUndefined()
    const ordinaryResult = await client.fetchQuery(ordinary)
    expect(ordinaryResult.document).toEqual(bResult.document)
    active = scopeA
    const aResult = await client.fetchQuery(a)
    expect(aResult.document.facets[0]?.values[0]?.value).toBe("A")
    expect(bResult.document.facets[0]?.values[0]?.value).toBe("B")
    expect(client.getQueryData(a.queryKey)?.document.facets[0]?.values[0]?.value).toBe("A")
    expect(client.getQueryData(b.queryKey)?.document.facets[0]?.values[0]?.value).toBe("B")
    expect(client.getQueryData(ordinary.queryKey)?.document.facets[0]?.values[0]?.value).toBe("B")
  }
})
