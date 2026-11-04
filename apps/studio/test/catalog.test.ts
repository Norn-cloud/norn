import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { catalogOf, conceptTitle, systemName } from "../src/lib/catalog.ts"
import { readDocument } from "../src/lib/exact.ts"
import type { Artifact } from "../src/lib/transport.ts"

// The cell's installed three-systems definitions (crates/norn-cell/tests/explorer_documents.rs).
const source = readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/three-systems.definitions.json"), "utf8")
const catalog = catalogOf(readDocument<readonly Artifact[]>(source, "recorded").document)

test("three systems feed one business model", () => {
  expect(catalog.systems.map(({ name }) => name).sort()).toEqual(["ERPNext", "Odoo", "Shopify"])
  for (const system of catalog.systems) expect(system.feeds).toContain("canon.Holding")
  expect(catalog.concepts.filter(({ kind }) => kind === "concept").length).toBe(8)
  const holding = catalog.byName.get("canon.Holding")
  expect(holding?.kind).toBe("concept")
  expect([...new Set(holding?.feeds.map(({ system }) => system))].sort()).toEqual(["ERPNext", "Odoo", "Shopify"])
})

test("a concept states its facts, relationships and parents as declared", () => {
  const holding = catalog.byName.get("canon.Holding")
  expect(holding?.parents).toEqual(["core.Resource"])
  expect(holding?.relationships.map(({ id, target, count }) => [id, target, count])).toEqual([
    ["item", "canon.Item", "one"], ["owner", "canon.Party", "at most one"], ["custodian", "canon.Party", "at most one"],
  ])
  const field = (id: string) => holding?.fields.find((candidate) => candidate.id === id)
  expect(field("quantity")?.type).toBe("Exact number, 6 decimal places")
  expect(field("stock_cost")).toEqual({ id: "stock_cost", type: "Exact number, 6 decimal places, currency from cost currency", optional: true })
  expect(field("quantity_basis")?.type).toBe("One of: on hand, available")
})

test("each system's declared source for a fact, including a declared gap", () => {
  const holding = catalog.byName.get("canon.Holding")
  const feed = (system: string) => holding?.feeds.find((candidate) => candidate.system === system)
  expect(feed("ERPNext")?.dataset).toBe("Warehouse Lot Balance")
  expect(feed("ERPNext")?.facts.get("quantity")).toEqual([{ kind: "field", field: "quantity" }])
  expect(feed("ERPNext")?.facts.get("quantity_basis")).toEqual([{ kind: "fixed", value: "on_hand" }])
  expect(feed("ERPNext")?.facts.get("currency")).toEqual([{ kind: "empty" }])
  expect(feed("Shopify")?.facts.get("stock_cost")?.[0]).toMatchObject({ kind: "gap", reason: "not_in_source" })
  expect(feed("ERPNext")?.facts.get("owner")).toEqual([{ kind: "field", field: "owner_ref" }])
  const delivery = catalog.byName.get("canon.Delivery")?.feeds.find((candidate) => candidate.system === "Shopify")
  expect(delivery?.facts.get("reverses")?.[0]?.kind).toBe("gap")
})

test("repeated links, nested rows, lifecycle states and gap reasons keep what is declared", () => {
  const artifacts = [
    { kind: "entity", artifactId: "demo.Thing.entity", name: "Thing", payload: { entityVersion: "2.0", objectType: { name: "demo.Thing", properties: [] } } },
    { kind: "source_schema", artifactId: "demo.source", payload: { source: { system: "synthetic-odoo" } } },
    { kind: "mapping", artifactId: "demo.mapping", payload: { source: { schemaId: "demo.source" }, maps: [{ source: { dataset: "Thing" }, target: { entity: "demo.Thing.entity" }, links: [{ link: "party", key: "input.a" }, { link: "party", key: "input.b" }],
      properties: [{ property: "amount", value: "input.amount", nested: { dataset: "Lines", field: "lines", where: "row.kind == 'gold'" } }],
      lifecycles: [{ property: "status", state: "input.state" }, { property: "phase", transition: "input.event" }], gaps: [{ member: "cost", reason: "deferred" }] }] } },
  ] as unknown as readonly Artifact[]
  const facts = catalogOf(artifacts).byName.get("demo.Thing")?.feeds[0]?.facts
  expect(facts?.get("party")).toEqual([{ kind: "field", field: "a" }, { kind: "field", field: "b" }])
  expect(facts?.get("amount")).toEqual([{ kind: "nested", dataset: "Lines", collection: "lines", where: "row.kind == 'gold'", value: { kind: "field", field: "amount" } }])
  expect(facts?.get("phase")).toEqual([{ kind: "transition", expression: "input.event" }])
  expect(facts?.get("status")).toEqual([{ kind: "field", field: "state" }])
  expect(facts?.get("cost")).toEqual([{ kind: "gap", reason: "deferred", note: undefined }])
})

test("laws attach to the concepts they govern", () => {
  expect(catalog.byName.get("core.analytics.Restatement")?.rules.length).toBeGreaterThan(0)
  expect(catalog.byName.get("core.analytics.RestatementEvidence")?.rules.length).toBeGreaterThan(0)
})

test("names read as plain words", () => {
  expect(conceptTitle("canon.SettlementObservation")).toBe("Settlement observation")
  expect(systemName("synthetic-erpnext")).toBe("ERPNext")
  expect(systemName("acme")).toBe("acme")
})

test("a mapping target resolves through its entity artifact's declared name, whatever the artifact ID", () => {
  const artifacts = [
    { kind: "entity", artifactId: "demo.things.v2", name: "Things", payload: { entityVersion: "2.0", objectType: { name: "demo.Thing", properties: [] } } },
    { kind: "source_schema", artifactId: "demo.source", payload: { source: { system: "synthetic-erpnext" } } },
    { kind: "mapping", artifactId: "demo.mapping", payload: { source: { schemaId: "demo.source" }, maps: [{ source: { dataset: "Thing" }, target: { entity: "demo.things.v2" }, properties: [] }] } },
  ] as unknown as readonly Artifact[]
  expect(catalogOf(artifacts).byName.get("demo.Thing")?.feeds.map(({ system }) => system)).toEqual(["ERPNext"])
})
