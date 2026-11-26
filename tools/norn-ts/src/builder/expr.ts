/**
 * Typed expression nodes and their ZEN Expression rendering (T7-FLOW-1).
 *
 * Expressions v1 of ruling 2026-09-27 authoring-effect-profile §3: literals,
 * immutable references with declared property paths, bounded array and record
 * construction, scalar `==`/`!=`, numeric ordering, and strict boolean
 * `and`/`or`/`not`. Nodes are plain data. ZEN text is rendered from the nodes
 * by {@link render}; source text is never copied into an expression.
 * {@link typeOf} types a node against the schemas of the references in scope,
 * so a read of an undeclared path is a `NORN-FLOW-SLICE` finding before the
 * Rust checker sees the artifact.
 */
import { assignable } from "./assignable.ts"
import type { Json } from "../canonical.ts"
import {
  type ValueSchema,
  array,
  boolean,
  number as numberSchema,
  text,
  integer,
  isNumeric,
  isScalar,
  literalSchema,
  mergeSchemas,
  nullable,
  nonNull,
  record as recordSchema,
} from "./schema.ts"

export type CallFn =
  | "concat"
  | "lower"
  | "upper"
  | "trim"
  | "len"
  | "take"
  | "contains"
  | "startsWith"
  | "endsWith"
  | "dateBefore"
  | "timestampBefore"
  | "timestampAtOrBefore"
  | "date"
  | "timestamp"
  | "sum"
  | "floor"
  | "days_between"
  | "add_days"
  | "seconds_between"
  | "add_seconds"
  | "intervals_overlap"
  | "interval_gap_seconds"
  | "interval_intersection_seconds"
  | "resolve_local_timestamp"
/** norn-zen/6 closures (`crates/norn-expr/README.md`): the body sees its own bound element. */
export type ClosureFn = "some" | "all" | "none" | "one" | "filter" | "count" | "map"
export type CompareOp = "==" | "!=" | "<" | "<=" | ">" | ">="
export type LogicalOp = "and" | "or"

type BinderId = number & { readonly __binder: unique symbol }
let nextBinder = 0

export type Expr =
  | { readonly _tag: "Binary"; readonly op: "+" | "-" | "*" | "/" | "%" | "in"; readonly left: Expr; readonly right: Expr }
  | { readonly _tag: "Call"; readonly fn: CallFn; readonly args: ReadonlyArray<Expr> }
  | { readonly _tag: "Closure"; readonly fn: ClosureFn; readonly list: Expr; readonly binder: BinderId; readonly body: Expr }
  | { readonly _tag: "Conditional"; readonly condition: Expr; readonly yes: Expr; readonly no: Expr }
  | { readonly _tag: "Coalesce"; readonly value: Expr; readonly fallback: Expr }
  | { readonly _tag: "Bound"; readonly binder: BinderId; readonly path: ReadonlyArray<string> }
  | { readonly _tag: "Literal"; readonly value: string | number | boolean | null }
  | { readonly _tag: "Input"; readonly name: string; readonly path: ReadonlyArray<string> }
  | { readonly _tag: "Compare"; readonly op: CompareOp; readonly left: Expr; readonly right: Expr }
  | { readonly _tag: "Logical"; readonly op: LogicalOp; readonly left: Expr; readonly right: Expr }
  | { readonly _tag: "Not"; readonly operand: Expr }
  | { readonly _tag: "List"; readonly items: ReadonlyArray<Expr> }
  | { readonly _tag: "Record"; readonly fields: ReadonlyArray<readonly [string, Expr]> }

export const binary = (op: "+" | "-" | "*" | "/" | "%" | "in", left: Expr, right: Expr): Expr => ({ _tag: "Binary", op, left, right })
export const call = (fn: Extract<Expr, { _tag: "Call" }>["fn"], ...args: Expr[]): Expr => ({ _tag: "Call", fn, args })
export const conditional = (condition: Expr, yes: Expr, no: Expr): Expr => ({ _tag: "Conditional", condition, yes, no })
export const coalesce = (value: Expr, fallback: Expr): Expr => ({ _tag: "Coalesce", value, fallback })

