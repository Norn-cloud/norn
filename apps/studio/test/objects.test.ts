import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import type { Field } from "../src/lib/catalog.ts"
import { readDocument } from "../src/lib/exact.ts"
import { collect, compareDecimals, factText, nextAfter, readProblem, readsOf, recordName } from "../src/lib/objects.ts"
import type { LinkPage, ObjectPage, ObjectRecord } from "../src/lib/transport.ts"

// The cell's reads of the three-systems world (crates/norn-api/tests/explorer_objects.rs).
const recorded = (name: string) => readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/objects", name), "utf8")
const text: Field = { id: "code", type: "Text", optional: false, decimal: undefined }

test("the one gold item reads as the cell stored it", () => {
  const page = readDocument<ObjectPage>(recorded("canon.Item.page.json"), "recorded")
  const [item] = page.document.records
  expect(page.document.records.length).toBe(1)
  expect(item && recordName(item)).toBe("Gold scrap")
  expect(item && factText(text, item, page.numbers, "/records/0")).toBe("GOLD")
})

test("decimals keep their exact text and declared currency; missing facts read as a dash", () => {
  const source = '{"objectType":"canon.Holding","id":"h","scope":"s","version":1,"values":{"stock_cost":8100.500000,"cost_currency":"USD","unit":null}}'
  const read = readDocument<ObjectRecord>(source, "recorded")
  const cost: Field = { id: "stock_cost", type: "", optional: true, decimal: { unit: undefined, perUnit: undefined, currencyFrom: "cost_currency" } }
  expect(factText(cost, read.document, read.numbers, "")).toBe("8100.500000 USD")
  expect(factText({ ...text, id: "unit" }, read.document, read.numbers, "")).toBe("—")
  expect(recordName(read.document)).toBe("h")
})

test("a declared per-unit reads after the unit", () => {
  const read = readDocument<ObjectRecord>('{"objectType":"t","id":"p","scope":"s","version":1,"values":{"price":12.50}}', "recorded")
  const price: Field = { id: "price", type: "", optional: false, decimal: { unit: "USD", perUnit: "kg", currencyFrom: undefined } }
  expect(factText(price, read.document, read.numbers, "")).toBe("12.50 USD per kg")
})

test("exact decimals sort by value, beyond float precision", () => {
  expect(compareDecimals("9007199254740993.000000", "9007199254740992.000000")).toBeGreaterThan(0)
  expect(compareDecimals("0.980", "0.98")).toBe(0)
  expect(compareDecimals("-2.5", "1")).toBeLessThan(0)
  expect(compareDecimals("-2.5", "-10")).toBeGreaterThan(0)
  expect(compareDecimals("10", "9.99")).toBeGreaterThan(0)
  expect(compareDecimals(undefined, "1")).toBeLessThan(0)
  expect(compareDecimals("-0.0", "0")).toBe(0)
})

test("server strings render verbatim: codes and identifiers are never reworded", () => {
  const read = readDocument<ObjectRecord>('{"objectType":"t","id":"x","scope":"s","version":1,"values":{"sku":"SKU_123"}}', "recorded")
  expect(factText({ ...text, id: "sku" }, read.document, read.numbers, "")).toBe("SKU_123")
})

const page = (records: number, scannedThrough: string | null, quarantineCount = 0) =>
  readDocument<ObjectPage>(JSON.stringify({ objectType: "t", records: Array.from({ length: records }, (_, i) => ({ objectType: "t", id: `r${i}`, scope: "s", version: 1, values: {} })), quarantineCount, scannedThrough }), "recorded")

test("a page with a cursor is partial until the server returns none", () => {
  expect(nextAfter(page(1, "r0"))).toBe("r0")
  expect(collect([page(1, "r0")]).complete).toBe(false)
  // An empty page that still carries a cursor (every candidate held back) is not "no records".
  const held = collect([page(0, "q9", 3)])
  expect([held.rows.length, held.held, held.complete]).toEqual([0, 3, false])
  expect(collect([page(1, "r0"), null]).complete).toBe(false)
  const done = collect([page(2, "r1"), page(0, null)])
  expect([done.rows.length, done.complete]).toEqual([2, true])
  expect(nextAfter(page(0, null))).toBeUndefined()
})

test("the recorded Item scan is complete: its continuation page returns no cursor", () => {
  const first = readDocument<ObjectPage>(recorded("canon.Item.page.json"), "recorded")
  const after = first.document.scannedThrough ?? ""
  const next = readDocument<ObjectPage>(recorded(`canon.Item.after.${after.replaceAll("/", "~")}.page.json`), "recorded")
  const all = collect([first, next])
  expect([all.rows.length, all.complete]).toEqual([1, true])
})

test("a failed read surfaces its message instead of loading forever", () => {
  expect(readProblem({ error: null }, { error: new Error("catalog read refused: scope.denied") })).toBe("catalog read refused: scope.denied")
  expect(readProblem({ error: null }, { error: null })).toBeUndefined()
})

test("a recorded object is returned only for the identity it was asked for", () => {
  const object = readDocument<ObjectRecord>(recorded("canon.Item/demo~item~gold.object.json"), "recorded").document
  const links = readDocument<LinkPage>(recorded("canon.Item/demo~item~gold.links.json"), "recorded").document
  expect(readsOf("canon.Item", "demo/item/gold", object, links)).toBe(true)
  // `/` and `~` fold to the same file name; the identity check tells them apart.
  expect(readsOf("canon.Item", "demo~item~gold", object, links)).toBe(false)
  expect(readsOf("canon.Party", "demo/item/gold", object, links)).toBe(false)
})
