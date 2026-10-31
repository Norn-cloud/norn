/**
 * Effect Schema → EntityIR v2 / closed schema projection (T7-EFX-4, #2787).
 *
 * Ruling 2026-09-27 authoring-effect-profile §2: Effect Schema is surface
 * syntax, never a second type authority, and never executed. This module
 * therefore reads Schema *syntax* — calls and values recognised by resolved
 * profile symbol — and never imports a Schema value or walks a runtime
 * `SchemaAST`/`SchemaRepresentation` (that would require evaluating the
 * author's module). The result is plain builder data:
 *
 * - {@link SchemaProjection.properties}: an EntityIR v2 property list
 *   (`Norn.type` / `Norn.interfaceType`);
 * - {@link SchemaProjection.closed}: the checker's closed value schema
 *   (`type`/`properties`/`required`/`additionalProperties: false`, string
 *   `enum`), the schema language of Function I/O, action input/output, and
 *   RuleIR contracts.
 *
 * A schema feature without an exact projection into the target is
 * `NORN-FLOW-SCHEMA-PROJECTION`, never approximated. The Rust checker
 * validates every emitted artifact. The projection table is
 * `knowledge/architecture/build-plan-2026-09-09/norn-effect-profile-v1.md` §9.
 */
import ts from "typescript"
import type { ProfileChecker } from "../check.ts"
import * as E from "../builder/entity.ts"
import * as S from "../builder/schema.ts"
import { CompileError } from "./types.ts"

/** The structural reading of a Schema expression. Every node keeps the syntax it came from for diagnostics. */
export type SchemaNode =
  | { readonly kind: "string" | "int" | "finite" | "boolean" | "null"; readonly node: ts.Node }
  | { readonly kind: "literals"; readonly values: ReadonlyArray<string>; readonly node: ts.Node }
  | { readonly kind: "struct"; readonly fields: ReadonlyArray<StructField>; readonly node: ts.Node }
  | { readonly kind: "array"; readonly items: SchemaNode; readonly node: ts.Node }
  | { readonly kind: "union"; readonly members: ReadonlyArray<SchemaNode>; readonly node: ts.Node }
  | { readonly kind: "value-type"; readonly type: E.PropertyType; readonly node: ts.Node }

export interface StructField {
  readonly name: string
  readonly schema: SchemaNode
  /** Declared with `Schema.optionalKey`: the member may be absent. */
  readonly optional: boolean
  readonly node: ts.Node
}

export interface PropertyProjection {
  readonly type: E.PropertyType
  readonly nullable: boolean
}

/** Nesting bound for schema syntax (shared with the structural Function schemas). */
const MAX_SCHEMA_DEPTH = 32
/** `PropertyType::Decimal` scale bound (ZEN Expression's exact decimal range). */
const MAX_DECIMAL_SCALE = 28

const projection = (message: string, node: ts.Node): CompileError => new CompileError("NORN-FLOW-SCHEMA-PROJECTION", message, node)

export class SchemaProjection {
  private readonly checker: ts.TypeChecker
  private readonly memo = new Map<ts.Node, SchemaNode>()
  private readonly active = new Set<ts.Node>()
  private readonly sizes = new WeakMap<SchemaNode, number>()
  private readonly flattened = new WeakMap<SchemaNode, ReadonlyArray<SchemaNode>>()

  constructor(
    private readonly profile: ProfileChecker,
    private readonly program: ts.Program,
    /** Cooperative time/size guard; throws a CompileError when a compiler bound is exceeded. */
    private readonly guard: (node: ts.Node) => void = () => {},
    /**
     * Bound on the expanded (tree) size of a schema. Reading shares nodes, so a
     * small source can denote an exponentially large tree; projection
     * materializes that tree, so it is measured on the shared DAG first.
     */
    private readonly maxExpanded = 100_000,
    /**
     * Reserve expanded size against the batch's shared budget before a
     * projection materializes it, so many projections of one shared schema
     * are bounded together, not one at a time.
     */
    private readonly reserve: (work: number) => void = () => {},
  ) {
    this.checker = program.getTypeChecker()
  }

  // ------------------------------------------------------------ reading

  /** Read a Schema expression structurally. */
  read(expression: ts.Expression): SchemaNode {
    return this.readAt(expression, 0)
  }

  private identity(node: ts.Node): string | undefined {
    return this.profile.identity(this.program, node)
  }

  private unwrap(e: ts.Expression): ts.Expression {
    while (ts.isParenthesizedExpression(e) || ts.isSatisfiesExpression(e) || (ts.isAsExpression(e) && ts.isConstTypeReference(e.type))) e = e.expression
    return e
  }