/** `sum(list of numbers)`; zen sums an empty list to 0. */
export const sum = (list: Expr): Expr => call("sum", list)
/** `floor(number)`: exact integer floor in `norn-zen/7`. */
export const floor = (value: Expr): Expr => call("floor", value)
/** `take(text, maxScalars)`: bounded Unicode scalar prefix in `norn-zen/7`. */
export const take = (value: Expr, maxScalars: Expr): Expr => call("take", value, maxScalars)
/** A calendar date operand: raw date text is parsed through `date(...)`, as norn-zen/7 requires. */
const asDate = (value: Expr): Expr => (temporalKind(value) === "date" ? value : call("date", value))
/** A UTC timestamp operand: raw timestamp text is parsed through `timestamp(...)`, as norn-zen/7 requires. */
const asTimestamp = (value: Expr): Expr => (temporalKind(value) === "timestamp" ? value : call("timestamp", value))
/** `days_between(start, end)` = signed calendar days `end - start`; operands are parsed as dates. */
export const daysBetween = (start: Expr, end: Expr): Expr => call("days_between", asDate(start), asDate(end))
/** `add_days(date, days)`: a parsed date, comparable with other parsed dates. */
export const addDays = (date: Expr, days: Expr): Expr => call("add_days", asDate(date), days)
/** `timestamp(left) < timestamp(right)`: strict UTC instant ordering. */
export const timestampBefore = (left: Expr, right: Expr): Expr => call("timestampBefore", asTimestamp(left), asTimestamp(right))
/** `timestamp(left) <= timestamp(right)`: non-strict UTC instant ordering. */
export const timestampAtOrBefore = (left: Expr, right: Expr): Expr => call("timestampAtOrBefore", asTimestamp(left), asTimestamp(right))
/** `seconds_between(start, end)` = signed exact decimal seconds `end - start`; operands are parsed as timestamps. */
export const secondsBetween = (start: Expr, end: Expr): Expr => call("seconds_between", asTimestamp(start), asTimestamp(end))
/** `add_seconds(timestamp, seconds)`: a parsed timestamp, comparable with other parsed timestamps. */
export const addSeconds = (ts: Expr, seconds: Expr): Expr => call("add_seconds", asTimestamp(ts), seconds)
/** `intervals_overlap(aStart, aEnd, bStart, bEnd)`: half-open `[start, end)` overlap; refuses improper intervals. */
export const intervalsOverlap = (aStart: Expr, aEnd: Expr, bStart: Expr, bEnd: Expr): Expr =>
  call("intervals_overlap", asTimestamp(aStart), asTimestamp(aEnd), asTimestamp(bStart), asTimestamp(bEnd))
/** `interval_gap_seconds(aStart, aEnd, bStart, bEnd)`: exact decimal seconds separating two proper intervals. */
export const intervalGapSeconds = (aStart: Expr, aEnd: Expr, bStart: Expr, bEnd: Expr): Expr =>
  call("interval_gap_seconds", asTimestamp(aStart), asTimestamp(aEnd), asTimestamp(bStart), asTimestamp(bEnd))
/** `interval_intersection_seconds(aStart, aEnd, bStart, bEnd)`: exact decimal seconds of overlap between two proper intervals. */
export const intervalIntersectionSeconds = (aStart: Expr, aEnd: Expr, bStart: Expr, bEnd: Expr): Expr =>
  call("interval_intersection_seconds", asTimestamp(aStart), asTimestamp(aEnd), asTimestamp(bStart), asTimestamp(bEnd))
/** `resolve_local_timestamp(localWallTime, ianaTz, dstPolicy)`: pinned IANA tzdb resolution returning a parsed timestamp. */
export const resolveLocalTimestamp = (localWallTime: Expr, ianaTz: Expr, dstPolicy: Expr): Expr =>
  call("resolve_local_timestamp", localWallTime, ianaTz, dstPolicy)

