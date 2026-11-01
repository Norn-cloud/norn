import { expect, test } from "bun:test"
import { renderDefinitionsDocument, type DefinitionEntry } from "../src/definitions.ts"

const entry: DefinitionEntry = {
  metadata: { artifactId: "alpha.SalesOrder", kind: "entity", name: "Sales order", version: "0.1.0" },
  source: '{"amount":9007199254740993,"rate":0.10000000000000000001}',
}

test("inspector preserves exact server source and definition order", () => {
  const other: DefinitionEntry = { ...entry, metadata: { ...entry.metadata, artifactId: "stock.Position", name: "Stock position" } }
  const html = renderDefinitionsDocument({ entries: [entry, other] })
  expect(html).toContain("9007199254740993")
  expect(html).toContain("0.10000000000000000001")
  expect(html.indexOf("alpha.SalesOrder")).toBeLessThan(html.indexOf("stock.Position"))
  expect(html).toContain("2 definitions")
  expect(html).not.toContain("<script")
})

test("all server text is displayed as text, including closing elements", () => {
  const hostile = '</code></pre><script>alert("x")</script>'
  const html = renderDefinitionsDocument({ title: hostile, entries: [{
    metadata: { artifactId: hostile, kind: "entity", name: hostile, version: hostile }, source: hostile,
  }] })
  expect(html).not.toContain(hostile)
  expect(html.match(/&lt;script&gt;/g)?.length).toBe(6)
  expect(html).toContain("default-src 'none'")
})

test("empty reads do not invent definitions or guarantees", () => {
  const html = renderDefinitionsDocument({ entries: [] })
  expect(html).toContain("0 definitions")
  expect(html).toContain("This document contains no definitions.")
  expect(html).not.toContain("<article")
  expect(html).not.toContain("proved")
})