  private readAt(input: ts.Expression, depth: number): SchemaNode {
    this.guard(input)
    if (depth > MAX_SCHEMA_DEPTH) throw new CompileError("NORN-FLOW-UNBOUNDED", "schema nesting exceeds the compiler bound", input)
    const e = this.unwrap(input)
    const known = this.memo.get(e)
    if (known) return known
    if (this.active.has(e)) throw projection("recursive schemas (suspend/lazy) have no EntityIR projection", e)
    this.active.add(e)
    try {
      const result = ts.isCallExpression(e) ? this.call(e, depth) : this.reference(e, depth)
      this.memo.set(e, result)
      return result
    } finally {
      this.active.delete(e)
    }
  }

  /** A Schema value (`Schema.String`) or a reference to a top-level schema definition. */
  private reference(e: ts.Expression, depth: number): SchemaNode {
    if (!ts.isIdentifier(e) && !ts.isPropertyAccessExpression(e)) {
      throw projection(`${ts.SyntaxKind[e.kind]} is not a schema in the structural subset`, e)
    }
    const id = this.identity(ts.isPropertyAccessExpression(e) ? e.name : e)
    switch (id) {
      case "effect/Schema#String":
        return { kind: "string", node: e }
      case "effect/Schema#Int":
        return { kind: "int", node: e }
      case "effect/Schema#Finite":
        return { kind: "finite", node: e }
      case "effect/Schema#Boolean":
        return { kind: "boolean", node: e }
      case "effect/Schema#Null":
        return { kind: "null", node: e }
      case undefined:
        break
      default:
        throw projection(`${id} has no EntityIR projection`, e)
    }
    const symbol = this.symbol(ts.isPropertyAccessExpression(e) ? e.name : e)
    const declaration = symbol?.valueDeclaration ?? symbol?.declarations?.[0]
    if (declaration && ts.isVariableDeclaration(declaration) && declaration.initializer && (declaration.parent.flags & ts.NodeFlags.Const) !== 0) {
      return this.readAt(declaration.initializer, depth + 1)
    }
    if (declaration && ts.isClassDeclaration(declaration)) {
      const heritage = declaration.heritageClauses?.[0]?.types[0]?.expression
      if (heritage && ts.isCallExpression(heritage) && ts.isCallExpression(heritage.expression) && this.identity(this.calleeName(heritage.expression)) === "effect/Schema#Class") {
        const fields = heritage.arguments[0]
        if (!fields) throw projection("Schema.Class needs its field struct", heritage)
        return { kind: "struct", fields: this.fields(fields, depth), node: e }
      }
    }
    throw new CompileError("NORN-FLOW-VALUE", `${e.getText()} is not a schema definition (a top-level const Schema value or a Schema.Class)`, e)
  }

  private symbol(node: ts.Node): ts.Symbol | undefined {
    // `{ field }` names the member; the value it reads is the binding `field`.
    const s = ts.isShorthandPropertyAssignment(node.parent) && node.parent.name === node
      ? this.checker.getShorthandAssignmentValueSymbol(node.parent)
      : this.checker.getSymbolAtLocation(node)
    return s && s.flags & ts.SymbolFlags.Alias ? this.checker.getAliasedSymbol(s) : s
  }

  private calleeName(call: ts.CallExpression): ts.Node {
    return ts.isPropertyAccessExpression(call.expression) ? call.expression.name : call.expression
  }

