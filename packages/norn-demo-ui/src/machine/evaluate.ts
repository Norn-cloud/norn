// The ZEN evaluator a page machine runs with: the checker's own engine, compiled to WASM
// (`evaluateExpression` in norn-checker-wasm). The checker type-checks guards and assignments
// against `input.fields` and `input.event`; the same engine evaluates them here.

import type { Evaluate } from "./machine.ts"

/** The slice of the checker WASM module the runtime needs. */
export interface CheckerEvaluator {
  evaluateExpression(expression: string, inputJson: string, profile?: string): string
}

export class EvaluationError extends Error {
  constructor(
    readonly expression: string,
    readonly code: string,
    message: string,
  ) {
    super(message)
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
