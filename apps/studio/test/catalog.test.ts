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
  expect(field("stock_cost")).toEqual({ id: "stock_cost", type: "Exact number, 6 decimal places, currency from cost currency", optional: true, decimal: { unit: undefined, currencyFrom: "cost_currency" } })
  expect(field("quantity_basis")?.type).toBe("One of: on hand, available")
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