  private call(call: ts.CallExpression, depth: number): SchemaNode {
    const id = this.identity(this.calleeName(call))
    const a = call.arguments
    const arg = (i: number): ts.Expression => {
      const x = a[i]
      if (!x) throw projection(`${id} is missing an argument`, call)
      return x
    }
    switch (id) {
      case "effect/Schema#Struct":
        return { kind: "struct", fields: this.fields(arg(0), depth), node: call }
      case "effect/Schema#TaggedStruct": {
        const tag = this.unwrap(arg(0))
        if (!ts.isStringLiteral(tag)) throw projection("TaggedStruct tags project only as string literals", tag)
        const fields = this.fields(arg(1), depth)
        if (fields.some((f) => f.name === "_tag")) throw projection("TaggedStruct fields cannot redeclare _tag", call)
        return { kind: "struct", fields: [{ name: "_tag", schema: { kind: "literals", values: [tag.text], node: tag }, optional: false, node: tag }, ...fields], node: call }
      }
      case "effect/Schema#Literal":
        return { kind: "literals", values: [this.literal(arg(0))], node: call }
      case "effect/Schema#Literals": {
        const list = this.unwrap(arg(0))
        if (!ts.isArrayLiteralExpression(list) || list.elements.length === 0) throw projection("Schema.Literals takes a non-empty literal array", list)
        return { kind: "literals", values: list.elements.map((x) => this.literal(x)), node: call }
      }
      case "effect/Schema#NullOr":
        return { kind: "union", members: [this.readAt(arg(0), depth + 1), { kind: "null", node: call }], node: call }
      case "effect/Schema#Union": {
        const list = this.unwrap(arg(0))
        if (!ts.isArrayLiteralExpression(list) || list.elements.length === 0) throw projection("Schema.Union takes a non-empty literal array of members", list)
        if (a.length > 1) throw projection("Schema.Union options have no EntityIR projection", a[1]!)
        return { kind: "union", members: list.elements.map((x) => this.readAt(x, depth + 1)), node: call }
      }
      case "effect/Schema#ArraySchema":
        return { kind: "array", items: this.readAt(arg(0), depth + 1), node: call }
      case "effect/Schema#TaggedUnion":
        throw projection("discriminated unions have no qualified EntityIR or closed-schema projection in v1", call)
      case "effect/Schema#optionalKey":
        throw projection("Schema.optionalKey marks a struct member; use it only as a Schema.Struct field", call)
      case "effect/Pipeable#Pipeable.pipe":
      case "effect/Function#pipe": {
        const base = id === "effect/Function#pipe" ? arg(0) : (call.expression as ts.PropertyAccessExpression).expression
        const operators = id === "effect/Function#pipe" ? a.slice(1) : [...a]
        for (const op of operators) {
          const o = this.unwrap(op)
          // A brand is nominal: it constrains authoring types and leaves the wire value unchanged.
          if (!ts.isCallExpression(o) || this.identity(this.calleeName(o)) !== "effect/Schema#brand") {
            throw projection("only Schema.brand may be piped onto a schema (checks and transforms have no projection)", o)
          }
          const name = o.arguments[0] && this.unwrap(o.arguments[0])
          if (!name || !ts.isStringLiteral(name)) throw projection("Schema.brand takes a string literal", o)
        }
        return this.readAt(base, depth + 1)
      }
      case "@norn/authoring#Norn.decimal": {
        const scale = this.unwrap(arg(0))
        const value = ts.isNumericLiteral(scale) && /^(0|[1-9][0-9]*)$/.test(scale.getText()) ? Number(scale.getText()) : Number.NaN
        if (!Number.isSafeInteger(value) || value > MAX_DECIMAL_SCALE) {
          throw new CompileError("NORN-FLOW-LITERAL", `Norn.decimal takes an integer literal scale between 0 and ${MAX_DECIMAL_SCALE}`, scale)
        }
        const options = a[1] ? this.objectLiteral(a[1]) : new Map<string, ts.Expression>()
        for (const key of options.keys()) if (key !== "unit" && key !== "currency") throw new CompileError("NORN-FLOW-LITERAL", `unknown Norn.decimal option ${key}`, a[1]!)
        const unitNode = options.get("unit")
        const currencyNode = options.get("currency")
        if (currencyNode !== undefined) {
          if (unitNode !== undefined) throw new CompileError("NORN-FLOW-LITERAL", "a monetary decimal declares a currency, not a unit", a[1]!)
          return { kind: "value-type", type: E.money(value, this.stringLiteral(currencyNode, a[1]!, "the currency property")), node: call }
        }
        if (unitNode === undefined) return { kind: "value-type", type: E.decimal(value), node: call }
        const unit = this.objectLiteral(unitNode)
        const system = this.stringLiteral(unit.get("system"), unitNode, "unit.system")
        if (system !== "ucum" && system !== "qudt") throw new CompileError("NORN-FLOW-LITERAL", "unit.system is \"ucum\" or \"qudt\"", unitNode)
        if (unit.size !== 2) throw new CompileError("NORN-FLOW-LITERAL", "a unit is exactly { system, code }", unitNode)
        return { kind: "value-type", type: E.decimal(value, { system, code: this.stringLiteral(unit.get("code"), unitNode, "unit.code") }), node: call }
      }
      case "@norn/authoring#Norn.ref":
        return { kind: "value-type", type: E.ref(this.stringLiteral(a[0], call, "the property type name")), node: call }
      case "@norn/authoring#Norn.timestamp": {
        const timezone = this.stringLiteral(a[0], call, "the timezone policy")
        if (timezone !== "utc" && timezone !== "offset") throw new CompileError("NORN-FLOW-LITERAL", "the timezone policy is \"utc\" or \"offset\"", call)
        return { kind: "value-type", type: E.timestamp(timezone), node: call }
      }
      case "@norn/authoring#Norn.date":
        return { kind: "value-type", type: E.date, node: call }
      case "@norn/authoring#Norn.json":
        return { kind: "value-type", type: E.json, node: call }
      default:
        throw projection(`${id ?? call.expression.getText()} is not a schema in the structural subset`, call)
    }
  }

