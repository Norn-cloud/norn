import { expect, test } from "bun:test"
import { parseFragment } from "parse5"
import { renderDefinitionsDocument, readDefinitions, type DefinitionEntry } from "../src/definitions.ts"

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

for (const ending of ["\r\n", "\r", "\n"]) {
  test(`parsed source preserves ${JSON.stringify(ending)} line endings`, () => {
    const source = `{${ending}  "name": "<alpha>&",${ending}  "amount": 9007199254740993${ending}}`
    const html = renderDefinitionsDocument({ entries: [{ ...entry, source }] })
    const pre = html.match(/<pre[^>]*>(.*?)<\/pre>/s)?.[1]
    if (!pre) throw new Error("Missing source block")
    const code = parseFragment(pre).childNodes[0]
    if (!code || !("tagName" in code) || code.tagName !== "code") throw new Error("Missing code element")
    const text = code.childNodes[0]
    if (!text || !("value" in text)) throw new Error("Missing source text")
    expect(text.value).toBe(source)
  })
}

const entity: DefinitionEntry = {
  metadata: { ...entry.metadata, artifactId: "demo.Parcel", name: "Parcel" }, source: '{"exact":9007199254740993}',
  definition: { kind: "entity", dependencies: [], payload: { entityVersion: "2.0", objectType: {
    name: "demo.Parcel", properties: [
      { id: "mass", type: { kind: "decimal", scale: 3, unit: { system: "ucum", code: "kg" } } },
      { id: "phase", type: { kind: "enum", values: [{ value: "draft" }, { value: "sent" }, { value: "lost" }] } },
    ], links: [{ id: "destination", target: "demo.Place", cardinality: { from: "many", to: "one", required: true } }],
    lifecycles: [{ property: "phase", initial: ["draft"], terminal: ["sent", "lost"], immutable: ["sent"],
      transitions: [{ id: "send", from: ["draft"], to: "sent", guards: ["self.mass > 0"] }] }],
    invariants: [{ id: "positive", message: "Mass stays positive", expression: "self.mass > 0" }],
  } } },
}
const action: DefinitionEntry = {
  metadata: { artifactId: "demo.Send", kind: "action", name: "Send parcel", version: "0.1.0" }, source: '{}',
  definition: { kind: "action", dependencies: [{ artifactId: "demo.Positive", kind: "law", required: true }], payload: {
    actionId: "demo.Send", idempotent: true, ledgered: true, scopeRequired: [], outputSchema: {},
    inputSchema: { type: "object", properties: { parcelId: { type: "string" } } }, preconditions: ["input.parcelId != null"],
    effectProgram: { kind: "fire_transition", entityArtifactId: "demo.Parcel", entityArtifactVersion: "0.1.0", objectKind: "demo.Parcel", objectIdField: "parcelId", lifecycle: "phase", transition: "send" },
  } },
}

test("D5–D7: declared properties, units, relationships, states and action bindings correspond", () => {
  const html = renderDefinitionsDocument({ entries: [action, entity] })
  for (const value of ["decimal(3) · ucum:kg", "destination", "demo.Place", "draft · initial", "sent · terminal · immutable", "lost · terminal", "Send parcel<br>— send →", "self.mass &gt; 0", "parcelId", "demo.Positive"]) expect(html).toContain(value)
  expect(html.indexOf('<h2>Entities')).toBeLessThan(html.indexOf('<h2>Actions'))
  expect(html).toContain('<summary>Source</summary>')
  expect(html).toContain('draft → sent')
  expect(html).toContain('positive: self.mass &gt; 0')
})

test("D6: absent action stays unresolved; absent declarations invent no transitions", () => {
  const html = renderDefinitionsDocument({ entries: [entity] })
  expect(html).toContain("No action supplied")
  expect(html).not.toContain("Send parcel")
  const empty = renderDefinitionsDocument({ entries: [{ ...entity, definition: { kind: "entity", dependencies: [], payload: { entityVersion: "2.0", objectType: { name: "demo.Empty" } } } }] })
  expect(empty).not.toContain('class="transitions"')
})

test("D8: rule target, derivation and outputs are declared; structured text is escaped", () => {
  const html = renderDefinitionsDocument({ entries: [{ metadata: { ...entry.metadata, kind: "rule" }, source: '{}', definition: {
    kind: "rule", dependencies: [], payload: { ruleId: "demo.Total", appliesTo: "demo.Parcel", expression: '<script>amount</script>', outputSchema: { total: { type: "number" } } },
  } }] })
  for (const value of ["Applies to", "demo.Parcel", "Derives / checks", "Declared outputs", "total", "&lt;script&gt;amount&lt;/script&gt;"]) expect(html).toContain(value)
  expect(html).not.toContain('<script>')
})

test("D6: transition labels never cross definition versions", () => {
  const html = renderDefinitionsDocument({ entries: [action, { ...entity, metadata: { ...entity.metadata, version: "0.2.0" } }] })
  expect(html).toContain("No action supplied")
  expect(html).not.toContain("Send parcel<br>— send →")
  expect(html).not.toContain("positive: self.mass")
})


test("D9–D10: live reads preserve exact numeric source and fail closed", async () => {
  const source = '{"admitted":true,"artifacts":[{"artifactId":"demo.X","kind":"entity","name":"X","version":"1","dependencies":[],"payload":{"entityVersion":"2.0","objectType":{"name":"demo.X"}},"n":9007199254740993}]}'
  const server = Bun.serve({ port: 0, fetch(request) {
    return request.headers.get("authorization") === "Bearer read-token"
      ? new Response(source) : new Response("denied", { status: 403 })
  } })
  try {
    const entries = await readDefinitions(server.url.href, "read-token")
    expect(entries).toHaveLength(1)
    expect(entries[0]?.source).toContain('"n":9007199254740993')
    expect(renderDefinitionsDocument({ entries })).toContain("demo.X")
    await expect(readDefinitions(server.url.href, "wrong")).rejects.toThrow("HTTP 403")
  } finally { server.stop(true) }
})

test("D11–D12: badges and posting bindings reflect supplied declarations only", () => {
  const protectedEntity: DefinitionEntry = { ...entity, definition: { kind: "entity", dependencies: [], payload: { entityVersion: "2.0", objectType: {
    name: "demo.Counter", properties: [
      { id: "count", type: { kind: "integer" }, protected: true },
      { id: "total", type: { kind: "integer" }, computed: "self.count + 1" },
    ],
  } } } } as DefinitionEntry
  const posting = { ...action, definition: { ...action.definition, kind: "action", dependencies: [], payload: {
    ...action.definition?.payload, stockPost: { operation: "receive", partition: { companyField: "company" }, accounts: { inventory: "Stock", clearing: "Clearing" } },
  } } }
  const html = renderDefinitionsDocument({ entries: [protectedEntity] })
  expect(html).toContain('class="badge">Rule-owned')
  expect(html).toContain('class="badge">Derived')
  // The stock binding is an admitted forward-compatible payload extension.
  const bound = renderDefinitionsDocument({ entries: [posting as DefinitionEntry] })
  for (const label of ["Declared stock posting", "receive", "partition", "companyField", "accounts", "Clearing"]) expect(bound).toContain(label)
  expect(renderDefinitionsDocument({ entries: [action] })).not.toContain("Declared stock posting")
})
