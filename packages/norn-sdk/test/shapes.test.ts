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

/** Keys that annotate a schema without constraining a value. */
const ANNOTATIONS = new Set(["description", "title", "example", "examples", "default"])
const COMPOSITIONS = ["oneOf", "anyOf", "allOf"] as const
/** The keys each JSON type may carry here; any other key is unmodelled, so the test fails on it. */
const TYPE_KEYS: Readonly<Record<string, readonly string[]>> = {
  string: ["type", "minLength", "maxLength", "pattern", "enum"],
  integer: ["type", "format", "minimum"],
  boolean: ["type"],
  null: ["type"],
  array: ["type", "items"],
  object: ["type", "properties", "required", "additionalProperties"],
}

/**
 * A contract node's description. Every constraint is kept: a `$ref` or a composition (`oneOf`,
 * `anyOf`, `allOf`) with sibling constraints is described as the `allOf` of all of them, so a
 * sibling can never be dropped by the first keyword found.
 */
function contract(node: Json, name = ""): string {
  const parts: string[] = []
  if (typeof node["$ref"] === "string") {
    const ref = node["$ref"].replace("#/components/schemas/", "")
    parts.push(ref in SHAPES ? `shape:${ref}` : contract(schemas[ref]!, ref))
  }
  for (const composition of COMPOSITIONS) {
    const members = node[composition] as Json[] | undefined
    if (members !== undefined) parts.push(`${composition}[${sorted(members.map((m) => contract(m)))}]`)
  }
  const rest = Object.fromEntries(Object.entries(node).filter(([key]) => key !== "$ref" && !(COMPOSITIONS as readonly string[]).includes(key) && !ANNOTATIONS.has(key)))
  if (Object.keys(rest).length > 0) parts.push(typed(rest, name))
  return parts.length === 1 ? parts[0]! : `allOf[${sorted(parts)}]`
}

function typed(node: Json, name: string): string {
  const type = node["type"]
  if (Array.isArray(type)) {
    const inner = type.filter((t) => t !== "null")
    if (type.includes("null") && inner.length === 1) return `nullable(${typed({ ...node, type: inner[0] }, name)})`
    return `unmodelled:${JSON.stringify(node)}`
  }
  const allowed = typeof type === "string" ? TYPE_KEYS[type] : undefined
  if (allowed === undefined || Object.keys(node).some((key) => !allowed.includes(key))) return `unmodelled:${JSON.stringify(node)}`
  if (type === "null") return "null"
  if (Array.isArray(node["enum"])) {
    // An enum with any other constraint beside it (`pattern`, bounds) is not one this test models.
    if (Object.keys(node).some((key) => key !== "type" && key !== "enum")) return `unmodelled:${JSON.stringify(node)}`
    return `enum:${name}:${sorted(node["enum"] as string[])}`
  }
  if (type === "string") return stringText(node)
  if (type === "boolean") return "boolean"
  if (type === "integer") return node["minimum"] === 0 ? "count" : `integer(${JSON.stringify(node["minimum"] ?? null)})`
  if (type === "array") return `array(${contract(node["items"] as Json)})`
  // `additionalProperties` is modelled only as closed (`false`) or unconstrained (absent, `true`, or
  // a schema of annotations only, such as `{ description }`): a typed value schema would narrow the
  // map, so it fails the test until it is modelled.
  const extra = node["additionalProperties"]
  const unconstrained = extra === undefined || extra === true
    || (typeof extra === "object" && extra !== null && !Array.isArray(extra) && Object.keys(extra).every((key) => ANNOTATIONS.has(key)))
  if (extra !== false && !unconstrained) return `unmodelled:${JSON.stringify(node)}`
  const properties = node["properties"] as Readonly<Record<string, Json>> | undefined
  const requiredNames = (node["required"] as string[] | undefined) ?? []
  // A required member the object does not declare is a constraint this description cannot render.
  if (requiredNames.some((member) => properties === undefined || !Object.hasOwn(properties, member))) return `unmodelled:${JSON.stringify(node)}`
  if (properties === undefined) return extra === false ? "object(closed){}" : "record"
  const mandatory = new Set(requiredNames)
  // Null-prototype maps: a member named `__proto__` is a member, never a prototype.
  const required: Record<string, string> = Object.create(null), optional: Record<string, string> = Object.create(null)
  for (const [member, child] of Object.entries(properties)) (mandatory.has(member) ? required : optional)[member] = contract(child)
  return objectText(required, optional, node["additionalProperties"] === false)
}

function described(check: Check): string {
  switch (check.kind) {
    case "string": return stringText(check)
    case "enum": return `enum:${check.name}:${sorted(check.values)}`
    case "nullable": return `nullable(${described(check.of)})`
    case "array": return `array(${described(check.of)})`
    case "oneOf": case "anyOf": case "allOf": return `${check.kind}[${sorted(check.of.map(described))}]`
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
    // Prototype names are members like any other: a closed object refuses them too.
    ["TraceContext", { traceId: "t", constructor: "extra" }],
    ["TraceContext", { traceId: "t", toString: "extra" }],
    ["TraceContext", { traceId: "t", valueOf: "extra" }],
    ["TraceContext", JSON.parse('{ "traceId": "t", "__proto__": "extra" }')],
    ["NornIrRefusalReason", { law: "l", subject: "s", values: {}, hasOwnProperty: 1 }],
    // A required member is the document's own, never one inherited from Object.prototype.
    ["RefusalSpan", Object.create({ pointer: "/input/a" })],
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

test("SDK-SHAPES-3: the contract description keeps every constraint, whatever a schema combines", () => {
  // A composition with sibling constraints is all of them, never just the first keyword.
  expect(contract({ oneOf: [{ type: "null" }, { type: "string" }], type: "string", minLength: 1 })).toBe("allOf[oneOf[null,string(||)],string(1||)]")
  expect(contract({ $ref: "#/components/schemas/RefusalSpan", type: "object", required: ["x"], properties: { x: { type: "boolean" } } })).toBe("allOf[object{x!:boolean},shape:RefusalSpan]")
  // A member named `__proto__` is a member.
  expect(contract({ type: "object", required: ["__proto__"], properties: JSON.parse('{"__proto__":{"type":"boolean"}}') })).toBe("object{__proto__!:boolean}")
  // A constraint the description does not model fails loudly.
  expect(contract({ type: "string", format: "date-time" })).toStartWith("unmodelled:")
  expect(contract({ type: "integer", minimum: 0, maximum: 5 })).toStartWith("unmodelled:")
  // An enum's sibling constraint, a typed map value, and a required member with no declaration.
  expect(contract({ type: "string", enum: ["committed"], pattern: "^committed$" })).toStartWith("unmodelled:")
  expect(contract({ type: "object", additionalProperties: { type: "string" } })).toStartWith("unmodelled:")
  expect(contract({ type: "object", properties: {}, additionalProperties: { type: "string" } })).toStartWith("unmodelled:")
  expect(contract({ type: "object", required: ["missing"], properties: { x: { type: "boolean" } } })).toStartWith("unmodelled:")
  expect(contract({ type: "object", required: ["missing"] })).toStartWith("unmodelled:")
  // The unconstrained forms stay one description.
  for (const open of [{ type: "object" }, { type: "object", additionalProperties: true }, { type: "object", additionalProperties: {} }, { type: "object", additionalProperties: { description: "any JSON" } }]) expect(contract(open)).toBe("record")
})
