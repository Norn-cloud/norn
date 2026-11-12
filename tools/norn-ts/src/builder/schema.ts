/**
 * Closed value schemas (T7-FLOW-1).
 *
 * The one schema language the builder emits is the checker's closed-schema
 * subset (`norn-checker` `validate_closed_schema`): one explicit `type`;
 * objects carry `properties`, optional `required`, and
 * `additionalProperties: false`; arrays carry `items`; strings may carry an
 * `enum` in RuleIR v2. RuleIR decisions additionally admit the existing nullable type pair. Function I/O, flow events, result
 * slots, and decision contracts are all written in it, and the Rust checker
 * validates every one; nothing here is a second type authority.
 */
import type { PropertyType } from "./entity.ts"
import type { Json } from "../canonical.ts"

export type ValueSchema = StringSchema | ScalarSchema | ArraySchema | ObjectSchema | NullableSchema

/**
 * Display-only labels (D66). The checker admits them on ActionIR input
 * schemas only; they never change the parsed type.
 */
export interface Labels {
  readonly title?: string
  readonly description?: string
}

/** One wizard step over an ActionIR input: fields are RFC 6901 pointers. */
export interface InputGroup {
  readonly id: string
  readonly title: string
  readonly fields: ReadonlyArray<string>
}

export interface StringSchema extends Labels {
  readonly type: "string"
  readonly enum?: ReadonlyArray<string>
}

export interface ScalarSchema extends Labels {
  readonly type: "integer" | "number" | "boolean" | "null"
  readonly nornType?: Extract<PropertyType, {readonly kind: "decimal"}>
}

export interface ArraySchema extends Labels {
  readonly type: "array"
  readonly items: ValueSchema
}

export interface ObjectSchema extends Labels {
  readonly type: "object"
  readonly properties: Readonly<Record<string, ValueSchema>>
  readonly required?: ReadonlyArray<string>
  readonly additionalProperties: false
  /** ActionIR input roots only. */
  readonly "x-norn"?: { readonly groups: ReadonlyArray<InputGroup> }
}

/**
 * A nullable closed schema. RuleIR admits every shape; ActionIR inputs and
 * outputs admit only a nullable scalar (`["string" | "integer" | "number" | "boolean", "null"]`,
 * with a string `enum` or a decimal `nornType` beside it), which the compiler enforces.
 */
export type NullableSchema =
  | (Omit<StringSchema, "type"> & { readonly type: readonly ["string", "null"] })
  | { readonly type: readonly ["integer" | "number" | "boolean", "null"] }
  | (Omit<ArraySchema, "type"> & { readonly type: readonly ["array", "null"] })
  | (Omit<ObjectSchema, "type"> & { readonly type: readonly ["object", "null"] })

export function nullable(schema: ValueSchema): ValueSchema {
  if (schema.type === "null" || Array.isArray(schema.type)) return schema
  return { ...schema, type: [schema.type, "null"] } as NullableSchema
}

export const text = (values?: ReadonlyArray<string>): StringSchema =>
  values === undefined ? { type: "string" } : { type: "string", enum: [...values] }
export const integer: ScalarSchema = { type: "integer" }
export const number: ScalarSchema = { type: "number" }
export const boolean: ScalarSchema = { type: "boolean" }
export const nullValue: ScalarSchema = { type: "null" }
export const array = (items: ValueSchema): ArraySchema => ({ type: "array", items })

/**
 * A closed record. Every field is required unless listed in `optional`;
 * `required` is omitted when nothing is required.
 */
export function record(
  fields: Readonly<Record<string, ValueSchema>>,
  optional: ReadonlyArray<string> = [],
): ObjectSchema {
  const required = Object.keys(fields).filter((name) => !optional.includes(name))
  return required.length > 0
    ? { type: "object", properties: { ...fields }, required, additionalProperties: false }
    : { type: "object", properties: { ...fields }, additionalProperties: false }
}

export const isScalar = (schema: ValueSchema): boolean =>
  schema.type !== "object" && schema.type !== "array"

export const isNumeric = (schema: ValueSchema): boolean => schema.type === "integer" || schema.type === "number"

