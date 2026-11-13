import { expect, test } from "bun:test"
import { chosen, isFacets, toggled, without } from "../src/lib/facets.ts"
import { newViewId, sortKeys, sortText, viewPrefix } from "../src/lib/views.ts"

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