/** Bind the element by identity; author parameter spellings never enter the IR. */
export function closure(fn: ClosureFn, list: Expr, body: (element: Expr) => Expr): Expr {
  const binder = nextBinder++ as BinderId
  return { _tag: "Closure", fn, list, binder, body: body({ _tag: "Bound", binder, path: [] }) }
}
const closureOf = (fn: ClosureFn) => (list: Expr, body: (element: Expr) => Expr) => closure(fn, list, body)
export const some = closureOf("some")
export const all = closureOf("all")
export const none = closureOf("none")
export const one = closureOf("one")
export const filter = closureOf("filter")
export const count = closureOf("count")
export const map = closureOf("map")

/** The parsed temporal kind of `expr`, if it is a parsed date or timestamp value. */
export function temporalKind(expr: Expr): "date" | "timestamp" | undefined {
  if (expr._tag !== "Call") return undefined
  if (expr.fn === "date" || expr.fn === "add_days") return "date"
  if (expr.fn === "timestamp" || expr.fn === "add_seconds" || expr.fn === "resolve_local_timestamp") return "timestamp"
  return undefined
}

export const lit = (value: string | number | boolean | null): Expr => ({ _tag: "Literal", value })
/** A read of `name` (a binding in scope) and a declared property path below it. */
export const ref = (name: string, ...path: ReadonlyArray<string>): Expr => ({ _tag: "Input", name, path: [...path] })
export function field(reference: Expr, key: string): Expr {
  if (reference._tag !== "Input" && reference._tag !== "Bound") throw new ExprError("NORN-FLOW-SLICE", "expected a property path")
  return { ...reference, path: [...reference.path, key] }
}
const compare =
  (op: CompareOp) =>
  (left: Expr, right: Expr): Expr => ({ _tag: "Compare", op, left, right })
export const eq = compare("==")
export const ne = compare("!=")
export const lt = compare("<")
export const le = compare("<=")
export const gt = compare(">")
export const ge = compare(">=")
export const and = (left: Expr, right: Expr): Expr => ({ _tag: "Logical", op: "and", left, right })
export const or = (left: Expr, right: Expr): Expr => ({ _tag: "Logical", op: "or", left, right })
export const not = (operand: Expr): Expr => ({ _tag: "Not", operand })
export const list = (...items: ReadonlyArray<Expr>): Expr => ({ _tag: "List", items: [...items] })
/** A record literal; fields keep their written order. */
export const record = (fields: Readonly<Record<string, Expr>>): Expr => ({
  _tag: "Record",
  fields: Object.entries(fields),
})

/**
 * How a reference reads in the current decision row, and what it holds:
 * either a ZEN path (`event.results.load`, `slice.run.slots.bindings.invoice`)
 * or an expression bound earlier in the same row (a failure record raised and
 * caught without a durable boundary between), rendered in its own scope.
 */
export type Binding =
  | { readonly zen: string; readonly schema: ValueSchema }
  | { readonly inline: Expr; readonly scope: Scope; readonly schema: ValueSchema }

export type Scope = ReadonlyMap<string, Binding>

export class ExprError extends Error {
  constructor(
    readonly code: string,
    message: string,
  ) {
    super(message)
  }
}

const IDENT = /^[A-Za-z_][A-Za-z0-9_]*$/

/** A ZEN string literal; v1 admits no escape sequences (their ZEN semantics are not qualified yet). */
export function zenString(value: string): string {
  if (/[\\\n\r\u0000-\u001f]/.test(value) || (value.includes("'") && value.includes('"'))) {
    throw new ExprError("NORN-FLOW-EXPRESSION", `string ${JSON.stringify(value)} needs escapes, which ZEN v1 lowering does not emit`)
  }
  return value.includes("'") ? `"${value}"` : `'${value}'`
}

function zenNumber(value: number): string {
  const text = String(value)
  if (!Number.isFinite(value) || /e/i.test(text) || (Number.isInteger(value) && !Number.isSafeInteger(value))) {
    throw new ExprError("NORN-FLOW-EXPRESSION", `number ${text} has no exact ZEN literal`)
  }
  return text
}

export function zenKey(name: string): string {
  if (!IDENT.test(name)) throw new ExprError("NORN-FLOW-EXPRESSION", `record key ${JSON.stringify(name)} is not an identifier`)
  return name
}

