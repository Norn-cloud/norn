/** Preflight shared data as its expanded tree before schema, text, or JSON materialization. */
import type { Expr, Scope } from "./expr.ts"

export interface ExpansionLimits {
  readonly maxWork: number
  readonly maxOutputBytes: number
  readonly deadline?: number
}

export class BudgetExceeded extends Error {}
interface Cost { readonly work: number; readonly bytes: number; readonly lines: number; readonly height: number }
const utf8 = (value: string): number => new TextEncoder().encode(value).length

export class ExpansionBudget {
  private readonly memo = new WeakMap<object, Cost>()
  private readonly active = new Set<object>()
  private readonly expressions = new WeakMap<Scope, WeakMap<Expr, Cost>>()
  private readonly activeExpressions = new WeakMap<Scope, Set<Expr>>()
  private work = 0
  private output = 0
  constructor(readonly limits: ExpansionLimits = { maxWork: 100_000, maxOutputBytes: 8_388_608 }) {}

  check(): void {
    if (this.limits.deadline !== undefined && Date.now() >= this.limits.deadline) throw new BudgetExceeded("cooperative expansion time bound exceeded")
  }
  private capWork(value: number): number { return Math.min(value, this.limits.maxWork + 1) }
  private capBytes(value: number): number { return Math.min(value, this.limits.maxOutputBytes + 1) }
  private charge(cost: Cost): void {
    this.check()
    this.work = this.capWork(this.work + cost.work)
    if (this.work > this.limits.maxWork || cost.bytes > this.limits.maxOutputBytes - this.output) throw new BudgetExceeded("expanded data exceeds compiler work or output byte bound")
  }

  /** Memoization counts every DAG edge, with saturated costs; it does not collapse emitted occurrences. */
  private cost(value: unknown, depth = 0): Cost {
    this.check()
    if (depth > 128) throw new BudgetExceeded("expanded data exceeds nesting bound")
    if (value === null || typeof value !== "object") {
      const text = JSON.stringify(value)
      return { work: 1, bytes: text === undefined ? 0 : utf8(text), lines: 0, height: 0 }
    }
    if (this.active.has(value)) throw new BudgetExceeded("cyclic expanded data")
    const known = this.memo.get(value)
    if (known) {
      if (depth + known.height > 128) throw new BudgetExceeded("expanded data exceeds nesting bound")
      return known
    }
    this.active.add(value)
    try {
      const entries: Array<readonly [string | undefined, unknown]> = Array.isArray(value)
        ? value.map((child) => [undefined, child] as const)
        : Object.entries(value).filter(([, child]) => child !== undefined)
      let work = 1, bytes = entries.length ? 3 : 2, lines = entries.length ? 1 : 0, height = 0
      for (let i = 0; i < entries.length; i++) {
        const [name, child] = entries[i]!, cost = this.cost(child, depth + 1)
        work = this.capWork(work + cost.work)
        bytes = this.capBytes(bytes + cost.bytes + 2 * (cost.lines + 1) + 1 + (i + 1 < entries.length ? 1 : 0) + (name === undefined ? 0 : utf8(JSON.stringify(name)) + 2))
        lines = this.capBytes(lines + cost.lines + 1)
        height = Math.max(height, cost.height + 1)
      }
      if (depth + height > 128) throw new BudgetExceeded("expanded data exceeds nesting bound")
      const result = { work, bytes, lines, height }; this.memo.set(value, result); return result
    } finally { this.active.delete(value) }
  }

  assert(value: unknown): void { this.charge(this.cost(value)) }
  /** Reserve `work` units for data about to be materialized (measured on a shared DAG by the caller). */
  reserve(work: number): void { this.charge({ work: this.capWork(work), bytes: 0, lines: 0, height: 0 }) }
  /** Track all generated row data before joining the final graph and artifact envelope. */
  addOutput(value: unknown): void {
    const cost = this.cost(value)
    this.charge(cost)
    this.output = this.capBytes(this.output + cost.bytes + 1)
    if (this.output > this.limits.maxOutputBytes) throw new BudgetExceeded("accumulated rows exceed compiler output byte bound")
  }

  /** Ref expansion follows immutable inline scopes, including Compose return values. */
  assertExpression(expr: Expr, scope: Scope): void {
    const cost = this.expressionCost(expr, scope)
    this.charge(cost)
    // Reserve before typeOf too: resolving an inline Ref currently renders its value.
    // Repeated uses reserve repeated bytes, even when their immutable node is memoized.
    this.output = this.capBytes(this.output + cost.bytes)
  }
  private expressionCost(expr: Expr, scope: Scope, depth = 0): Cost {
    this.check()
    if (depth > 128) throw new BudgetExceeded("inline expressions exceed expansion nesting bound")
    let memo = this.expressions.get(scope)
    if (!memo) { memo = new WeakMap(); this.expressions.set(scope, memo) }
    let active = this.activeExpressions.get(scope)
    if (!active) { active = new Set(); this.activeExpressions.set(scope, active) }
    if (active.has(expr)) throw new BudgetExceeded("cyclic inline expression")
    const known = memo.get(expr)
    if (known) {
      if (depth + known.height > 128) throw new BudgetExceeded("inline expressions exceed expansion nesting bound")
      return known
    }
    active.add(expr)
    try {
      let work = 1, bytes = 64, height = 0
      const child = (value: Expr, where = scope): void => {
        const cost = this.expressionCost(value, where, depth + 1)
        work = this.capWork(work + cost.work); bytes = this.capBytes(bytes + cost.bytes)
        height = Math.max(height, cost.height + 1)
      }
      switch (expr._tag) {
        case "Literal": bytes = this.capBytes(bytes + utf8(JSON.stringify(expr.value))); break
        case "Ref": {
          const binding = scope.get(expr.name)
          if (!binding) break // Existing type checker supplies the slice diagnostic.
          if ("zen" in binding) { bytes = this.capBytes(bytes + utf8(binding.zen) + expr.path.reduce((n, part) => n + utf8(part) + 1, 0)); break }
          let value = binding.inline
          for (const [index, segment] of expr.path.entries()) {
            if (value._tag === "Record") {
              const field = value.fields.find(([name]) => name === segment)
              if (!field) break
              value = field[1]
            } else if (value._tag === "Ref") {
              value = { _tag: "Ref", name: value.name, path: [...value.path, ...expr.path.slice(index)] }; break
            } else break
          }
          child(value, binding.scope)
          break
        }
        case "Binary": case "Compare": case "Logical": child(expr.left); child(expr.right); break
        case "Unsafe": bytes = this.capBytes(bytes + utf8(expr.text)); break
        case "Call": for (const arg of expr.args) child(arg); break
        case "Conditional": child(expr.condition); child(expr.yes); child(expr.no); break
        case "Coalesce": child(expr.value); child(expr.fallback); break
        case "Not": child(expr.operand); break
        case "List": for (const item of expr.items) child(item); break
        case "Record": for (const [name, value] of expr.fields) { bytes = this.capBytes(bytes + utf8(name)); child(value) } break
      }
      if (depth + height > 128) throw new BudgetExceeded("inline expressions exceed expansion nesting bound")
      const result = { work, bytes, lines: 0, height }; memo.set(expr, result); return result
    } finally { active.delete(expr) }
  }
}
