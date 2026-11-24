import { expect, test } from "bun:test"
import document from "../../../sdks/openapi/norn-api.openapi.json"
import { conforms, SHAPES, type Check, type ShapeName } from "../src/shapes.ts"

type Json = { readonly [key: string]: unknown }
const schemas = (document as { components: { schemas: Readonly<Record<string, Json>> } }).components.schemas
const sorted = (items: readonly string[]) => [...items].sort().join(",")

/** A description both sides render: a contract schema node, and a shape's check. */
function objectText(required: Readonly<Record<string, string>>, optional: Readonly<Record<string, string>>, closed: boolean): string {
  const members = [
    ...Object.entries(required).map(([member, text]) => `${member}!:${text}`),
    ...Object.entries(optional).map(([member, text]) => `${member}?:${text}`),
  ].sort()
  return `object${closed ? "(closed)" : ""}{${members.join(";")}}`
}

function stringText(node: { minLength?: unknown; maxLength?: unknown; pattern?: unknown }): string {
  return `string(${[node.minLength ?? "", node.maxLength ?? "", node.pattern ?? ""].join("|")})`
}

function contract(node: Json, name = ""): string {
  if (typeof node["$ref"] === "string") {
    const ref = node["$ref"].replace("#/components/schemas/", "")
    return ref in SHAPES ? `shape:${ref}` : contract(schemas[ref]!, ref)
  }
  for (const union of ["oneOf", "anyOf"] as const) {
    const members = node[union] as Json[] | undefined
    if (members === undefined) continue
    const others = members.filter((m) => m["type"] !== "null")
    if (others.length === 1 && members.length === 2) return `nullable(${contract(others[0]!)})`
    return `oneOf[${sorted(members.map((m) => contract(m)))}]`
  }
  if (Array.isArray(node["allOf"])) {
    // Refusal@2: an optional-details part (`oneOf` an object or null) and the required envelope.
    const required: Record<string, string> = {}, optional: Record<string, string> = {}
    for (const part of node["allOf"] as Json[]) {
      const object = (part["oneOf"] as Json[] | undefined)?.find((m) => m["type"] === "object") ?? part
      const mandatory = new Set((part["required"] as string[] | undefined) ?? [])
      for (const [member, child] of Object.entries((object["properties"] ?? {}) as Readonly<Record<string, Json>>)) {
        (mandatory.has(member) ? required : optional)[member] = contract(child)
      }
    }
    return objectText(required, optional, false)
  }
  const type = node["type"]
  if (Array.isArray(type)) {
    const inner = type.filter((t) => t !== "null")
    if (type.includes("null") && inner.length === 1) return `nullable(${contract({ ...node, type: inner[0] })})`
  }
  if (Array.isArray(node["enum"])) return `enum:${name}:${sorted(node["enum"] as string[])}`
  if (type === "string") return stringText(node)
  if (type === "boolean") return "boolean"
  if (type === "integer") return node["minimum"] === 0 ? "count" : "integer"
  if (type === "array") return `array(${contract(node["items"] as Json)})`
  if (type === "object") {
    const properties = node["properties"] as Readonly<Record<string, Json>> | undefined
    if (properties === undefined) return "record"
    const mandatory = new Set((node["required"] as string[] | undefined) ?? [])
    const required: Record<string, string> = {}, optional: Record<string, string> = {}
    for (const [member, child] of Object.entries(properties)) (mandatory.has(member) ? required : optional)[member] = contract(child)
    return objectText(required, optional, node["additionalProperties"] === false)
  }
  return `unmodelled:${JSON.stringify(node)}`
}

function described(check: Check): string {
  switch (check.kind) {
    case "string": return stringText(check)
    case "enum": return `enum:${check.name}:${sorted(check.values)}`
    case "nullable": return `nullable(${described(check.of)})`
    case "array": return `array(${described(check.of)})`
    case "oneOf": return `oneOf[${sorted(check.of.map(described))}]`
    case "shape": return `shape:${check.name}`
    case "object": {
      const render = (members: Readonly<Record<string, Check>>) => Object.fromEntries(Object.entries(members).map(([m, c]) => [m, described(c)]))
      return objectText(render(check.shape.required), render(check.shape.optional), check.shape.closed === true)
    }
    default: return check.kind
  }
}

test("SDK-SHAPES-1: every validated document follows the OpenAPI contract, constraint for constraint", () => {
  for (const [name, check] of Object.entries(SHAPES)) {
    const schema = schemas[name]
    expect([name, schema === undefined]).toEqual([name, false])
    expect([name, described(check)]).toEqual([name, contract(schema!, name)])
  }
})