/** Render a JSON value as a ZEN literal (used for generated fact, command, and timer records). */
export function zenJson(value: Json): string {
  if (value === null) return "null"
  if (typeof value === "string") return zenString(value)
  if (typeof value === "number") return zenNumber(value)
  if (typeof value === "boolean") return String(value)
  if (Array.isArray(value)) return `[${value.map((v) => zenJson(v)).join(", ")}]`
  return `{${Object.entries(value).map(([k, v]) => `${zenKey(k)}: ${zenJson(v as Json)}`).join(", ")}}`
}

/** FOLD-STEP-TYPE: record paths and bounded inline list indexes retain their declared schema. */
function resolve(binding: Binding | undefined, name: string, path: ReadonlyArray<string>): { readonly zen: string; readonly schema: ValueSchema } {
  if (!binding) {
    throw new ExprError("NORN-FLOW-SLICE", `\`${name}\` is not bound here`)
  }
  let schema = binding.schema
  let optional = false
  let zenPath = "zen" in binding ? binding.zen : ""
  for (const segment of path) {
    const arrayIndex = schema.type === "array" && /^(0|[1-9][0-9]*)$/.test(segment) && Number.isSafeInteger(Number(segment))
    if (arrayIndex && "zen" in binding) {
      throw new ExprError("NORN-FLOW-SLICE", "runtime array indexes are outside the admitted expression profile")
    }
    if (schema.type === "object" && !schema.required?.includes(segment)) optional = true
    const member = schema.type === "array" && arrayIndex ? schema.items : schema.type === "object" && Object.hasOwn(schema.properties, segment) ? schema.properties[segment] : undefined
    if (!member) {
      throw new ExprError("NORN-FLOW-SLICE", `expected a declared field, got ${segment} on ${name}; did you mean ${schema.type === "object" ? Object.keys(schema.properties).join(", ") : "a record field"}?`)
    }
    zenPath += arrayIndex ? `[${segment}]` : `${zenPath ? "." : ""}${zenKey(segment)}`
    schema = member
  }
  if (optional) schema = nullable(schema)
  if ("zen" in binding) return { zen: zenPath, schema }
  // An inline record: follow the path through its fields at build time.
  let expr = binding.inline
  for (const [index, segment] of path.entries()) {
    if (expr._tag === "Record") {
      const field = expr.fields.find(([key]) => key === segment)
      if (!field) throw new ExprError("NORN-FLOW-SLICE", `\`${[name, ...path].join(".")}\` reads ${segment}, which is not set`)
      expr = field[1]
    } else if (expr._tag === "List" && /^(0|[1-9][0-9]*)$/.test(segment)) {
      const item = expr.items[Number(segment)]
      if (!item) throw new ExprError("NORN-FLOW-SLICE", `list index ${segment} is not set`)
      expr = item
    } else if (expr._tag === "Input" || expr._tag === "Bound") {
      expr = { ...expr, path: [...expr.path, ...path.slice(index)] }
      break
    } else {
      throw new ExprError("NORN-FLOW-SLICE", `\`${[name, ...path].join(".")}\` reads into a non-record value`)
    }
  }
  return { zen: operand(expr, binding.scope, new Map()), schema }
}

type Binders = ReadonlyMap<BinderId, { zen: string; schema: ValueSchema }>

/** Reserve even bare column names (a decision input renders with an empty root). */
function freshAlias(scope: Scope, binders: Binders, element: ValueSchema): string {
  const used = new Set<string>()
  const schemas = new Set<ValueSchema>()
  const scopes = new Set<Scope>()
  function reserveSchema(schema: ValueSchema): void {
    if (schemas.has(schema)) return
    schemas.add(schema)
    if ("items" in schema) reserveSchema(schema.items)
    if ("properties" in schema) for (const [key, member] of Object.entries(schema.properties)) {
      used.add(key)
      reserveSchema(member)
    }
  }
  function reserveScope(bindings: Scope): void {
    if (scopes.has(bindings)) return
    scopes.add(bindings)
    for (const [name, binding] of bindings) {
      used.add(name)
      reserveSchema(binding.schema)
      if ("inline" in binding) reserveScope(binding.scope)
      else for (const id of binding.zen.match(/[A-Za-z_][A-Za-z0-9_]*/g) ?? []) used.add(id)
    }
  }
  reserveScope(scope)
  reserveSchema(element)
  for (const binding of binders.values()) {
    used.add(binding.zen)
    reserveSchema(binding.schema)
  }
  let i = 0
  while (used.has(`b${i}`)) i++
  return `b${i}`
}

