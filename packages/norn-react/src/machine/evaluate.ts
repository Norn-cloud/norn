// The ZEN evaluator a page machine runs with: the checker's own engine, compiled to WASM
// (`evaluateExpression` in norn-checker-wasm). The checker type-checks guards and assignments
// against `input.fields` and `input.event`; the same engine evaluates them here.

import type { Evaluate } from "./machine.ts"

/** The slice of the checker WASM module the runtime needs. */
export interface CheckerEvaluator {
  evaluateExpression(expression: string, inputJson: string, profile?: string): string
}

export class EvaluationError extends Error {
  readonly expression: string
  readonly code: string
  constructor(expression: string, code: string, message: string) {
    super(message)
    this.expression = expression
    this.code = code
  }
}

export function wasmEvaluate(checker: CheckerEvaluator): Evaluate {
  return (expression, input) => {
    const result = JSON.parse(checker.evaluateExpression(expression, JSON.stringify(input))) as {
      readonly ok: boolean
      readonly value?: unknown
      readonly error?: { readonly code?: string; readonly message?: string }
    }
    if (!result.ok) {
      throw new EvaluationError(expression, result.error?.code ?? "unknown", result.error?.message ?? `expression ${expression} failed`)
    }
    return result.value ?? null
  }
}

/**
 * Thrown by a lazy evaluator asked for an expression before the engine is here. The driver defers
 * the message that needed it and replays it, in order, once the engine is ready (`resume`).
 */
export class EngineNotReady extends Error {
  constructor(expression: string) {
    super(`the expression engine is not loaded yet (${expression})`)
  }
}

/**
 * The one ZEN form the runtime reads without the engine: a Boolean literal or a double-quoted text
 * literal with no escapes, as a page binds a constant option (`dismissible: false`, a label).
 * Anything else, numbers included (an exact decimal is the engine's to read), waits for the engine.
 */
export function literal(expression: string): { readonly value: boolean | string } | null {
  const source = expression.trim()
  if (source === "true" || source === "false") return { value: source === "true" }
  if (/^"[^"\\]*"$/.test(source)) return { value: source.slice(1, -1) }
  return null
}

/** The checker's engine, loaded on first use or when asked; guard-free work never waits for it. */
export interface LazyEngine {
  readonly evaluate: Evaluate
  /**
   * One expression over explicit inputs other than the machine's (an action binding's
   * `input.record`, `input.context`, …): a constant at once, anything else once the engine is here.
   */
  readonly evaluateWith: (expression: string, input: unknown) => Promise<unknown>
  /** Starts loading (once) and resolves when the engine is ready. */
  readonly load: () => Promise<void>
  readonly state: () => "idle" | "loading" | "ready" | "failed"
}

export function lazyEngine(loader: () => Promise<CheckerEvaluator>): LazyEngine {
  let ready: Evaluate | null = null
  let engine: CheckerEvaluator | null = null
  let loading: Promise<void> | null = null
  let failed = false
  const load = () => {
    loading ??= loader().then(
      (checker) => {
        engine = checker
        ready = wasmEvaluate(checker)
      },
      (error: unknown) => {
        failed = true
        loading = null
        throw error
      },
    )
    return loading
  }
  return {
    evaluate: (expression, input) => {
      if (ready !== null) return ready(expression, input)
      const constant = literal(expression)
      if (constant !== null) return constant.value
      throw new EngineNotReady(expression)
    },
    evaluateWith: async (expression, input) => {
      const constant = engine === null ? literal(expression) : null
      if (constant !== null) return constant.value
      await load()
      return wasmEvaluate(engine!)(expression, input as Parameters<Evaluate>[1])
    },
    load,
    state: () => (ready !== null ? "ready" : failed ? "failed" : loading !== null ? "loading" : "idle"),
  }
}
