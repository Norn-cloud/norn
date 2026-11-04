import type { ProcessResult } from "./host"
import { object, strings, array, integer } from "./protocol"

export type Trace = { state: Record<string, boolean | number> }[]
export type Verdict = { status: "ok" } | { status: "violation"; trace: Trace } | { status: "unknown"; reason: string }

/** Strict pinned ITF boundary: no rounding, missing variables, duplicate names or reordered states. */
export function decodeTrace(value: unknown): Trace {
  if (!object(value) || !strings(value.vars) || value.vars.length === 0 ||
      new Set(value.vars).size !== value.vars.length || !array(value.states) || value.states.length === 0) throw new Error("malformed ITF trace")
  const variables = value.vars
  const names = new Set(variables)
  return value.states.map((entry, index) => {
    if (!object(entry) || !object(entry["#meta"]) || entry["#meta"].index !== index ||
        Object.keys(entry).length !== variables.length + 1 ||
        Object.keys(entry).some(key => key !== "#meta" && !names.has(key))) throw new Error("malformed ITF state")
    const values: [string, boolean | number][] = []
    for (const name of variables) {
      const raw = entry[name]
      if (typeof raw === "boolean") values.push([name, raw])
      else {
        if (!object(raw) || Object.keys(raw).length !== 1) throw new Error("inexact ITF integer")
        values.push([name, Number(integer(raw["#bigint"], -2147483648n, 2147483647n))])
      }
    }
    return { state: Object.fromEntries(values) }
  })
}

export function traceShape(value: unknown): string | undefined {
  if (!array(value) || value.length === 0) return undefined
  const shape = []
  for (const step of value) {
    if (!object(step) || !object(step.state) || typeof step.state.present !== "boolean") return undefined
    if (!Object.values(step.state).every(v => typeof v === "boolean" ||
      (typeof v === "number" && Number.isSafeInteger(v) && v >= -2147483648 && v <= 2147483647))) return undefined
    shape.push([step.state.present, Object.entries(step.state).map(([key, v]) => [key, typeof v]).sort()])
  }
  return JSON.stringify(shape)
}

export function oracleVerdict(result: ProcessResult, trace: unknown): Verdict {
  if (result.kind === "unknown") return { status: "unknown", reason: result.reason }
  const outcomes = [...result.stdout.matchAll(/^The outcome is: (\w+)\b/gm)].map(match => match[1])
  if (result.code === 0 && outcomes.length === 1 && outcomes[0] === "NoError") return { status: "ok" }
  if (result.code === 12 && outcomes.length === 1 && outcomes[0] === "Error" && /State \d+: state invariant \d+ violated\./.test(result.stdout)) {
    try { return { status: "violation", trace: decodeTrace(trace) } }
    catch (error) { return { status: "unknown", reason: String(error) } }
  }
  return { status: "unknown", reason: `unqualified Apalache exit ${result.code}/outcome` }
}

export function parity(primary: unknown, oracle: Verdict, expected: number): Verdict {
  if (!object(primary) || !Number.isSafeInteger(expected) || expected < 1) return { status: "unknown", reason: "malformed primary result or cardinality" }
  if (oracle.status === "unknown") return oracle
  if (primary.status === "unknown" && typeof primary.reason === "string") return { status: "unknown", reason: `primary: ${primary.reason}` }
  if (primary.status === "success" && primary.distinctStates === expected && primary.queuedStates === 0 &&
      object(primary.initialStates) && typeof primary.initialStates.distinct === "number" &&
      primary.initialStates.distinct > 0 && oracle.status === "ok") return { status: "ok" }
  const trace = object(primary.violation) ? primary.violation.trace : undefined
  if (primary.status === "invariant-violation" && oracle.status === "violation" &&
      traceShape(trace) !== undefined && traceShape(trace) === traceShape(oracle.trace)) return oracle
  return { status: "unknown", reason: "unqualified or disagreeing backend verdicts" }
}