/** The schema of a JSON literal; strings are plain `string` (no singleton enum). */
export function literalSchema(value: Json): ValueSchema | undefined {
  if (value === null) return nullValue
  switch (typeof value) {
    case "string":
      return text()
    case "boolean":
      return boolean
    case "number":
      return Number.isInteger(value) ? integer : number
    default:
      return undefined
  }
}

/**
 * The least closed schema admitting both, or a reason they cannot be merged
 * (the closed subset has no unions). `integer` widens to `number`; string
 * enums merge to their union, or to plain `string` when either side is open;
 * a record field present on one side only becomes optional.
 */
export function mergeSchemas(a: ValueSchema, b: ValueSchema, path = "$"): ValueSchema | string {
  if (a.type === "string" && b.type === "string") {
    if (a.enum === undefined || b.enum === undefined) return text()
    return text([...new Set([...a.enum, ...b.enum])])
  }
  if (isNumeric(a) && isNumeric(b)) {
    const exactA = "nornType" in a ? a.nornType : undefined
    const exactB = "nornType" in b ? b.nornType : undefined
    if (exactA !== undefined || exactB !== undefined) return JSON.stringify(exactA) === JSON.stringify(exactB) ? a : `${path}: different exact numeric contracts`
    return a.type === b.type ? a : number
  }
  if (a.type !== b.type) return `${path}: ${a.type} and ${b.type} have no closed union`
  if (a.type === "array" && b.type === "array") {
    const items = mergeSchemas(a.items, b.items, `${path}[]`)
    return typeof items === "string" ? items : array(items)
  }
  if (a.type === "object" && b.type === "object") {
    const names = [...new Set([...Object.keys(a.properties), ...Object.keys(b.properties)])]
    const fields: Record<string, ValueSchema> = {}
    const optional: string[] = []
    const requiredA = new Set(a.required ?? [])
    const requiredB = new Set(b.required ?? [])
    for (const name of names) {
      const left = a.properties[name]
      const right = b.properties[name]
      if (left && right) {
        const merged = mergeSchemas(left, right, `${path}.${name}`)
        if (typeof merged === "string") return merged
        fields[name] = merged
        if (!requiredA.has(name) || !requiredB.has(name)) optional.push(name)
      } else {
        fields[name] = (left ?? right)!
        optional.push(name)
      }
    }
    return record(fields, optional)
  }
  return a
}

/** The closed record with no members. */
export const emptyRecord: ObjectSchema = record({})

/** Retain the exact declaration when choosing the bounded expression profile. */
export function hasExact(schema: ValueSchema): boolean {
  if ("nornType" in schema && schema.nornType !== undefined) return true
  if ("properties" in schema) return Object.values(schema.properties).some(hasExact)
  return "items" in schema && hasExact(schema.items)
}

/** FOLD-OPERANDS: every value admitted by actual also satisfies expected. */
export function acceptsSchema(expected: ValueSchema, actual: ValueSchema): boolean {
  const nonnull = (s: ValueSchema): ValueSchema => Array.isArray(s.type) ? { ...s, type: s.type[0] } as ValueSchema : s
  if (actual.type === "null") return expected.type === "null" || Array.isArray(expected.type)
  if (Array.isArray(actual.type)) return Array.isArray(expected.type) && acceptsSchema(nonnull(expected), nonnull(actual))
  if (Array.isArray(expected.type)) return acceptsSchema(nonnull(expected), actual)
  if (expected.type === "number" && actual.type === "integer") return true
  if (expected.type !== actual.type) return false
  if (expected.type === "string" && actual.type === "string") {
    const values = expected.enum
    return values === undefined || (actual.enum !== undefined && actual.enum.every(v => values.includes(v)))
  }
  if (expected.type === "array" && actual.type === "array") return acceptsSchema(expected.items, actual.items)
  if (expected.type === "object" && actual.type === "object") {
    return (expected.required ?? []).every(k => actual.required?.includes(k))
      && Object.entries(actual.properties).every(([k, v]) => expected.properties[k] !== undefined && acceptsSchema(expected.properties[k], v))
  }
  return true
}