  private fields(input: ts.Expression, depth: number): StructField[] {
    const fields: StructField[] = []
    for (const [name, value, node] of this.entries(input)) {
      const v = this.unwrap(value)
      if (ts.isCallExpression(v) && this.identity(this.calleeName(v)) === "effect/Schema#optionalKey") {
        const inner = v.arguments[0]
        if (!inner) throw projection("Schema.optionalKey needs a schema", v)
        fields.push({ name, schema: this.readAt(inner, depth + 1), optional: true, node })
      } else {
        fields.push({ name, schema: this.readAt(v, depth + 1), optional: false, node })
      }
    }
    return fields
  }

  /** Ordered `[name, value, member]` entries of an object literal (shorthand members read their binding). */
  private entries(input: ts.Expression): Array<readonly [string, ts.Expression, ts.Node]> {
    const o = this.unwrap(input)
    if (!ts.isObjectLiteralExpression(o)) throw projection("struct fields must be an inline object literal", o)
    const out: Array<readonly [string, ts.Expression, ts.Node]> = []
    for (const p of o.properties) {
      if (ts.isPropertyAssignment(p) && (ts.isIdentifier(p.name) || ts.isStringLiteral(p.name))) out.push([p.name.text, p.initializer, p])
      else if (ts.isShorthandPropertyAssignment(p)) out.push([p.name.text, p.name, p])
      else throw projection("struct members are plain name: schema entries", p)
      if (out[out.length - 1]![0] === "__proto__") throw projection("__proto__ is not a portable member name", p)
    }
    return out
  }

  private objectLiteral(input: ts.Expression): Map<string, ts.Expression> {
    const o = this.unwrap(input)
    if (!ts.isObjectLiteralExpression(o)) throw new CompileError("NORN-FLOW-LITERAL", "expected an inline object literal", o)
    const out = new Map<string, ts.Expression>()
    for (const p of o.properties) {
      if (!ts.isPropertyAssignment(p) || !(ts.isIdentifier(p.name) || ts.isStringLiteral(p.name))) throw new CompileError("NORN-FLOW-LITERAL", "expected plain name: value members", p)
      out.set(p.name.text, p.initializer)
    }
    return out
  }

  private stringLiteral(input: ts.Expression | undefined, at: ts.Node, what: string): string {
    const e = input && this.unwrap(input)
    if (!e || !(ts.isStringLiteral(e) || ts.isNoSubstitutionTemplateLiteral(e))) throw new CompileError("NORN-FLOW-LITERAL", `${what} must be a string literal`, e ?? at)
    return e.text
  }

  private literal(input: ts.Expression): string {
    const e = this.unwrap(input)
    if (ts.isStringLiteral(e) || ts.isNoSubstitutionTemplateLiteral(e)) return e.text
    throw projection("only string literals project (as enum values); numeric, boolean, and bigint literals have no exact projection", e)
  }

  // --------------------------------------------------------- projections

  /** The expanded tree size of a schema, saturated past the bound; memoized on the shared DAG. */
  private expandedSize(schema: SchemaNode): number {
    const known = this.sizes.get(schema)
    if (known !== undefined) return known
    this.guard(schema.node)
    let size = 1
    const add = (n: number): void => { size = Math.min(size + n, this.maxExpanded + 1) }
    switch (schema.kind) {
      case "literals":
        add(schema.values.length)
        break
      case "struct":
        for (const f of schema.fields) add(this.expandedSize(f.schema))
        break
      case "array":
        add(this.expandedSize(schema.items))
        break
      case "union":
        for (const m of schema.members) add(this.expandedSize(m))
        break
      default:
        break
    }
    this.sizes.set(schema, size)
    return size
  }

  /**
   * Refuse a schema whose expanded tree exceeds the bound, and reserve its
   * size against the shared budget, before anything is materialized.
   */
  private bounded(schema: SchemaNode): void {
    const size = this.expandedSize(schema)
    if (size > this.maxExpanded) {
      throw new CompileError("NORN-FLOW-UNBOUNDED", "schema expands beyond the compiler size bound (shared definitions are counted at every use)", schema.node)
    }
    this.reserve(size)
  }

