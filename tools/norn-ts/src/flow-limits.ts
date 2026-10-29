/**
 * Flow limits for `norn plan` (T7-TS-4, #2692): per flow, the maximum
 * attempts and retry delays of every perform, the sleep timers, the durable
 * steps and decision rows, and the maximum fan-out (commands and timers one
 * decision emits), checked against the builder's static bounds
 * (`FLOW_LIMITS`) and the cell's `norn.limits/1` envelope caps.
 *
 * Attempts, delays, and steps come from the compiled definition (exact);
 * fan-out comes from the lowered decide rule's rows, which the builder
 * renders. The resource profile is read from the Rust-owned canonical golden
 * (`crates/norn-ir/tests/golden/norn.limits.v1.canonical.json`), never
 * restated here.
 */
import { readFileSync } from "node:fs"
import { join } from "node:path"
import type { Json } from "./canonical.ts"
import { FLOW_LIMITS, retryDelay } from "./builder/lower-flow.ts"
import type { FlowDef, Step } from "./builder/flow.ts"
import { TOOL_ROOT } from "./profile.ts"

export const LIMITS_PROFILE_PATH = join(TOOL_ROOT, "..", "..", "crates", "norn-ir", "tests", "golden", "norn.limits.v1.canonical.json")

export interface LimitsProfile {
  readonly id: string
  readonly version: number
  readonly envelope: { readonly max_commands: number; readonly max_events: number; readonly max_output_bytes: number; readonly max_timers: number }
}

export function loadLimitsProfile(path: string = LIMITS_PROFILE_PATH): LimitsProfile {
  return JSON.parse(readFileSync(path, "utf8")) as LimitsProfile
}

export interface StepLimits {
  readonly step: string
  readonly kind: "perform" | "sleep"
  readonly function?: string
  /** Attempts including the first (`recurs + 1`); 1 without a retry. */
  readonly maxAttempts?: number
  /** The timer before each retry, in order. */
  readonly retryDelaysMillis?: ReadonlyArray<number>
  readonly timerMillis?: number
}

export interface LimitCheck {
  readonly limit: string
  readonly value: number
  readonly max: number
  readonly ok: boolean
}

export interface FlowLimits {
  readonly flow: string
  readonly steps: ReadonlyArray<StepLimits>
  readonly durableSteps: number
  readonly rows: number
  /** Most commands one decision emits. */
  readonly maxFanOut: number
  /** Most timers one decision emits. */
  readonly maxTimersPerDecision: number
  /** Longest single timer. */
  readonly maxTimerMillis: number
  /** The cell's per-decision output cap (`envelope.max_output_bytes`); the payload a decision emits must fit it. */
  readonly maxOutputBytes: number
  readonly checks: ReadonlyArray<LimitCheck>
}

function walk(steps: ReadonlyArray<Step>, out: StepLimits[]): void {
  for (const step of steps) {
    switch (step._tag) {
      case "Perform":
        out.push({ step: step.step, kind: "perform", function: `${step.fn.id}@${step.fn.version}`, maxAttempts: 1, retryDelaysMillis: [] })
        break
      case "Retry": {
        const recurs = step.policy.recurs ?? 0
        const delays = Array.from({ length: recurs }, (_, i) => retryDelay(step.policy, i + 1))
        out.push({ step: step.perform.step, kind: "perform", function: `${step.perform.fn.id}@${step.perform.fn.version}`, maxAttempts: recurs + 1, retryDelaysMillis: delays })
        break
      }
      case "Sleep":
        out.push({ step: step.step, kind: "sleep", timerMillis: step.millis })
        break
      case "Route":
        for (const branch of step.branches) walk(branch.then, out)
        walk(step.otherwise, out)
        break
      case "CatchTag":
        walk(step.body, out)
        walk(step.handler, out)
        break
      case "Compose":
        walk(step.body, out)
        break
      case "All":
        walk(step.steps, out)
        break
      default:
        break
    }
  }
}

/**
 * The number of top-level elements of a rendered ZEN list literal (`[{…}, {…}]`),
 * counted structurally: brackets inside string literals and nested records are
 * skipped, so a command's input data never counts as another command.
 */
