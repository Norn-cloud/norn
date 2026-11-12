import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { catalogOf, type Field } from "../src/lib/catalog.ts"
import { numberTexts, readDocument } from "../src/lib/exact.ts"
import { exactJson, exactPretty, factText, lifecycleOf, lifecycleStates, readProblem, readsOf, recordName } from "../src/lib/objects.ts"
import type { Artifact, LinkPage, ObjectPage, ObjectRecord } from "../src/lib/transport.ts"

// The cell's reads of the three-systems world (crates/norn-api/tests/explorer_objects.rs).
const recorded = (name: string) => readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/objects", name), "utf8")
const text: Field = { id: "code", label: "code", labelLang: "en", valueLabels: new Map(), type: "Text", optional: false, decimal: undefined, scalar: { kind: "text" } }

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
  const cost: Field = { id: "stock_cost", label: "stock cost", labelLang: "en", valueLabels: new Map(), type: "", optional: true, decimal: { unit: undefined, perUnit: undefined, currencyFrom: "cost_currency" }, scalar: { kind: "decimal" } }
  expect(factText(cost, read.document, read.numbers, "")).toBe("8100.500000 USD")
  expect(factText({ ...text, id: "unit" }, read.document, read.numbers, "")).toBe("—")
  expect(recordName(read.document)).toBe("h")
})

test("a declared per-unit reads after the unit", () => {
  const read = readDocument<ObjectRecord>('{"objectType":"t","id":"p","scope":"s","version":1,"values":{"price":12.50}}', "recorded")
  const price: Field = { id: "price", label: "price", labelLang: "en", valueLabels: new Map(), type: "", optional: false, decimal: { unit: "USD", perUnit: "kg", currencyFrom: undefined }, scalar: { kind: "decimal" } }
  expect(factText(price, read.document, read.numbers, "")).toBe("12.50 USD per kg")
})

test("server strings render verbatim: codes and identifiers are never reworded", () => {
  const read = readDocument<ObjectRecord>('{"objectType":"t","id":"x","scope":"s","version":1,"values":{"sku":"SKU_123"}}', "recorded")
  expect(factText({ ...text, id: "sku" }, read.document, read.numbers, "")).toBe("SKU_123")
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

// Lifecycles come from the admitted definitions; the record is a minimal synthetic one.
const lifecycleCatalog = catalogOf(readDocument<readonly Artifact[]>(readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/three-systems.definitions.json"), "utf8"), "recorded").document)
const record = { objectType: "t", id: "x", scope: "s", version: 1, values: {} } as ObjectRecord

test("a lifecycle's states follow its declared transitions from the initial state", () => {
  const [lifecycle] = lifecycleCatalog.byName.get("core.analytics.Restatement")?.lifecycles ?? []
  if (lifecycle === undefined) throw new Error("Restatement declares a lifecycle")
  expect(lifecycleStates(lifecycle)).toEqual(["building", "ready", "approved", "active"])
  const ready = lifecycleOf(lifecycle, { ...record, values: { [lifecycle.property]: "ready" } })
  expect(ready.state).toBe("ready")
  expect(ready.next.map(({ id }) => id)).toEqual(["approve"])
  expect(lifecycleOf(lifecycle, record).state).toBeUndefined()
  expect(lifecycleOf(lifecycle, record).next).toEqual([])
})

test("object types without a lifecycle declare none", () => {
  expect(lifecycleCatalog.byName.get("canon.Item")?.lifecycles).toEqual([])
})

test("composite facts keep every nested number's exact text", () => {
  const read = readDocument<ObjectRecord>('{"objectType":"t","id":"x","scope":"s","version":1,"values":{"lots":[{"qty":100.000,"unit":"g"}],"range":{"low":1.50,"high":2}}}', "recorded")
  expect(exactJson(read.document.values.lots, read.numbers, "/values/lots")).toBe('[{"qty":100.000,"unit":"g"}]')
  expect(factText({ id: "lots", label: "lots", labelLang: "en", valueLabels: new Map(), type: "List", optional: false, decimal: undefined, scalar: { kind: "record" } }, read.document, read.numbers, "")).toBe('[{"qty":100.000,"unit":"g"}]')
  expect(exactJson(read.document.values.range, read.numbers, "/values/range")).toBe('{"low":1.50,"high":2}')
})

test("an indented document keeps every number exactly as the server wrote it", () => {
  const source = '{"a":{"b":[1.10,9007199254740993]},"c":"x","d":[],"e":{}}'
  expect(exactPretty(JSON.parse(source), numberTexts(source), "")).toBe('{\n  "a": {\n    "b": [\n      1.10,\n      9007199254740993\n    ]\n  },\n  "c": "x",\n  "d": [],\n  "e": {}\n}')
})

test("a decimal shows at its declared scale: zeros are added, no digit is ever rounded or dropped", async () => {
  const { atScale } = await import("../src/lib/objects.ts")
  expect(atScale("72", 2)).toBe("72.00")
  expect(atScale("-0.5", 2)).toBe("-0.50")
  expect(atScale("19.995", 2)).toBe("19.995")
  expect(atScale("1e3", 2)).toBe("1e3")
  expect(atScale("7", undefined)).toBe("7")
  // Property: for any plain decimal and scale, the result has at least `scale` places and reads as the same number.
  for (let i = 0; i < 500; i++) {
    const whole = String(Math.floor(Math.random() * 1e6) * (Math.random() < 0.3 ? -1 : 1))
    const places = Math.floor(Math.random() * 5)
    const text = places === 0 ? whole : `${whole}.${String(Math.floor(Math.random() * 10 ** places)).padStart(places, "0")}`
    const scale = Math.floor(Math.random() * 7)
    const shown = atScale(text, scale)
    expect(shown.startsWith(text)).toBe(true)
    expect(/^0*$/.test(shown.slice(text.length).replace(".", ""))).toBe(true)
    if (scale > 0) expect((shown.split(".")[1] ?? "").length).toBeGreaterThanOrEqual(scale)
  }
})