/** The closed schema of `expr` in `scope`, or {@link ExprError}. */
export function typeOf(expr: Expr, scope: Scope): ValueSchema {
  return infer(expr, scope, new Map())
}

function infer(expr: Expr, scope: Scope, binders: Binders): ValueSchema {
  switch (expr._tag) {
    case "Binary": {
      const l = infer(expr.left, scope, binders), r = infer(expr.right, scope, binders)
      if (expr.op === "in") {
        if (r.type !== "array" || typeof mergeSchemas(l, r.items) === "string") throw new ExprError("NORN-FLOW-EXPRESSION", "membership expects a value and an array of the same type")
        return boolean
      }
      if (expr.op === "+" && l.type === "string" && r.type === "string") return text()
      if (!isNumeric(l) || !isNumeric(r)) throw new ExprError("NORN-FLOW-EXPRESSION", `expected numbers${expr.op === "+" ? " or two strings" : ""}, got ${l.type} and ${r.type}`)
      return numberSchema
    }
    case "Call": {
      const args = expr.args.map((arg) => infer(arg, scope, binders))
      if (expr.fn === "concat") {
        if (args.length !== 2 || args[0]?.type !== "array" || args[1]?.type !== "array") throw new ExprError("NORN-FLOW-EXPRESSION", "concat expects two arrays")
        const merged = mergeSchemas(args[0].items, args[1].items)
        if (typeof merged === "string") throw new ExprError("NORN-FLOW-EXPRESSION", merged)
        return array(merged)
      }
      if (expr.fn === "sum") {
        if (args.length !== 1 || args[0]?.type !== "array" || !isNumeric(args[0].items)) throw new ExprError("NORN-FLOW-EXPRESSION", "sum expects a list of numbers")
        return numberSchema
      }
      if (expr.fn === "floor") {
        if (args.length !== 1 || !isNumeric(args[0]!)) throw new ExprError("NORN-FLOW-EXPRESSION", "floor expects a number")
        return integer
      }
      if (expr.fn === "days_between" || expr.fn === "add_days") {
        if (args.length !== 2) throw new ExprError("NORN-FLOW-EXPRESSION", `${expr.fn} expects 2 arguments`)
        const dates = expr.fn === "days_between" ? expr.args : expr.args.slice(0, 1)
        if (dates.some((arg) => temporalKind(arg) !== "date")) throw new ExprError("NORN-FLOW-EXPRESSION", `${expr.fn} expects parsed dates; build it with ${expr.fn === "days_between" ? "daysBetween" : "addDays"}`)
        if (expr.fn === "add_days" && args[1]!.type !== "integer") throw new ExprError("NORN-FLOW-EXPRESSION", `add_days adds a whole number of days, not ${String(args[1]!.type)}`)
        return expr.fn === "days_between" ? integer : text()
      }
      if (expr.fn === "timestampBefore" || expr.fn === "timestampAtOrBefore") {
        if (args.length !== 2 || expr.args.some((arg) => temporalKind(arg) !== "timestamp")) throw new ExprError("NORN-FLOW-EXPRESSION", `${expr.fn} expects 2 parsed timestamps`)
        return boolean
      }
      if (expr.fn === "seconds_between" || expr.fn === "add_seconds") {
        if (args.length !== 2) throw new ExprError("NORN-FLOW-EXPRESSION", `${expr.fn} expects 2 arguments`)
        const timestamps = expr.fn === "seconds_between" ? expr.args : expr.args.slice(0, 1)
        if (timestamps.some((arg) => temporalKind(arg) !== "timestamp")) throw new ExprError("NORN-FLOW-EXPRESSION", `${expr.fn} expects parsed timestamps; build it with ${expr.fn === "seconds_between" ? "secondsBetween" : "addSeconds"}`)
        if (expr.fn === "add_seconds" && args[1]!.type !== "integer") throw new ExprError("NORN-FLOW-EXPRESSION", `add_seconds adds a whole number of seconds, not ${String(args[1]!.type)}`)
        return expr.fn === "seconds_between" ? numberSchema : text()
      }
      if (expr.fn === "intervals_overlap" || expr.fn === "interval_gap_seconds" || expr.fn === "interval_intersection_seconds") {
        if (args.length !== 4 || expr.args.some((arg) => temporalKind(arg) !== "timestamp")) throw new ExprError("NORN-FLOW-EXPRESSION", `${expr.fn} expects 4 parsed timestamps`)
        return expr.fn === "intervals_overlap" ? boolean : numberSchema
      }
      if (expr.fn === "resolve_local_timestamp") {
        if (args.length !== 3 || args.some((a) => a.type !== "string")) throw new ExprError("NORN-FLOW-EXPRESSION", "resolve_local_timestamp expects 3 string arguments (localWallTime, ianaTz, dstPolicy)")
        return text()
      }
      if (expr.fn === "len") {
        if (args.length !== 1 || (args[0]?.type !== "string" && args[0]?.type !== "array")) throw new ExprError("NORN-FLOW-EXPRESSION", "length expects a string or array")
        return integer
      }
      if (expr.fn === "take") {
        if (args.length !== 2 || args[0]?.type !== "string" || args[1]?.type !== "integer") throw new ExprError("NORN-FLOW-EXPRESSION", "take expects a string and an integer literal cap")
        return text()
      }
      const arity = ["lower", "upper", "trim", "date", "timestamp"].includes(expr.fn) ? 1 : 2
      if (args.length !== arity || args.some((a) => a.type !== "string")) throw new ExprError("NORN-FLOW-EXPRESSION", `${expr.fn} expects ${arity} string arguments`)
      return arity === 1 ? text() : boolean
    }
    case "Closure": {
      const list = infer(expr.list, scope, binders)
      if (list.type !== "array") throw new ExprError("NORN-FLOW-EXPRESSION", `${expr.fn} iterates a list, not ${String(list.type)}`)
      const inner = new Map(binders).set(expr.binder, { zen: "", schema: list.items })
      const body = infer(expr.body, scope, inner)
      if (expr.fn === "map") {
        if (body.type === "null" || Array.isArray(body.type)) throw new ExprError("NORN-FLOW-EXPRESSION", "a map body must not be null")
        return array(body)
      }
      if (body.type !== "boolean") throw new ExprError("NORN-FLOW-EXPRESSION", `a ${expr.fn} body must be Boolean, not ${String(body.type)}`)
      return expr.fn === "filter" ? list : expr.fn === "count" ? integer : boolean
    }
    case "Conditional": {
      if (infer(expr.condition, scope, binders).type !== "boolean") throw new ExprError("NORN-FLOW-EXPRESSION", "conditional expects a boolean condition")
      // Empty arrays acquire the other branch's item type.
      const empty = (e: Expr) => e._tag === "List" && e.items.length === 0
      if (empty(expr.yes) || empty(expr.no)) {
        const other = infer(empty(expr.yes) ? expr.no : expr.yes, scope, binders)
        if (!("items" in other)) throw new ExprError("NORN-FLOW-EXPRESSION", "empty list needs an array on the other branch")
        return other
      }
      const merged = mergeSchemas(infer(expr.yes, scope, binders), infer(expr.no, scope, binders))
      if (typeof merged === "string") throw new ExprError("NORN-FLOW-EXPRESSION", merged)
      return merged
    }
    case "Coalesce": {
      const value = infer(expr.value, scope, binders), fallback = infer(expr.fallback, scope, binders)
      const nonnull = nonNull(value)
      const merged = value.type === "null" ? fallback : mergeSchemas(nonnull, fallback)
      if (typeof merged === "string") throw new ExprError("NORN-FLOW-EXPRESSION", merged)
      return merged
    }
    case "Literal": {
      const schema = literalSchema(expr.value)
      if (!schema) throw new ExprError("NORN-FLOW-EXPRESSION", "literal is not a JSON scalar")
      return schema
    }
    case "Bound":
      return resolve(binders.get(expr.binder), "closure element", expr.path).schema
    case "Input":
      return resolve(scope.get(expr.name), expr.name, expr.path).schema
    case "Compare": {
      const left = infer(expr.left, scope, binders)
      const right = infer(expr.right, scope, binders)
      if (expr.op === "==" || expr.op === "!=") {
        const nullCheck = (left.type === "null" && Array.isArray(right.type)) || (right.type === "null" && Array.isArray(left.type))
        const comparable =
          nullCheck || (isScalar(left) && isScalar(right) && (left.type === right.type || left.type === "null" || right.type === "null" || (isNumeric(left) && isNumeric(right))))
        if (!comparable) {
          throw new ExprError("NORN-FLOW-EXPRESSION", `${expr.op} compares scalars of one type, not ${left.type} and ${right.type}`)
        }
      } else if (temporalKind(expr.left) !== undefined && temporalKind(expr.left) === temporalKind(expr.right)) {
        // Same-kind parsed temporal values order semantically (norn-zen/6).
      } else if (!isNumeric(left) || !isNumeric(right)) {
        throw new ExprError("NORN-FLOW-EXPRESSION", `${expr.op} is qualified for numbers only, not ${left.type} and ${right.type}`)
      }
      return boolean
    }
    case "Logical": {
      const left = infer(expr.left, scope, binders), right = infer(expr.right, scope, binders)
      if (left.type !== "boolean" || right.type !== "boolean") throw new ExprError("NORN-FLOW-EXPRESSION", `${expr.op} takes booleans, not ${left.type} and ${right.type}`)
      return boolean
    }
    case "Not": {
      const t = infer(expr.operand, scope, binders)
      if (t.type !== "boolean") throw new ExprError("NORN-FLOW-EXPRESSION", `not takes booleans, not ${t.type}`)
      return boolean
    }
    case "List": {
      if (expr.items.length === 0) throw new ExprError("NORN-FLOW-EXPRESSION", "an empty list has no item type; v1 lists are non-empty")
      let items = infer(expr.items[0]!, scope, binders)
      for (const item of expr.items.slice(1)) {
        const merged = mergeSchemas(items, infer(item, scope, binders))
        if (typeof merged === "string") throw new ExprError("NORN-FLOW-EXPRESSION", `list items disagree: ${merged}`)
        items = merged
      }
      return array(items)
    }
    case "Record": {
      const fields: Record<string, ValueSchema> = {}
      for (const [name, value] of expr.fields) {
        zenKey(name)
        if (name in fields) throw new ExprError("NORN-FLOW-EXPRESSION", `record key ${name} is repeated`)
        fields[name] = infer(value, scope, binders)
      }
      return recordSchema(fields)
    }
  }
}