export function listLength(text: string): number {
  const source = text.trim()
  if (!source.startsWith("[")) return 0
  let depth = 0, elements = 0, quote: string | undefined, sawValue = false
  for (let i = 0; i < source.length; i++) {
    const c = source[i]!
    if (quote) {
      if (c === "\\") i++
      else if (c === quote) quote = undefined
      continue
    }
    if (c === "'" || c === '"') { quote = c; if (depth === 1) sawValue = true; continue }
    if (c === "[" || c === "{" || c === "(") {
      if (depth === 1) sawValue = true
      depth++
      continue
    }
    if (c === "]" || c === "}" || c === ")") {
      depth--
      if (depth === 0) return elements + (sawValue ? 1 : 0)
      continue
    }
    if (depth === 1) {
      if (c === ",") { elements++; sawValue = false }
      else if (!/\s/.test(c)) sawValue = true
    }
  }
  return elements + (sawValue ? 1 : 0)
}

/** The rows of a lowered flow's decide rule (its first decision table). */
function rows(rule: Json | undefined): ReadonlyArray<Record<string, string>> {
  const graph = (rule as { payload?: { graph?: { nodes?: Array<{ type?: string; content?: { rules?: Array<Record<string, string>> } }> } } } | undefined)?.payload?.graph
  return graph?.nodes?.find((n) => n.type === "decisionTableNode")?.content?.rules ?? []
}

export function flowLimits(def: FlowDef, artifacts: ReadonlyArray<Json>, profile: LimitsProfile = loadLimitsProfile()): FlowLimits {
  const steps: StepLimits[] = []
  walk(def.body, steps)
  const rule = artifacts.find((a) => (a as { kind?: string }).kind === "rule")
  // The final blank-guard row (every other event is a no-op) is not a generated row; FLOW_LIMITS.maxRows excludes it too.
  const table = rows(rule).filter((r) => (r.c_when ?? "").trim() !== "")
  const maxFanOut = Math.max(0, ...table.map((r) => listLength(r.o_commands ?? "")))
  const maxTimersPerDecision = Math.max(0, ...table.map((r) => listLength(r.o_timers ?? "")))
  const timers = steps.flatMap((s) => [...(s.retryDelaysMillis ?? []), ...(s.timerMillis === undefined ? [] : [s.timerMillis])])
  const maxTimerMillis = Math.max(0, ...timers)
  const maxRecurs = Math.max(0, ...steps.map((s) => (s.maxAttempts ?? 1) - 1))
  const check = (limit: string, value: number, max: number): LimitCheck => ({ limit, value, max, ok: value <= max })
  return {
    flow: `${def.meta.id}@${def.meta.version}`,
    steps,
    durableSteps: steps.length,
    rows: table.length,
    maxFanOut,
    maxTimersPerDecision,
    maxTimerMillis,
    maxOutputBytes: profile.envelope.max_output_bytes,
    checks: [
      check("flow.maxRecurs", maxRecurs, FLOW_LIMITS.maxRecurs),
      check("flow.maxTimerMillis", maxTimerMillis, FLOW_LIMITS.maxTimerMillis),
      check("flow.maxSteps", steps.length, FLOW_LIMITS.maxSteps),
      check("flow.maxRows", table.length, FLOW_LIMITS.maxRows),
      check(`${profile.id}/${profile.version}.envelope.max_commands`, maxFanOut, profile.envelope.max_commands),
      check(`${profile.id}/${profile.version}.envelope.max_timers`, maxTimersPerDecision, profile.envelope.max_timers),
    ],
  }
}

export function renderFlowLimits(limits: ReadonlyArray<FlowLimits>): string {
  const lines: string[] = []
  for (const f of limits) {
    lines.push(`flow ${f.flow}: ${f.durableSteps} durable steps, ${f.rows} rows, fan-out ${f.maxFanOut}, ${f.maxTimersPerDecision} timer(s)/decision, longest timer ${f.maxTimerMillis} ms, output cap ${f.maxOutputBytes} B/decision`)
    for (const s of f.steps) {
      lines.push(
        s.kind === "perform"
          ? `  perform ${s.step} -> ${s.function}: max ${s.maxAttempts} attempt(s)${(s.retryDelaysMillis ?? []).length > 0 ? `, retry timers ${(s.retryDelaysMillis ?? []).join("/")} ms` : ""}`
          : `  sleep ${s.step}: ${s.timerMillis} ms`,
      )
    }
    for (const c of f.checks.filter((x) => !x.ok)) lines.push(`  OVER LIMIT ${c.limit}: ${c.value} > ${c.max}`)
  }
  return lines.join("\n")
}
