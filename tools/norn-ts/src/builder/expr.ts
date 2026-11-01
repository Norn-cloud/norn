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
  record as recordSchema,
} from "./schema.ts"

export type CompareOp = "==" | "!=" | "<" | "<=" | ">" | ">="
export type LogicalOp = "and" | "or"

export type Expr =
  | { readonly _tag: "Binary"; readonly op: "+" | "-" | "*" | "/" | "%" | "in"; readonly left: Expr; readonly right: Expr }
  | { readonly _tag: "Call"; readonly fn: "lower" | "upper" | "trim" | "len" | "contains" | "startsWith" | "endsWith" | "dateBefore" | "date" | "timestamp"; readonly args: ReadonlyArray<Expr> }
  | { readonly _tag: "Conditional"; readonly condition: Expr; readonly yes: Expr; readonly no: Expr }
  | { readonly _tag: "Coalesce"; readonly value: Expr; readonly fallback: Expr }
  | { readonly _tag: "Unsafe"; readonly text: string; readonly schema: ValueSchema }
  | { readonly _tag: "Literal"; readonly value: string | number | boolean | null }
  | { readonly _tag: "Ref"; readonly name: string; readonly path: ReadonlyArray<string> }
  | { readonly _tag: "Compare"; readonly op: CompareOp; readonly left: Expr; readonly right: Expr }
  | { readonly _tag: "Logical"; readonly op: LogicalOp; readonly left: Expr; readonly right: Expr }
  | { readonly _tag: "Not"; readonly operand: Expr }
  | { readonly _tag: "List"; readonly items: ReadonlyArray<Expr> }
  | { readonly _tag: "Record"; readonly fields: ReadonlyArray<readonly [string, Expr]> }

export const binary = (op: "+" | "-" | "*" | "/" | "%" | "in", left: Expr, right: Expr): Expr => ({ _tag: "Binary", op, left, right })
export const call = (fn: Extract<Expr, { _tag: "Call" }>["fn"], ...args: Expr[]): Expr => ({ _tag: "Call", fn, args })
export const conditional = (condition: Expr, yes: Expr, no: Expr): Expr => ({ _tag: "Conditional", condition, yes, no })
export const coalesce = (value: Expr, fallback: Expr): Expr => ({ _tag: "Coalesce", value, fallback })
export const unsafeZen = (text: string, schema: ValueSchema): Expr => ({ _tag: "Unsafe", text, schema })

export const lit = (value: string | number | boolean | null): Expr => ({ _tag: "Literal", value })
/** A read of `name` (a binding in scope) and a declared property path below it. */
export const ref = (name: string, ...path: ReadonlyArray<string>): Expr => ({ _tag: "Ref", name, path: [...path] })
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

/** The ZEN text and schema of `name.path` in `scope`. */
function resolve(scope: Scope, name: string, path: ReadonlyArray<string>): { readonly zen: string; readonly schema: ValueSchema } {
  const binding = scope.get(name)
  if (!binding) {
    throw new ExprError("NORN-FLOW-SLICE", `\`${name}\` is not bound here (bindings: ${[...scope.keys()].join(", ") || "none"})`)
  }
  let schema = binding.schema
  for (const segment of path) {
    const member = schema.type === "object" ? schema.properties[segment] : undefined
    if (!member) {
      throw new ExprError("NORN-FLOW-SLICE", `expected a declared field, got ${segment} on ${name}; did you mean ${schema.type === "object" ? Object.keys(schema.properties).join(", ") : "a record field"}?`)
    }
    schema = member
  }
  if ("zen" in binding) return { zen: [binding.zen, ...path.map(zenKey)].filter(Boolean).join("."), schema }
  // An inline record: follow the path through its fields at build time.
  let expr = binding.inline
  for (const [index, segment] of path.entries()) {
    if (expr._tag === "Record") {
      const field = expr.fields.find(([key]) => key === segment)
      if (!field) throw new ExprError("NORN-FLOW-SLICE", `\`${[name, ...path].join(".")}\` reads ${segment}, which is not set`)
      expr = field[1]
    } else if (expr._tag === "Ref") {
      expr = { _tag: "Ref", name: expr.name, path: [...expr.path, ...path.slice(index)] }
      break
    } else {
      throw new ExprError("NORN-FLOW-SLICE", `\`${[name, ...path].join(".")}\` reads into a non-record value`)
    }
  }
  return { zen: operand(expr, binding.scope), schema }
}