  /** EntityIR v2 properties of a struct schema, in declaration order. */
  properties(schema: SchemaNode): Array<{ readonly id: string; readonly projection: PropertyProjection; readonly node: ts.Node }> {
    this.bounded(schema)
    if (schema.kind !== "struct") throw projection("a type's properties are a Schema.Struct (or a Schema.Class)", schema.node)
    return schema.fields.map((f) => {
      if (f.optional) {
        throw projection("EntityIR v2 has one absence policy, nullable: write Schema.NullOr(...) instead of Schema.optionalKey(...)", f.node)
      }
      return { id: f.name, projection: this.property(f.schema), node: f.node }
    })
  }

  /** One EntityIR v2 property type and its null policy. */
  property(schema: SchemaNode): PropertyProjection {
    switch (schema.kind) {
      case "string":
        return { type: E.text(), nullable: false }
      case "int":
        return { type: E.integer, nullable: false }
      case "boolean":
        return { type: E.boolean, nullable: false }
      case "value-type":
        return { type: schema.type, nullable: false }
      case "literals":
        return { type: E.enumeration(...schema.values), nullable: false }
      case "finite":
        throw projection("EntityIR has no binary floating-point kind: use Schema.Int or Norn.decimal(scale)", schema.node)
      case "null":
        throw projection("a property cannot hold only null", schema.node)
      case "struct":
      case "array":
        throw projection("EntityIR v2 properties are scalar: model a nested record as its own type with a link, and a list as a to-many link", schema.node)
      case "union": {
        const members = this.flatten(schema)
        const present = members.filter((m) => m.kind !== "null")
        const nullable = present.length < members.length
        if (present.length === 0) throw projection("a property cannot hold only null", schema.node)
        if (present.every((m) => m.kind === "literals")) {
          const values = present.flatMap((m) => (m.kind === "literals" ? m.values : []))
          return { type: E.enumeration(...new Set(values)), nullable }
        }
        if (present.length === 1) {
          const inner = this.property(present[0]!)
          return { type: inner.type, nullable: nullable || inner.nullable }
        }
        throw projection("only literal unions (enum) and Schema.NullOr project to EntityIR properties", schema.node)
      }
    }
  }

  /** The closed value schema (Function I/O, action input/output, RuleIR contracts). */
  closed(schema: SchemaNode, nullable = false): S.ValueSchema {
    this.bounded(schema)
    return this.closedAt(schema, nullable)
  }

  /** Wire types for syntax-only expressions; Rust derives semantic types from the real context. */
  expressionInput(schema: SchemaNode): S.ValueSchema {
    this.bounded(schema)
    return this.closedAt(schema, true, true)
  }

  private closedAt(schema: SchemaNode, nullable: boolean, temporalWire = false): S.ValueSchema {
    this.guard(schema.node)
    switch (schema.kind) {
      case "string":
        return S.text()
      case "int":
        return S.integer
      case "finite":
        return S.number
      case "boolean":
        return S.boolean
      case "null":
        return S.nullValue
      case "literals":
        return S.text(schema.values)
      case "array":
        return S.array(this.closedAt(schema.items, nullable, temporalWire))
      case "struct": {
        const fields: Record<string, S.ValueSchema> = Object.create(null) as Record<string, S.ValueSchema>
        const optional: string[] = []
        for (const f of schema.fields) {
          if (f.name in fields) throw projection(`duplicate member ${f.name}`, f.node)
          fields[f.name] = this.closedAt(f.schema, nullable, temporalWire)
          if (f.optional) optional.push(f.name)
        }
        return S.record(fields, optional)
      }
      case "union": {
        const members = this.flatten(schema)
        if (nullable && members.length === 2 && members.some((m) => m.kind === "null")) {
          const nonNull = members.find((m) => m.kind !== "null")!
          return S.nullable(this.closedAt(nonNull, nullable, temporalWire))
        }
        if (members.every((m) => m.kind === "literals")) return S.text([...new Set(members.flatMap((m) => (m.kind === "literals" ? m.values : [])))])
        throw projection("closed schemas have no unions: use a literal union, or Schema.optionalKey for an absent member", schema.node)
      }
      case "value-type":
        if (temporalWire && (schema.type.kind === "timestamp" || schema.type.kind === "date")) return S.text()
        throw projection(`Norn.${schema.type.kind === "ref" ? "ref" : schema.type.kind} projects to EntityIR properties only; closed schemas have no ${schema.type.kind} type yet`, schema.node)
    }
  }

  private flatten(schema: SchemaNode): ReadonlyArray<SchemaNode> {
    if (schema.kind !== "union") return [schema]
    const known = this.flattened.get(schema)
    if (known) return known
    const members = schema.members.flatMap((m) => this.flatten(m))
    this.flattened.set(schema, members)
    return members
  }
}