test("SDK-SHAPES-2: a document is admitted only whole", () => {
  const commit = { receiptHash: "h", commitSeq: 2, recordedAtMs: 1 }
  const receipt = { outcome: "committed", objectType: "fixture.Record", objectId: "r-1", version: 1, receipt: commit }
  const digest = `sha256:${"a".repeat(64)}`
  const refusal = { format: "norn/refusal@2", code: "x.y", category: "conflict", message: "m", explanation: "e", retryable: false, reason: null }
  const admitted: ReadonlyArray<readonly [ShapeName, unknown]> = [
    ["ActionReceipt", receipt],
    ["ActionReceipt", { ...receipt, trace: { traceId: "t", intentRef: "i", sessionDigest: digest } }],
    // An open object ignores members a newer contract adds.
    ["ActionReceipt", { ...receipt, later: true }],
    ["Refusal", refusal],
    ["Refusal", { ...refusal, decidedAt: "2026-01-01T00:00:00Z", diagnostics: [{ code: "c", severity: "error", message: "m", span: { pointer: "/input/a" } }] }],
    ["Refusal", { ...refusal, reason: { law: "l", subject: "s", values: {}, pin: { kind: "law", artifactId: "a", lawId: "l", version: "1" } } }],
    ["Refusal", { ...refusal, reason: { law: "l", subject: "s", values: {}, pin: { kind: "invariant", owner: { kind: "object", name: "fixture.Record" }, invariantId: "total_ok", version: "1" } } }],
  ]
  for (const [name, value] of admitted) expect([name, value, conforms(name, value)]).toEqual([name, value, true])
  const rejected: ReadonlyArray<readonly [ShapeName, unknown]> = [
    ["ActionReceipt", { ...receipt, version: 1.5 }],
    ["ActionReceipt", { ...receipt, version: -1 }],
    ["ActionReceipt", { ...receipt, version: 2 ** 53 }],
    ["ActionReceipt", { ...receipt, trace: {} }],
    ["ActionReceipt", { ...receipt, trace: { traceId: "" } }],
    ["ActionReceipt", { ...receipt, trace: { traceId: "t".repeat(257) } }],
    ["ActionReceipt", { ...receipt, trace: { traceId: "t", sessionDigest: "wrong" } }],
    ["ActionReceipt", { ...receipt, trace: { traceId: "t", intentRef: "" } }],
    // A closed object refuses a member it does not declare.
    ["ActionReceipt", { ...receipt, trace: { traceId: "t", extra: 1 } }],
    ["ActionReceipt", { ...receipt, trace: null }],
    ["ActionReceipt", { ...receipt, receipt: { ...commit, commitSeq: "2" } }],
    ["ActionReceipt", { ...receipt, outcome: "maybe" }],
    ["ActionReceipt", []],
    ["NornActionCatalog", { definitionsVersion: 1, items: [null] }],
    ["NornActionCatalog", { definitionsVersion: 1, items: [{ actionId: "a" }] }],
    ["NornActionCatalog", { definitionsVersion: 1.5, items: [] }],
    ["PendingDecision", { id: "d", expiresAtMs: 1.25 }],
    // Refusal@2 is a whole envelope, never a bare `{ format, code }`.
    ["Refusal", { format: "norn/refusal@2", code: "x.y" }],
    ["Refusal", { ...refusal, format: "norn/refusal@1" }],
    ["Refusal", { ...refusal, category: "law" }],
    ["Refusal", { ...refusal, retryable: "no" }],
    ["Refusal", (({ reason: _, ...rest }) => rest)(refusal)],
    ["Refusal", (({ explanation: _, ...rest }) => rest)(refusal)],
    ["Refusal", { ...refusal, reason: { law: "l" } }],
    ["Refusal", { ...refusal, reason: { law: "l", subject: "s", values: {}, extra: 1 } }],
    ["Refusal", { ...refusal, reason: { law: "l", subject: "s", values: {}, pin: { kind: "law", artifactId: "a" } } }],
    ["Refusal", { ...refusal, reason: { law: "l", subject: "s", values: {}, pin: { kind: "invariant", owner: { kind: "object", name: "Record" }, invariantId: "x", version: "1" } } }],
    ["Refusal", { ...refusal, diagnostics: [null] }],
    ["Refusal", { ...refusal, diagnostics: [{ code: "c", message: "m", pointer: "/input/a" }] }],
  ]
  for (const [name, value] of rejected) expect([name, value, conforms(name, value)]).toEqual([name, value, false])
})
