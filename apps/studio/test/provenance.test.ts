import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import type { ObjectVersion } from "../src/evidence/review.ts"
import { buckets } from "../src/lib/answer.ts"
import { readDocument } from "../src/lib/exact.ts"
import { bucketProvenance, factProvenance, metricLabels, versionShown } from "../src/lib/provenance.ts"
import type { Answer, Metric, ObjectType, Question, SourceLineage } from "../src/lib/transport.ts"

const demo = (path: string) => readFileSync(join(import.meta.dir, "../../../fixtures/demo", path), "utf8")
const lineage = JSON.parse(demo("explorer/objects/canon.Item/demo~item~gold.source.json")) as SourceLineage
const version = (writer: ObjectVersion["writer"], at = 3): ObjectVersion => ({ version: at, commit: 40 + at, logicalTime: Date.UTC(2026, 10, 12, 10), values: {}, writer })

test("a mapped fact names its source record and fields, the mapping rule and pin, the receipt and both times", () => {
  const card = factProvenance("code", version({ kind: "mapping", lineage }), null)
  expect(card.sources).toEqual([{ system: "synthetic-erpnext", record: "Item/erpnext/demo/Item/SYN-ERPNEXT-item", fields: ["name", "custom_norn_ref", "code"] }])
  expect(card.transform).toEqual({ kind: "mapping", artifactId: "erpnext.three.mapping", version: "erpnext.three.mapping@0.1.0", sha256: lineage.mappingPin.sha256, rule: "item.code", upstream: 0 })
  expect(card.receipt).toEqual({ kind: "commit", commit: 43 })
  expect(card.digests).toEqual([{ label: "Bundle", value: lineage.bundleDigest ?? null }, { label: "Document", value: lineage.documentDigest ?? null }])
  expect(card.asOf).toEqual([{ label: "Recorded", value: "2026-11-02T12:00:00Z" }, { label: "Written", value: "2026-11-12T10:00:00.000Z" }])
})

test("a fact an action wrote names the action and its verified actor, never a source it did not read", () => {
  const card = factProvenance("code", version({ kind: "action", actionId: "canon.item.correct", actor: "admin@demo", input: {} }), lineage)
  expect(card.sources).toEqual([])
  expect(card.transform).toEqual({ kind: "action", actionId: "canon.item.correct", actor: "admin@demo" })
  expect(card.receipt).toEqual({ kind: "commit", commit: 43 })
})

test("without a history entry the record's own lineage is the card; a withheld digest stays null", () => {
  const card = factProvenance("code", null, { ...lineage, bundleDigest: null })
  expect(card.receipt).toBeNull()
  expect(card.sources[0]?.record).toBe("Item/erpnext/demo/Item/SYN-ERPNEXT-item")
  expect(card.digests[0]).toEqual({ label: "Bundle", value: null })
  expect(card.asOf).toEqual([{ label: "Recorded", value: "2026-11-02T12:00:00Z" }])
})

test("a fact the mapping names no field for keeps the record and the mapping, with no fields or times invented", () => {
  const card = factProvenance("not_mapped", version({ kind: "mapping", lineage }), null)
  expect(card.sources).toEqual([{ system: "synthetic-erpnext", record: "Item/erpnext/demo/Item/SYN-ERPNEXT-item", fields: [] }])
  expect(card.transform).toMatchObject({ kind: "mapping", rule: null, version: "0.1.0" })
  expect(card.asOf.map(({ label }) => label)).toEqual(["Written"])
})

test("the version shown is found in the history; a newer one is named, a missing one is null", () => {
  const versions = [version({ kind: "other" }, 5), version({ kind: "other" }, 4), version({ kind: "other" }, 3)]
  expect(versionShown(versions, 5)).toEqual({ entry: versions[0] ?? null, newer: null })
  expect(versionShown(versions, 3)).toEqual({ entry: versions[2] ?? null, newer: 5 })
  expect(versionShown(versions, 1)).toEqual({ entry: null, newer: 5 })
  expect(factProvenance("x", versions[0] ?? null, null).transform).toEqual({ kind: "other" })
})

test("an answer's number names the systems it read, its metrics, the ledger head and the knowledge it was answered at", () => {
  const answer = readDocument<Answer>(demo("answers/scrap-yield.k1.answer.json"), "recorded")
  const declared = {
    question: JSON.parse(demo("answers/scrap-yield.question.json")) as Question,
    metrics: JSON.parse(demo("answers/scrap-yield.metrics.json")) as Metric[],
    objectType: JSON.parse(demo("answers/scrap-yield.object-type.json")) as ObjectType,
  }
  const [first] = buckets(declared, { answer: answer.document, numbers: answer.numbers, at: "" })
  if (first === undefined) throw new Error("no bucket")
  const card = bucketProvenance(answer.document, first, metricLabels(declared), "Computed by the cell's exact engine")
  expect(card.sources.map(({ system }) => system)).toEqual([...new Set([...Object.keys(answer.document.watermarks), ...Object.keys(answer.document.sourceCursors)])].sort())
  expect(metricLabels(declared)).toEqual(["Yield"])
  expect(metricLabels({ ...declared, metrics: [] })).toEqual(["demo.yield"])
  expect(card.transform).toEqual({ kind: "question", metrics: ["Yield"], records: first.members.length, engine: "Computed by the cell's exact engine" })
  expect(card.receipt).toEqual({ kind: "head", seq: answer.document.revision.seq, hash: answer.document.revision.hash })
  expect(card.asOf[0]).toEqual({ label: "Known", value: `after ${answer.document.knownAsOf} commit${answer.document.knownAsOf === 1 ? "" : "s"}` })
  expect(card.asOf[1]).toEqual({ label: "Window to", value: answer.document.effectiveAsOf })
})
