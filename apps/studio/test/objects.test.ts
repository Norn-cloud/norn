import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { catalogOf } from "../src/lib/catalog.ts"
import { readDocument } from "../src/lib/exact.ts"
import { exactJson, lifecycleOf, lifecycleStates, recordTitle } from "../src/lib/objects.ts"
import type { Artifact, ObjectRecord } from "../src/lib/transport.ts"

const fixture = (name: string) => readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer", name), "utf8")
const catalog = catalogOf(readDocument<readonly Artifact[]>(fixture("three-systems.definitions.json"), "recorded").document)
// The one recorded object (crates/norn-api/tests/explorer_objects.rs).
const gold = readDocument<ObjectRecord>(fixture("objects/canon.Item/demo~item~gold.object.json"), "recorded").document

test("a record is named by its own label", () => {
  expect(recordTitle(gold)).toBe("Gold scrap")
  expect(recordTitle({ ...gold, values: {} })).toBe("demo/item/gold")
})

test("a lifecycle's states follow its declared transitions from the initial state", () => {
  const [lifecycle] = catalog.byName.get("core.analytics.Restatement")?.lifecycles ?? []
  if (lifecycle === undefined) throw new Error("Restatement declares a lifecycle")
  expect(lifecycleStates(lifecycle)).toEqual(["building", "ready", "approved", "active"])
  const ready = lifecycleOf(lifecycle, { ...gold, values: { [lifecycle.property]: "ready" } })
  expect(ready.state).toBe("ready")
  expect(ready.next.map(({ id }) => id)).toEqual(["approve"])
  expect(lifecycleOf(lifecycle, gold).state).toBeUndefined()
  expect(lifecycleOf(lifecycle, gold).next).toEqual([])
})

test("object types without a lifecycle declare none", () => {
  expect(catalog.byName.get("canon.Item")?.lifecycles).toEqual([])
})

test("composite facts keep every nested number's exact text", () => {
  const read = readDocument<ObjectRecord>('{"objectType":"t","id":"x","scope":"s","version":1,"values":{"lots":[{"qty":100.000,"unit":"g"}],"range":{"low":1.50,"high":2}}}', "recorded")
  expect(exactJson(read.document.values.lots, read.numbers, "/values/lots")).toBe('[{"qty":100.000,"unit":"g"}]')
  expect(exactJson(read.document.values.range, read.numbers, "/values/range")).toBe('{"low":1.50,"high":2}')
})
