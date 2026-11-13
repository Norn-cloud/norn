import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import type { ObjectVersion } from "../src/evidence/review.ts"
import { lineageGraph, NODE_CAP } from "../src/lib/lineage-graph.ts"
import { byValidTime, countText, keyFacts, linkGroups, timePropertyOf, timeline, writerCounts, type CountedLinkPage } from "../src/lib/record-page.ts"
import type { SourceLineage } from "../src/lib/transport.ts"

const field = (id: string, kind = "text") => ({ id, type: kind, optional: false, decimal: undefined, scalar: { kind } }) as never

test("key facts follow the definition's order, skip the title facts and nested values, and stop at six", () => {
  const fields = ["id", "label", "code", "payload", "a", "b", "c", "d", "e", "f"].map((id) => field(id, id === "payload" ? "json-ext" : "text"))
  expect(keyFacts({ fields }).map(({ id }) => id)).toEqual(["code", "a", "b", "c", "d", "e"])
})

test("a measure's unit and currency sources are read inside it, and an empty fact takes no slot", () => {
  const quantity = { ...field("quantity", "decimal"), decimal: { unit: undefined, perUnit: undefined, currencyFrom: undefined, unitFrom: ["unit", "currency"] } }
  const cost = { ...field("stock_cost", "decimal"), decimal: { unit: undefined, perUnit: undefined, currencyFrom: "cost_currency" } }
  const fields = [field("resource_code"), quantity, field("unit"), field("currency"), field("location"), cost, field("cost_currency")] as never
  expect(keyFacts({ fields }).map(({ id }) => id)).toEqual(["resource_code", "quantity", "location", "stock_cost"])
  expect(keyFacts({ fields }, { resource_code: "GOLD", quantity: 200, location: null, stock_cost: 9000 }).map(({ id }) => id)).toEqual(["resource_code", "quantity", "stock_cost"])
})

const item = { name: "canon.Item", relationships: [] }
const incoming = [{ from: { name: "canon.Holding", title: "Holding" }, relationship: { id: "item" } }]
const edge = (source: string, target = "demo/item/gold") => ({ sourceType: "canon.Holding", link: "item", source, targetType: "canon.Item", target, version: 1 })

test("with the server's counts a group per counted link type, its ids from the first page; a stopped count reads as a lower bound", () => {
  const page: CountedLinkPage = {
    objectType: "canon.Item", id: "demo/item/gold", hasMore: true, items: [edge("h1"), edge("h2"), edge("other", "demo/item/silver")],
    counts: { groups: [{ sourceType: "canon.Holding", link: "item", targetType: "canon.Item", direction: "in", count: 500 }], complete: false, bound: 500 },
  }
  const [group, ...rest] = linkGroups(item, incoming, "demo/item/gold", page)
  expect(rest).toEqual([])
  expect(group).toMatchObject({ direction: "in", otherType: "canon.Holding", label: "Holding records (item)", count: 500, atLeast: true, ids: ["h1", "h2"] })
  expect(countText(group!)).toBe("500+")
  expect(countText({ count: 1204, atLeast: false })).toBe("1,204")
})

test("without counts every declared relationship is a group, uncounted, never a count of one page", () => {
  const page: CountedLinkPage = { objectType: "canon.Item", id: "demo/item/gold", hasMore: false, items: [edge("h1")] }
  const groups = linkGroups(item, incoming, "demo/item/gold", page)
  expect(groups.map(({ label, count, ids }) => ({ label, count, ids }))).toEqual([{ label: "Holding records (item)", count: null, ids: ["h1"] }])
  expect(countText(groups[0]!)).toBe("")
})

const version = (at: number, kind: "mapping" | "action" | "other", observed?: string): ObjectVersion => ({
  version: at, commit: 10 + at, logicalTime: Date.UTC(2026, 10, at), values: observed === undefined ? {} : { observed_at: observed },
  writer: kind === "action" ? { kind, actionId: "a", actor: "admin@demo", input: {} } : kind === "mapping" ? { kind, lineage: {} as SourceLineage } : { kind },
})

test("the timeline keeps the server's knowledge order; valid time orders by the time property and puts untimed versions last", () => {
  const versions = [version(3, "action", "2026-10-01T00:00:00Z"), version(2, "other"), version(1, "mapping", "2026-10-05T00:00:00Z")]
  const entries = timeline(versions, "observed_at")
  expect(entries.map(({ version }) => version)).toEqual([3, 2, 1])
  expect(entries[0]).toMatchObject({ commit: 13, kind: "action", known: "2026-11-03T00:00:00.000Z", valid: "2026-10-01T00:00:00Z" })
  expect(byValidTime(entries).map(({ version }) => version)).toEqual([1, 3, 2])
  expect(writerCounts(entries)).toEqual({ mapping: 1, action: 1, other: 1 })
  expect(timeline(versions, null).every(({ valid }) => valid === null)).toBe(true)
})

test("a type's time property is the one its metrics name; none or two is no time property", () => {
  const metric = (objectType: string, timeProperty: string | null) => ({ objectType, timeProperty })
  expect(timePropertyOf([metric("canon.Holding", "observed_at"), metric("canon.Holding", "observed_at"), metric("canon.Delivery", "occurred_at")], "canon.Holding")).toBe("observed_at")
  expect(timePropertyOf([metric("canon.Holding", "observed_at"), metric("canon.Holding", "due_at")], "canon.Holding")).toBeNull()
  expect(timePropertyOf([metric("canon.Holding", null)], "canon.Holding")).toBeNull()
})

const lineage = JSON.parse(readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/objects/canon.Item/demo~item~gold.source.json"), "utf8")) as SourceLineage
const root = { objectType: "canon.Item", key: "demo/item/gold", version: 1 }

test("one hop is the source record with every fact it feeds; two hops add the source document; nothing past that is claimed", () => {
  const one = lineageGraph(root, lineage, new Map(), 1)
  expect(one.nodes.map(({ kind, hop }) => [kind, hop])).toEqual([["record", 0], ["source", 1]])
  expect(one.edges).toHaveLength(1)
  expect(one.edges[0]?.facts).toEqual(Object.keys(lineage.fieldProvenance))
  expect(one.further).toBe(true)
  const two = lineageGraph(root, lineage, new Map(), 2)
  expect(two.nodes.map(({ kind }) => kind)).toEqual(["record", "source", "document"])
  expect(two.further).toBe(false)
  expect(lineageGraph(root, lineage, new Map(), 6)).toEqual(two)
})

test("an upstream fact is a hop of its own; its lineage is asked for, then followed, and a cycle stops", () => {
  const upstream = { objectType: "canon.Item", key: "demo/item/base", version: 2 }
  const withUpstream = (fields: SourceLineage["fieldProvenance"]) => ({ ...lineage, fieldProvenance: fields }) as SourceLineage
  const [first] = Object.values(lineage.fieldProvenance)
  const top = withUpstream({ code: { ...first!, upstream: [upstream] } })
  const asked = lineageGraph(root, top, new Map(), 3)
  expect(asked.unread).toEqual([upstream])
  const base = withUpstream({ code: { ...first!, upstream: [root] } })
  const followed = lineageGraph(root, top, new Map([[`fact:canon.Item/demo/item/base@2`, base]]), 6)
  expect(followed.unread).toEqual([])
  expect(followed.nodes.filter(({ kind }) => kind === "fact").map(({ hop }) => hop)).toEqual([2])
  expect(followed.nodes.length).toBeLessThanOrEqual(NODE_CAP)
})