/** ZEN text of `expr` in `scope`. Every compound operand is parenthesised, so precedence never matters. */
export function render(expr: Expr, scope: Scope): string {
  return renderIn(expr, scope, new Map())
}

function renderIn(expr: Expr, scope: Scope, binders: Binders): string {
  switch (expr._tag) {
    case "Binary": return `${operand(expr.left, scope, binders)} ${expr.op} ${operand(expr.right, scope, binders)}`
    case "Call": return expr.fn === "concat"
      ? `flatten([${expr.args.map((arg) => renderIn(arg, scope, binders)).join(", ")}])`
      : expr.fn === "dateBefore"
      ? `date(${renderIn(expr.args[0]!, scope, binders)}) < date(${renderIn(expr.args[1]!, scope, binders)})`
      : expr.fn === "timestampBefore"
      ? `${renderIn(expr.args[0]!, scope, binders)} < ${renderIn(expr.args[1]!, scope, binders)}`
      : expr.fn === "timestampAtOrBefore"
      ? `${renderIn(expr.args[0]!, scope, binders)} <= ${renderIn(expr.args[1]!, scope, binders)}`
      : `${expr.fn}(${expr.args.map((arg) => renderIn(arg, scope, binders)).join(", ")})`
    case "Closure": {
      const schema = closureElement(expr, scope, binders)
      const alias = freshAlias(scope, binders, schema)
      const inner = new Map(binders).set(expr.binder, { zen: alias, schema })
      return `${expr.fn}(${operand(expr.list, scope, binders)} as ${alias}, ${renderIn(expr.body, scope, inner)})`
    }
    case "Conditional": return `${operand(expr.condition, scope, binders)} ? ${operand(expr.yes, scope, binders)} : ${operand(expr.no, scope, binders)}`
    case "Coalesce": return `${operand(expr.value, scope, binders)} ?? ${operand(expr.fallback, scope, binders)}`
    case "Literal":
      return zenJson(expr.value)
    case "Bound":
      return resolve(binders.get(expr.binder), "closure element", expr.path).zen
    case "Input":
      return resolve(scope.get(expr.name), expr.name, expr.path).zen
    case "Compare":
    case "Logical":
      return `${operand(expr.left, scope, binders)} ${expr.op} ${operand(expr.right, scope, binders)}`
    case "Not":
      return `not ${operand(expr.operand, scope, binders)}`
    case "List":
      return `[${expr.items.map((item) => renderIn(item, scope, binders)).join(", ")}]`
    case "Record":
      return `{${expr.fields.map(([name, value]) => `${zenKey(name)}: ${renderIn(value, scope, binders)}`).join(", ")}}`
  }
}

