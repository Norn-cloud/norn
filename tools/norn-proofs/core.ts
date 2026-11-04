import { check, type CheckOptions, type CheckResult } from "tla-ts"

type Statistics = Pick<CheckResult, "generatedStates" | "distinctStates" | "queuedStates">
export type CheckedResult =
  | (Statistics & { status: "success"; initialStates: NonNullable<CheckResult["initialStates"]> })
  | (Statistics & { status: "invariant-violation"; violation: NonNullable<CheckResult["violation"]> })
  | { status: "unknown"; reason: string }

export type CheckRequest = Pick<CheckOptions, "source" | "config" | "loadModule"> & {
  seed: bigint
  expectedStates: number
}

/** PROOF-TLA-1: the same single-worker checking path for CLI and Worker hosts. */
export async function checkFinite(request: CheckRequest): Promise<CheckedResult> {
  if (!Number.isSafeInteger(request.expectedStates) || request.expectedStates < 1) {
    return { status: "unknown", reason: "invalid declared state cardinality" }
  }
  // Retain the qualified integer budget: two expanded additive layers, each
  // bounded by 4096 leaves, must fit the interpreter's signed scalar range.
  if ([...request.source.matchAll(/(?<![A-Za-z_0-9])\d+/g)]
      .some(literal => BigInt(literal[0]) * 4096n * 4096n > 2_147_483_647n)) {
    return { status: "unknown", reason: "tla-ts integer arithmetic exceeds qualified profile" }
  }
  try {
    const result = await check({ source: request.source, config: request.config,
      loadModule: request.loadModule, seed: request.seed, workers: 1 })
    const statistics = { generatedStates: result.generatedStates,
      distinctStates: result.distinctStates, queuedStates: result.queuedStates }
    if (result.status === "success") {
      const initialStates = result.initialStates
      if (!initialStates || initialStates.distinct < 1 || result.queuedStates !== 0 ||
          result.distinctStates !== request.expectedStates) {
        return { status: "unknown", reason: "empty initialization, unfinished search or state cardinality mismatch" }
      }
      return { status: "success", ...statistics, initialStates }
    }
    if (result.status === "invariant-violation" && result.violation && result.violation.trace.length > 0) {
      return { status: "invariant-violation", ...statistics, violation: result.violation }
    }
    return { status: "unknown", reason: `checker returned ${result.status}: ${JSON.stringify(result.failure)}` }
  } catch (error) {
    return { status: "unknown", reason: String(error) }
  }
}

/** Exact JSON transport for the pinned library's integer-valued result objects. */
export function encodeResult(result: CheckedResult, metadata?: { modelSha256: string; elapsedMs?: number }): string {
  return JSON.stringify(metadata ? { ...metadata, outcome: result } : result,
    (_key, value: unknown) => typeof value === "bigint" ? value.toString() : value)
}
