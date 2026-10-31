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
import type { Json } from "../canonical.ts"

export type ValueSchema = StringSchema | DecimalSchema | ScalarSchema | ArraySchema | ObjectSchema | NullableSchema

export interface StringSchema {
  readonly type: "string"
  readonly enum?: ReadonlyArray<string>
}

/** Exact decimal text on ActionIR wire, decoded by the kernel. */
export interface DecimalSchema {
  readonly type: "decimal"
  readonly scale: number
}

export interface ScalarSchema {
  readonly type: "integer" | "number" | "boolean" | "null"
}

export interface ArraySchema {
  readonly type: "array"
  readonly items: ValueSchema
}

export interface ObjectSchema {
  readonly type: "object"
  readonly properties: Readonly<Record<string, ValueSchema>>
  readonly required?: ReadonlyArray<string>
  readonly additionalProperties: false
}

/** RuleIR-only nullable shape; other closed-schema profiles reject it. */
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
  if (a.type === "decimal" && b.type === "decimal") return { type: "decimal", scale: Math.max(a.scale, b.scale) }
  if (isNumeric(a) && isNumeric(b)) return a.type === b.type ? a : number
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
