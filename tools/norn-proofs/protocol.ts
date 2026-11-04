export const object = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value)
export const array = (value: unknown): value is unknown[] => Array.isArray(value)
export const strings = (value: unknown): value is string[] => array(value) && value.every(item => typeof item === "string")

/** Exact wire integer text, including the actual end of input rather than JavaScript `$`. */
export function integer(value: unknown, lower: bigint, upper: bigint): bigint {
  if (typeof value !== "string" || value.length > 20 || !/^(0|-?[1-9][0-9]*)(?![\s\S])/.test(value)) throw new Error("noncanonical integer text")
  const result = BigInt(value)
  if (result < lower || result > upper) throw new Error("out-of-profile integer")
  return result
}

export type Job = { key: string; model: string; result: string; modelSha: string; challenge: string;
  invariant: "law" | "unreachable"; step: "idle" | "step"; seed: string; expectedStates: number }
export function parseJob(value: unknown): Job {
  if (!object(value) || Object.keys(value).length !== 9 || typeof value.key !== "string" ||
      typeof value.model !== "string" || typeof value.result !== "string" ||
      typeof value.modelSha !== "string" || !/^[a-f0-9]{64}(?![\s\S])/.test(value.modelSha) ||
      typeof value.challenge !== "string" || !/^[a-f0-9]{64}(?![\s\S])/.test(value.challenge) ||
      (value.invariant !== "law" && value.invariant !== "unreachable") ||
      (value.step !== "idle" && value.step !== "step") || typeof value.seed !== "string" ||
      typeof value.expectedStates !== "number" || !Number.isSafeInteger(value.expectedStates) || value.expectedStates < 1) throw new Error("malformed generated job")
  integer(value.seed, 0n, 18446744073709551615n)
  return { key: value.key, model: value.model, result: value.result, modelSha: value.modelSha,
    challenge: value.challenge, invariant: value.invariant, step: value.step, seed: value.seed, expectedStates: value.expectedStates }
}
