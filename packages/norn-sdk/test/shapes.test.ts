import { expect, test } from "bun:test"
import document from "../../../sdks/openapi/norn-api.openapi.json"
import { conforms, SHAPES, type Check, type ShapeName } from "../src/shapes.ts"

type Json = { readonly [key: string]: unknown }
const schemas = (document as { components: { schemas: Readonly<Record<string, Json>> } }).components.schemas

/** One schema node in the vocabulary the shapes use; `$ref`s to other shapes stay by name. */
function contract(node: Json): string {
  const ref = typeof node["$ref"] === "string" ? node["$ref"].replace("#/components/schemas/", "") : null
  if (ref !== null) {
    const target = schemas[ref]!
    if (ref in SHAPES) return `shape:${ref}`
    if (Array.isArray(target["enum"])) return `enum:${ref}:${[...target["enum"]].sort().join("|")}`
    return contract(target)
  }
  const type = node["type"]
  if (Array.isArray(type)) {
    const inner = type.filter((t) => t !== "null")
    if (type.includes("null") && inner.length === 1) return `nullable:${contract({ ...node, type: inner[0] })}`
  }
  if (type === "integer") return node["minimum"] === 0 ? "count" : "integer"
  if (type === "array") return `array:${contract(node["items"] as Json)}`
  if (type === "string" || type === "boolean" || type === "object") return String(type)
  return `unmodelled:${JSON.stringify(node)}`
}

function described(check: Check): string {
  switch (check.kind) {
    case "enum": return `enum:${check.name}:${[...check.values].sort().join("|")}`
    case "nullable": return `nullable:${described(check.of)}`
    case "array": return `array:${described(check.of)}`
    case "shape": return `shape:${check.name}`
    default: return check.kind
  }
}

test("SDK-SHAPES-1: every validated success document follows the OpenAPI contract member for member", () => {
  for (const [name, shape] of Object.entries(SHAPES)) {
    const schema = schemas[name]!
    expect(schema).toBeDefined()
    const required = new Set(schema["required"] as string[])
    const properties = schema["properties"] as Readonly<Record<string, Json>>
    const expected = Object.fromEntries(Object.entries(properties).map(([member, node]) => [member, `${required.has(member) ? "required" : "optional"} ${contract(node)}`]))
    const actual = Object.fromEntries([
      ...Object.entries(shape.required).map(([member, check]) => [member, `required ${described(check)}`]),
      ...Object.entries(shape.optional).map(([member, check]) => [member, `optional ${described(check)}`]),
    ])
    expect([name, actual]).toEqual([name, expected])
  }
})

test("SDK-SHAPES-2: a document is admitted only whole", () => {
  const commit = { receiptHash: "h", commitSeq: 2, recordedAtMs: 1 }
  const receipt = { outcome: "committed", objectType: "fixture.Record", objectId: "r-1", version: 1, receipt: commit }
  expect(conforms("ActionReceipt", receipt)).toBe(true)
  expect(conforms("ActionReceipt", { ...receipt, trace: { traceId: "t" } })).toBe(true)
  // Members a newer contract adds are ignored.
  expect(conforms("ActionReceipt", { ...receipt, later: true })).toBe(true)
  const rejected: ReadonlyArray<readonly [ShapeName, unknown]> = [
    ["ActionReceipt", { ...receipt, version: 1.5 }],
    ["ActionReceipt", { ...receipt, version: -1 }],
    ["ActionReceipt", { ...receipt, version: 2 ** 53 }],
    ["ActionReceipt", { ...receipt, trace: {} }],
    ["ActionReceipt", { ...receipt, trace: { traceId: 7 } }],
    ["ActionReceipt", { ...receipt, trace: null }],
    ["ActionReceipt", { ...receipt, receipt: { ...commit, commitSeq: "2" } }],
    ["ActionReceipt", { ...receipt, outcome: "maybe" }],
    ["ActionReceipt", []],
    ["NornActionCatalog", { definitionsVersion: 1, items: [null] }],
    ["NornActionCatalog", { definitionsVersion: 1, items: [{ actionId: "a" }] }],
    ["NornActionCatalog", { definitionsVersion: 1.5, items: [] }],
    ["PendingDecision", { id: "d", expiresAtMs: 1.25 }],
  ]
  for (const [name, value] of rejected) expect([name, value, conforms(name, value)]).toEqual([name, value, false])
})