/** The closed schema of `expr` in `scope`, or {@link ExprError}. */
export function typeOf(expr: Expr, scope: Scope): ValueSchema {
  switch (expr._tag) {
    case "Unsafe": return expr.schema
    case "Binary": {
      const l = typeOf(expr.left, scope), r = typeOf(expr.right, scope)
      if (expr.op === "in") {
        if (r.type !== "array" || typeof mergeSchemas(l, r.items) === "string") throw new ExprError("NORN-FLOW-EXPRESSION", "membership expects a value and an array of the same type")
        return boolean
      }
      if (expr.op === "+" && l.type === "string" && r.type === "string") return text()
      if (!isNumeric(l) || !isNumeric(r)) throw new ExprError("NORN-FLOW-EXPRESSION", `expected numbers${expr.op === "+" ? " or two strings" : ""}, got ${l.type} and ${r.type}`)
      return numberSchema
    }
    case "Call": {
      const args = expr.args.map((arg) => typeOf(arg, scope))
      if (expr.fn === "len") {
        if (args.length !== 1 || (args[0]?.type !== "string" && args[0]?.type !== "array")) throw new ExprError("NORN-FLOW-EXPRESSION", "length expects a string or array")
        return integer
      }
      const arity = ["lower", "upper", "trim", "date", "timestamp"].includes(expr.fn) ? 1 : 2
      if (args.length !== arity || args.some((a) => a.type !== "string")) throw new ExprError("NORN-FLOW-EXPRESSION", `${expr.fn} expects ${arity} string arguments`)
      return arity === 1 ? text() : boolean
    }
    case "Conditional": {
      if (typeOf(expr.condition, scope).type !== "boolean") throw new ExprError("NORN-FLOW-EXPRESSION", "conditional expects a boolean condition")
      // Empty arrays acquire the other branch's item type.
      if (expr.yes._tag === "List" && expr.yes.items.length === 0) return typeOf(expr.no, scope)
      if (expr.no._tag === "List" && expr.no.items.length === 0) return typeOf(expr.yes, scope)
      const merged = mergeSchemas(typeOf(expr.yes, scope), typeOf(expr.no, scope))
      if (typeof merged === "string") throw new ExprError("NORN-FLOW-EXPRESSION", merged)
      return merged
    }
    case "Coalesce": {
      const value = typeOf(expr.value, scope), fallback = typeOf(expr.fallback, scope)
      const nonnull = Array.isArray(value.type) ? { ...value, type: value.type[0] } as ValueSchema : value
      const merged = value.type === "null" ? fallback : mergeSchemas(nonnull, fallback)
      if (typeof merged === "string") throw new ExprError("NORN-FLOW-EXPRESSION", merged)
      return merged
    }
    case "Literal": {
      const schema = literalSchema(expr.value)
      if (!schema) throw new ExprError("NORN-FLOW-EXPRESSION", "literal is not a JSON scalar")
      return schema
    }
    case "Ref":
      return resolve(scope, expr.name, expr.path).schema
    case "Compare": {
      const left = typeOf(expr.left, scope)
      const right = typeOf(expr.right, scope)
      if (expr.op === "==" || expr.op === "!=") {
        const comparable =
          isScalar(left) && isScalar(right) && (left.type === right.type || left.type === "null" || right.type === "null" || (isNumeric(left) && isNumeric(right)))
        if (!comparable) {
          throw new ExprError("NORN-FLOW-EXPRESSION", `${expr.op} compares scalars of one type, not ${left.type} and ${right.type}`)
        }
      } else if (!isNumeric(left) || !isNumeric(right)) {
        throw new ExprError("NORN-FLOW-EXPRESSION", `${expr.op} is qualified for numbers only, not ${left.type} and ${right.type}`)
      }
      return boolean
    }
    case "Logical":
    case "Not": {
      const operands = expr._tag === "Not" ? [expr.operand] : [expr.left, expr.right]
      for (const operand of operands) {
        const t = typeOf(operand, scope)
        if (t.type !== "boolean") throw new ExprError("NORN-FLOW-EXPRESSION", `${expr._tag === "Not" ? "not" : expr.op} takes booleans, not ${t.type}`)
      }
      return boolean
    }
    case "List": {
      if (expr.items.length === 0) throw new ExprError("NORN-FLOW-EXPRESSION", "an empty list has no item type; v1 lists are non-empty")
      let items = typeOf(expr.items[0]!, scope)
      for (const item of expr.items.slice(1)) {
        const merged = mergeSchemas(items, typeOf(item, scope))
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
        fields[name] = typeOf(value, scope)
      }
      return recordSchema(fields)
    }
  }
}

/** ZEN text of `expr` in `scope`. Every compound operand is parenthesised, so precedence never matters. */
export function render(expr: Expr, scope: Scope): string {
  switch (expr._tag) {
    case "Unsafe": return expr.text
    case "Binary": return `${operand(expr.left, scope)} ${expr.op} ${operand(expr.right, scope)}`
    case "Call": return expr.fn === "dateBefore"
      ? `date(${render(expr.args[0]!, scope)}) < date(${render(expr.args[1]!, scope)})`
      : `${expr.fn}(${expr.args.map((arg) => render(arg, scope)).join(", ")})`
    case "Conditional": return `${operand(expr.condition, scope)} ? ${operand(expr.yes, scope)} : ${operand(expr.no, scope)}`
    case "Coalesce": return `${operand(expr.value, scope)} ?? ${operand(expr.fallback, scope)}`
    case "Literal":
      return zenJson(expr.value)
    case "Ref":
      return resolve(scope, expr.name, expr.path).zen
    case "Compare":
    case "Logical":
      return `${operand(expr.left, scope)} ${expr.op} ${operand(expr.right, scope)}`
    case "Not":
      return `not ${operand(expr.operand, scope)}`
    case "List":
      return `[${expr.items.map((item) => render(item, scope)).join(", ")}]`
    case "Record":
      return `{${expr.fields.map(([name, value]) => `${zenKey(name)}: ${render(value, scope)}`).join(", ")}}`
  }
}

function operand(expr: Expr, scope: Scope): string {
  const text = render(expr, scope)
  return ["Literal", "Ref", "List", "Record", "Call"].includes(expr._tag) ? text : `(${text})`
}