function closureElement(expr: Extract<Expr, { _tag: "Closure" }>, scope: Scope, binders: Binders): ValueSchema {
  const list = infer(expr.list, scope, binders)
  if (list.type !== "array") throw new ExprError("NORN-FLOW-EXPRESSION", `${expr.fn} iterates a list, not ${String(list.type)}`)
  return list.items
}

function operand(expr: Expr, scope: Scope, binders: Binders): string {
  const text = renderIn(expr, scope, binders)
  // `dateBefore` / `timestampBefore` / `timestampAtOrBefore` are calls in the
  // AST but render as comparisons, so they are compound operands like any other (#1109).
  const atomic = ["Literal", "Input", "Bound", "List", "Record", "Call", "Closure"].includes(expr._tag)
    && !(expr._tag === "Call" && (expr.fn === "dateBefore" || expr.fn === "timestampBefore" || expr.fn === "timestampAtOrBefore"))
  return atomic ? text : `(${text})`
}

/** Check a contextual expression, retaining literal enum members and empty-list item types. */
export function assignmentError(expr: Expr, expected: ValueSchema, scope: Scope, path = "$"): string | undefined {
  if (expr._tag === "Conditional") {
    if (typeOf(expr.condition, scope).type !== "boolean") return `${path} condition must be boolean`
    return assignmentError(expr.yes, expected, scope, path) ?? assignmentError(expr.no, expected, scope, path)
  }
  if (expr._tag === "Coalesce") {
    const source = typeOf(expr.value, scope)
    const problem = source.type === "null" ? undefined : assignable(nonNull(source), expected, path)
    return problem ?? assignmentError(expr.fallback, expected, scope, path)
  }
  const target = nonNull(expected)
  if (expr._tag === "List" && target.type === "array") {
    for (const item of expr.items) {
      const problem = assignmentError(item, target.items, scope, `${path}[]`)
      if (problem) return problem
    }
    return undefined
  }
  if (expr._tag === "Record" && target.type === "object") {
    const keys = new Set<string>()
    for (const [key, value] of expr.fields) {
      zenKey(key)
      if (keys.has(key)) return `${path}.${key} is repeated`
      keys.add(key)
      if (!Object.hasOwn(target.properties, key)) return `${path}.${key} is not declared`
      const problem = assignmentError(value, target.properties[key]!, scope, `${path}.${key}`)
      if (problem) return problem
    }
    for (const key of target.required ?? []) if (!keys.has(key)) return `${path}.${key} is required`
    return undefined
  }
  const source = expr._tag === "Literal" && typeof expr.value === "string" ? text([expr.value]) : typeOf(expr, scope)
  return assignable(source, expected, path)
}
