import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import type { Field } from "../src/lib/catalog.ts"
import { readDocument } from "../src/lib/exact.ts"
import { compareDecimals, factText, recordName } from "../src/lib/objects.ts"
import type { ObjectPage, ObjectRecord } from "../src/lib/transport.ts"

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
