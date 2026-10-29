/** Structural Function I/O types. Effect Schema declarations are lowered separately. */
import ts from "typescript"
import * as S from "../builder/schema.ts"
import { BudgetExceeded, ExpansionBudget } from "../builder/budget.ts"

export class CompileError extends Error {
  constructor(readonly code: string, message: string, readonly node: ts.Node) { super(message) }
}

export class SchemaBudget {
  readonly memo = new Map<ts.Type, S.ValueSchema>()
  private readonly heights = new WeakMap<S.ValueSchema, number>()
  constructor(readonly expansion = new ExpansionBudget()) {}
  height(schema: S.ValueSchema): number {
    const known = this.heights.get(schema)
    if (known !== undefined) return known
    const children = schema.type === "object" ? Object.values(schema.properties) : schema.type === "array" ? [schema.items] : []
    let height = 0
    for (const child of children) height = Math.max(height, 1 + this.height(child))
    this.heights.set(schema, height)
    return height
  }
}

export function valueSchema(checker: ts.TypeChecker, type: ts.Type, node: ts.Node, active = new Set<ts.Type>(), budget = new SchemaBudget()): S.ValueSchema {
  try {
    budget.expansion.check()
    if (active.has(type)) throw new CompileError("NORN-FLOW-UNBOUNDED", "recursive Function schema", node)
    const known = budget.memo.get(type)
    if (known) {
      if (active.size + budget.height(known) >= 32) throw new CompileError("NORN-FLOW-UNBOUNDED", "Function schema exceeds nesting bound", node)
      return known
    }
    const result = buildValueSchema(checker, type, node, active, budget)
    budget.expansion.assert(result)
    if (active.size + budget.height(result) >= 32) throw new CompileError("NORN-FLOW-UNBOUNDED", "Function schema exceeds nesting bound", node)
    budget.memo.set(type, result)
    return result
  } catch (error) {
    if (error instanceof BudgetExceeded) throw new CompileError("NORN-FLOW-UNBOUNDED", error.message, node)
    throw error
  }
}

function buildValueSchema(checker: ts.TypeChecker, type: ts.Type, node: ts.Node, active: Set<ts.Type>, budget: SchemaBudget): S.ValueSchema {
  if (active.has(type) || active.size >= 32) throw new CompileError("NORN-FLOW-UNBOUNDED", "recursive or deeply nested Function types are not closed schemas", node)
  const next = new Set(active).add(type)
  if (type.isUnion()) {
    if (type.types.every((t) => (t.flags & ts.TypeFlags.StringLiteral) !== 0)) {
      return S.text(type.types.map((t) => (t as ts.StringLiteralType).value))
    }
    if (type.types.every((t) => (t.flags & ts.TypeFlags.BooleanLike) !== 0)) return S.boolean
    throw new CompileError("NORN-FLOW-VALUE", "Function schemas do not admit general unions", node)
  }
  if (type.flags & ts.TypeFlags.StringLike) return type.isStringLiteral() ? S.text([type.value]) : S.text()
  if (type.flags & ts.TypeFlags.NumberLike) return S.number
  if (type.flags & ts.TypeFlags.BooleanLike) return S.boolean
  if (type.flags & ts.TypeFlags.Null) return S.nullValue
  if (checker.isArrayType(type) || checker.isTupleType(type)) {
    const args = checker.getTypeArguments(type as ts.TypeReference)
    if (args.length === 0) throw new CompileError("NORN-FLOW-VALUE", "an array needs a closed item type", node)
    let items = valueSchema(checker, args[0]!, node, next, budget)
    for (const arg of args.slice(1)) {
      const merged = S.mergeSchemas(items, valueSchema(checker, arg, node, next, budget))
      if (typeof merged === "string") throw new CompileError("NORN-FLOW-VALUE", merged, node)
      items = merged
      budget.expansion.assert(items)
    }
    return S.array(items)
  }
  if (!(type.flags & ts.TypeFlags.Object) || checker.getSignaturesOfType(type, ts.SignatureKind.Call).length > 0 || checker.getIndexInfosOfType(type).length > 0) {
    throw new CompileError("NORN-FLOW-VALUE", `type ${checker.typeToString(type)} is outside the closed Function schema subset`, node)
  }
  const fields: Record<string, S.ValueSchema> = Object.create(null)
  const optional: string[] = []
  for (const prop of checker.getPropertiesOfType(type)) {
    const declaration = prop.valueDeclaration ?? prop.declarations?.[0] ?? node
    let member = checker.getTypeOfSymbolAtLocation(prop, declaration)
    if (prop.flags & ts.SymbolFlags.Optional) {
      optional.push(prop.name)
      if (member.isUnion()) {
        const present = member.types.filter((t) => !(t.flags & ts.TypeFlags.Undefined))
        if (present.length !== 1) throw new CompileError("NORN-FLOW-VALUE", "optional fields must have one closed type", declaration)
        member = present[0]!
      }
    }
    fields[prop.name] = valueSchema(checker, member, declaration, next, budget)
  }
  return S.record(fields, optional)
}


/** Callback result branches join through the same closed schema merge as lowerFlow. */
export function resultSchema(checker: ts.TypeChecker, type: ts.Type, node: ts.Node, budget = new SchemaBudget()): S.ValueSchema {
  if (!type.isUnion()) return valueSchema(checker, type, node, new Set(), budget)
  const branches = type.types.map((branch) => valueSchema(checker, branch, node, new Set(), budget))
  let schema = branches[0]!
  for (const branch of branches.slice(1)) {
    const merged = S.mergeSchemas(schema, branch)
    if (typeof merged === "string") throw new CompileError("NORN-FLOW-VALUE", `callback results have no closed shared shape: ${merged}`, node)
    schema = merged
    try { budget.expansion.assert(schema) } catch (error) {
      if (error instanceof BudgetExceeded) throw new CompileError("NORN-FLOW-UNBOUNDED", error.message, node)
      throw error
    }
  }
  return schema
}
