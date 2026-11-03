import { expect, test } from "bun:test"
import { readdirSync, readFileSync } from "node:fs"
import { join } from "node:path"
import type { Schemas } from "../../norn-sdk/src/index.ts"
import { renderCatalogDocument } from "../src/catalog.ts"

type Artifact = Schemas["NornArtifactEnvelope"]

const root = join(import.meta.dir, "../../..")
const directory = (path: string): Artifact[] => readdirSync(join(root, path)).filter((name) => name.endsWith(".json")).sort()
  .map((name) => JSON.parse(readFileSync(join(root, path, name), "utf8")) as Artifact)
const core = directory("registry/core/artifacts")
const measurements = directory("registry/packs/demo-erpnext/artifacts")
const laws = JSON.parse(readFileSync(join(root, "fixtures/law-ir/laws.json"), "utf8")) as Artifact[]
const compiled = [...core, ...measurements, ...laws]
const html = renderCatalogDocument({ artifacts: compiled })
const compiledLaws = compiled.filter((artifact) => artifact.kind === "law")

const declaredTypes = compiled.flatMap((artifact) => {
  if (artifact.kind !== "entity") return []
  const payload = artifact.payload
  if ("interface" in payload) return [{ name: payload.interface.name, parents: payload.interface.extends ?? [] }]
  if ("objectType" in payload) return [{ name: payload.objectType.name, parents: [...(payload.objectType.extends ? [payload.objectType.extends] : []), ...(payload.objectType.implements ?? [])] }]
  return []
})

test("every compiled concept has a card, and a graph node when it has a supertype edge", () => {
  expect(declaredTypes.length).toBeGreaterThan(15)
  const related = new Set(declaredTypes.flatMap(({ name, parents }) => parents.length ? [name, ...parents] : []))
  for (const { name } of declaredTypes) {
    expect(html).toContain(`-name">${name}</h3>`)
    expect(html.includes(`aria-label="${name}"`)).toBe(related.has(name))
  }
  expect(html).toContain("Without declared supertypes or subtypes:")
})

test("the graph draws exactly the declared supertype edges", () => {
  const edges = declaredTypes.reduce((sum, { parents }) => sum + parents.length, 0)
  expect(html.match(/<path class="edge /g)?.length).toBe(edges)
  expect(html).toContain("demo.erpnext.StockEntryMeasurement implements demo.erpnext.Measurement")
})

test("supertypes outside the document are marked, not expanded", () => {
  const alone = renderCatalogDocument({ artifacts: core.filter(({ artifactId }) => artifactId === "core.Conversion.interface") })
  expect(alone.match(/class="node external"/g)?.length).toBe(1)
  expect(alone).toContain("core.Process (not in this document)")
  expect(alone).not.toContain('aria-label="core.Process"')
})

test("decimal properties show their declared scale and unit", () => {
  expect(html).toContain('decimal, scale 6 <span class="unit">g</span>')
  expect(html).toContain("decimal, scale 6</td>")
})

test("laws show their subject, modality and declaration without inventing wording or origin", () => {
  expect(compiledLaws.length).toBeGreaterThan(laws.length)
  for (const law of compiledLaws) expect(html).toContain(law.name)
  expect(html).toContain("<code>self.status == &#39;shipped&#39; and self.paid == false</code>")
  expect(html).toContain('<span class="chip">never</span>')
  expect(html).toContain("app <code>erpnext</code>")
  expect(html.match(/The inherited origin chain is not exposed by the server\./g)?.length).toBe(compiledLaws.length)
  expect(html.match(/<div lang="ar" dir="rtl">/g)?.length).toBe(compiledLaws.length)
  expect(html).not.toMatch(/\bproved\b/)
})

test("the page is observed, script-free and isolated", () => {
  expect(html).toContain("This read carries no frontier or freshness label.")
  expect(html).not.toContain("<script")
  expect(html).toContain("default-src 'none'")
})

test("server text never becomes markup", () => {
  const hostile = '</code><script>alert("x")</script>'
  const [entity] = measurements.filter((artifact) => artifact.kind === "entity" && "objectType" in artifact.payload)
  if (!entity || entity.kind !== "entity" || !("objectType" in entity.payload)) throw new Error("Missing object type")
  const objectType = entity.payload.objectType
  const poisoned: Artifact = { ...entity, name: hostile, payload: { ...entity.payload, objectType: {
    ...objectType, name: hostile, description: hostile, implements: [hostile],
    properties: [{ id: hostile, type: { kind: "enum", values: [{ value: hostile }] } }],
  } } }
  const rendered = renderCatalogDocument({ title: hostile, artifacts: [poisoned] })
  expect(rendered).not.toContain("<script>")
  expect(rendered).toContain("&lt;script&gt;")
})

test("an empty read invents nothing", () => {
  const empty = renderCatalogDocument({ artifacts: [] })
  expect(empty).toContain("This document contains no definitions.")
  expect(empty).not.toContain("<svg")
  expect(empty).not.toContain('<nav')
})
