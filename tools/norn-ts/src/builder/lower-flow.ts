/**
 * Flow lowering (T7-FLOW-1, #2649): builder data → ordinary artifacts.
 *
 * A {@link FlowDef} lowers to what ruling 2026-09-25 §4 already has, and
 * nothing else (no FlowIR, process artifact, program counter, or Effect
 * runtime):
 *
 * 1. **A Run type** (`<ns>.<Name>Run`, an `entity` artifact, EntityIR v2):
 *    `run_id` (identity), the lifecycle property `status`, and `slots`, a
 *    `json-ext` result slot holding the bindings that live across a durable
 *    boundary, the flow result, and an uncaught failure. The generated
 *    lifecycle is the single owner of the Run's state machine: states
 *    `started`, `awaiting_perform_<step>` for one-shot performs,
 *    `awaiting_retry_<step>_attempt_<n>` and
 *    `retrying_<step>_attempt_<n>` for retries, `sleeping_<step>`,
 *    `completed`, and `failed_<tag>`; one transition `to_<state>` per target.
 * 2. **One `decide` decision** (`<flowId>.flow.rule`, RuleIR v2): a first-hit
 *    decision table over `(slice.run.status, event)`. Each row is one durable
 *    step: it fires the Run transition, emits `perform` commands and timers,
 *    and writes the result slot. Guards in a state are pairwise disjoint, so
 *    first-hit order never changes the outcome. A last blank row matches
 *    every other event and does nothing: a duplicate result, a late timer,
 *    or a result for a step the Run is no longer waiting on cannot advance
 *    it.
 *
 * Event contract (until T1-FN-1/T2-RES-1 fix the receipt shape; documented
 * in the module and checked by the decision's closed input schema):
 * `{kind: "start", runId}`, `{kind: "result", runId, step, attempt,
 * results: {<step>: output}}`, `{kind: "failure", runId, step, attempt,
 * failure: {_tag, ...fields}}`, and the timer record delivered back
 * (`{kind: "timer", runId, step, attempt, timerId, afterMillis}`).
 *
 * Retry: `recurs(n)` is `n` retries. Every receipt and timer is correlated
 * to its run and step; the attempt travels in the command and event, while
 * the Run status encodes the expected attempt. Timer ids include the attempt
 * for retries and the step for sleeps. The business idempotency key
 * (`<runId>:<step>`) is the same for every attempt.
 *
 * Every finding is a `NORN-FLOW-*` diagnostic with the builder call site;
 * nothing is clamped or approximated. The Rust checker validates the result.
 */
import { type Json, toJson } from "../canonical.ts"
import { FLOW_SOURCE_MAP_DRAFT_FORMAT, flowSourceMapGraph, type FlowSourceMapDraft, type FlowSourceMapGraph } from "../flow-source-map.ts"
import { Diagnostics, type BuildDiagnostic, type SourceLocation } from "./diagnostics.ts"
import { type EnvelopeOptions, envelope } from "./envelope.ts"
import { type Binding, type Expr, ExprError, type Scope, render, typeOf, zenJson, zenString } from "./expr.ts"
import { BudgetExceeded, ExpansionBudget, type ExpansionLimits } from "./budget.ts"
import type { All, CatchTag, FlowDef, FnRef, Perform, RetryPolicy, Step, TaggedErrorDecl } from "./flow.ts"
import {
  type ObjectSchema,
  type ValueSchema,
  array,
  integer,
  mergeSchemas,
  record,
  text,
} from "./schema.ts"

/** Static bounds; a definition over a bound is rejected, never clamped. */
export const FLOW_LIMITS = {
  /** `Schedule.recurs(n)`: at most this many retries. */
  maxRecurs: 10,
  /** Any one timer (sleep or retry delay). */
  maxTimerMillis: 7 * 24 * 3_600_000,
  /** Durable steps (perform and sleep sites) in one flow. */
  maxSteps: 64,
  /** Decision-table rows. */
  maxRows: 1024,
} as const

/** Install-time facts the artifact envelope needs; not part of the definition. */
export interface BuildOptions extends EnvelopeOptions {
  /** Compiler-owned resource limits; no authoring code executes through these options. */
  readonly expansionLimits?: ExpansionLimits
  readonly expansionBudget?: ExpansionBudget
  /** Emit the separate, non-executable authoring graph projection (default true). */
  readonly includeFlowSourceMap?: boolean
  /** Root used for source-map locations; never written into lowered artifacts. */
  readonly sourceRoot?: string
}

export type BuildResult =
  | { readonly ok: true; readonly artifacts: ReadonlyArray<Json>; readonly sourceMap: Json; readonly flowSourceMap?: FlowSourceMapDraft }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<BuildDiagnostic> }

const STEP_ID = /^[a-z][a-z0-9_]*$/
const FLOW_ID = /^[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)+$/
const TAG = /^[A-Z][A-Za-z0-9]*$/

type Frame =
  | { readonly kind: "rest"; readonly steps: ReadonlyArray<Step>; readonly index: number; readonly outer: ReadonlySet<string>; readonly sourceMapContext: ReadonlyArray<string> }
  | { readonly kind: "catch"; readonly node: CatchTag; readonly sourceMapContext: ReadonlyArray<string> }
  | { readonly kind: "compose"; readonly name: string; readonly schema: ValueSchema; readonly outer: ReadonlySet<string>; readonly at: SourceLocation | undefined; readonly sourceMapContext: ReadonlyArray<string> }

/** The continuation: frames, top last. */
type Cont = ReadonlyArray<Frame>

interface Row {
  readonly id: string
  readonly from: string
  readonly guard: string
  readonly to: string
  readonly facts: ReadonlyArray<string>
  readonly commands: ReadonlyArray<string>
  readonly timers: ReadonlyArray<string>
  readonly description: string
  readonly at: SourceLocation | undefined
}

/** One straight-line path through a segment, from a resume point to the next durable boundary. */
interface Path {
  readonly from: string
  readonly event: string
  readonly guards: ReadonlyArray<string>
  readonly scope: Scope
  readonly facts: ReadonlyArray<string>
  readonly key: string
  readonly start: boolean
  /** Structural source nodes that control this path through its next durable boundary. */
  readonly sourceMapContext: ReadonlyArray<string>
  /** Source nodes responsible for the event currently being lowered. */
  readonly sourceMapOwners: ReadonlyArray<string>
}

interface SourceMapLinks {
  readonly states: Set<string>
  readonly rows: Set<string>
}

const snake = (tag: string) => tag.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase()
const pascal = (id: string) => id.split("_").map((s) => s.charAt(0).toUpperCase() + s.slice(1)).join("")
class LoweringBound extends Error {}

/** Lower a flow definition to its Run type and decision. */
export function lowerFlow(def: FlowDef, options: BuildOptions): BuildResult {
  try { return new FlowLowering(def, options).run() } catch (error) {
    if (!(error instanceof BudgetExceeded)) throw error
    return { ok: false, diagnostics: [{ code: "NORN-FLOW-UNBOUNDED", message: error.message, ...(def.at ? { at: def.at } : {}) }] }
  }
}

class FlowLowering {
  private readonly diagnostics = new Diagnostics()
  private readonly namespace: string
  private readonly runType: string
  private readonly rows: Row[] = []
  private readonly rowCounts = new Map<string, number>()
  /** Target state → transition sources, in first-seen order. */
  private readonly transitions = new Map<string, string[]>()
  private readonly states = new Map<string, SourceLocation | undefined>([["started", undefined]])
  private readonly terminal = new Set<string>()
  /** Durable step id → its node (each durable occurrence names one site). */
  private readonly steps = new Map<string, Step>()
  private readonly performs = new Map<string, Perform>()
  private readonly sleeps = new Set<string>()
  private readonly functions = new Map<string, FnRef>()
  private readonly errors = new Map<string, TaggedErrorDecl>()
  private readonly bindingSchemas = new Map<string, ValueSchema>()
  private resultSchema: ValueSchema | undefined
  private uncaught: ValueSchema | undefined
  private readonly queue: Array<() => void> = []
  private readonly registered = new Set<string>()
  /** Perform steps whose failures are retried (each owns a retry timer). */
  private readonly retried = new Set<string>()
  private proceedDepth = 0
  private readonly budget: ExpansionBudget
  private sourceMapGraph?: FlowSourceMapGraph
  private readonly sourceMapLinks = new Map<string, SourceMapLinks>()

  constructor(
    private readonly def: FlowDef,
    private readonly options: BuildOptions,
  ) {
    this.budget = options.expansionBudget ?? new ExpansionBudget(options.expansionLimits)
    const id = def.meta.id
    if (!FLOW_ID.test(id)) {
      this.report("NORN-FLOW-LITERAL", `flow id ${JSON.stringify(id)} must be dotted lower-case segments (namespace.name)`, def.at)
    }
    const dot = id.lastIndexOf(".")
    this.namespace = id.slice(0, Math.max(dot, 0))
    this.runType = `${this.namespace}.${pascal(id.slice(dot + 1))}Run`
  }

  private report(code: string, message: string, at: SourceLocation | undefined): void {
    this.diagnostics.report(code, message, at)
  }

  /** Run `f`, turning an expression finding into a diagnostic at `at`. */
  private expr<T>(at: SourceLocation | undefined, f: () => T): T | undefined {
    try {
      return f()
    } catch (error) {
      if (error instanceof ExprError) {
        this.report(error.code, error.message, at)
        return undefined
      }
      throw error
    }
  }

  run(): BuildResult {
    this.budget.assert(this.def)
    this.collect(this.def.body)
    if (!this.diagnostics.empty) return { ok: false, diagnostics: this.diagnostics.items }
    if (this.options.includeFlowSourceMap !== false) {
      const graph = flowSourceMapGraph(this.def.body)
      if (graph.steps.length > 0) {
        this.sourceMapGraph = graph
        for (const step of graph.steps) this.sourceMapLinks.set(step.id, { states: new Set(), rows: new Set() })
      }
    }
    const start: Path = {
      from: "started",
      event: "start",
      guards: ["slice.run == null", "event.kind == 'start'"],
      scope: new Map(),
      facts: [`{op: 'create', objectType: ${zenString(this.runType)}, key: event.runId, values: {run_id: event.runId, status: 'started'}}`],
      key: "event.runId",
      start: true,
      sourceMapContext: [],
      sourceMapOwners: [],
    }
    try {
      this.proceed(start, [{ kind: "rest", steps: this.def.body, index: 0, outer: new Set(), sourceMapContext: [] }])
      while (this.queue.length > 0) this.queue.shift()!()
    } catch (error) {
      if (!(error instanceof LoweringBound)) throw error
    }
    if (this.rows.length > FLOW_LIMITS.maxRows) {
      this.report("NORN-FLOW-UNBOUNDED", `the flow lowers to ${this.rows.length} decision rows, over the bound of ${FLOW_LIMITS.maxRows}`, this.def.at)
    }
    if (!this.diagnostics.empty) return { ok: false, diagnostics: this.diagnostics.items }
    const artifacts = [this.entity(), this.rule()]
    const sourceMap = this.sourceMap()
    const flowSourceMap = this.flowSourceMap()
    return { ok: true, artifacts, sourceMap, ...(flowSourceMap ? { flowSourceMap } : {}) }
  }

  // ------------------------------------------------------------ validation

  /** Register steps, functions, and errors; check ids, bounds, and tags before lowering. */
  private collect(steps: ReadonlyArray<Step>): void {
    for (const step of steps) {
      switch (step._tag) {
        case "Compose":
          this.binding(step.name, step.schema, step.at)
          this.collect(step.body)
          break
        case "All":
          this.collect(step.steps)
          break
        case "Value":
          break
        case "Retry":
          this.checkPolicy(step.policy, step.at)
          this.collect([step.perform])
          if (step.perform.fn.errors.length > 0) this.retried.add(step.perform.step)
          break
        case "Perform":
          this.durable(step.step, step, step.at)
          this.performs.set(step.step, step)
          this.registerFunction(step.fn)
          if (step.bind !== undefined) this.binding(step.bind, step.fn.output, step.at)
          break
        case "Sleep":
          this.durable(step.step, step, step.at)
          this.sleeps.add(step.step)
          if (!Number.isInteger(step.millis) || step.millis <= 0 || step.millis > FLOW_LIMITS.maxTimerMillis) {
            this.report("NORN-FLOW-UNBOUNDED", `sleep ${step.step} must be a whole number of milliseconds in (0, ${FLOW_LIMITS.maxTimerMillis}]`, step.at)
          }
          break
        case "Route":
          for (const branch of step.branches) this.collect(branch.then)
          this.collect(step.otherwise)
          break
        case "CatchTag": {
          if (!this.raises(step.body).has(step.tag)) {
            this.report(
              "NORN-FLOW-TAG",
              `catchTag("${step.tag}") catches a tag no Function performed in its body declares and no Effect.fail there raises`,
              step.at,
            )
          }
          this.collect(step.body)
          const error = this.errors.get(step.tag)
          if (step.as !== undefined && error) this.binding(step.as, this.failureRecord(error), step.at)
          this.collect(step.handler)
          break
        }
        case "Fail":
          this.registerError(step.error)
          break
        case "Succeed":
          break
      }
    }
  }

  /** Tags a block can fail with (declared errors of its performs, raised failures). */
  private raises(steps: ReadonlyArray<Step>): Set<string> {
    const out = new Set<string>()
    for (const step of steps) {
      switch (step._tag) {
        case "Perform":
          for (const e of step.fn.errors) out.add(e.tag)
          break
        case "Retry":
          for (const e of step.perform.fn.errors) out.add(e.tag)
          break
        case "Fail":
          out.add(step.error.tag)
          break
        case "Route":
          for (const b of step.branches) for (const t of this.raises(b.then)) out.add(t)
          for (const t of this.raises(step.otherwise)) out.add(t)
          break
        case "CatchTag": {
          for (const t of this.raises(step.body)) if (t !== step.tag) out.add(t)
          for (const t of this.raises(step.handler)) out.add(t)
          break
        }
        default:
          if (step._tag === "Compose") for (const t of this.raises(step.body)) out.add(t)
          if (step._tag === "All") for (const t of this.raises(step.steps)) out.add(t)
          break
      }
    }
    return out
  }

  private durable(id: string, node: Step, at: SourceLocation | undefined): void {
    if (!STEP_ID.test(id)) this.report("NORN-FLOW-LITERAL", `step id ${JSON.stringify(id)} must match [a-z][a-z0-9_]*`, at)
    const seen = this.steps.get(id)
    if (seen) this.report("NORN-FLOW-UNSUPPORTED-SYNTAX", `step id "${id}" occurs more than once; durable step ids are stable and unique per flow`, at)
    this.steps.set(id, node)
    if (this.steps.size > FLOW_LIMITS.maxSteps) {
      this.report("NORN-FLOW-UNBOUNDED", `more than ${FLOW_LIMITS.maxSteps} durable steps`, at)
    }
  }

  private registerFunction(handle: FnRef): void {
    const seen = this.functions.get(handle.id)
    if (seen && seen.version !== handle.version) {
      this.report("NORN-FLOW-UNSUPPORTED-SYNTAX", `Function ${handle.id} is used at versions ${seen.version} and ${handle.version}`, handle.at)
    }
    this.functions.set(handle.id, handle)
    for (const e of handle.errors) this.registerError(e)
  }

  private registerError(decl: TaggedErrorDecl): void {
    if (!TAG.test(decl.tag)) this.report("NORN-FLOW-LITERAL", `error tag ${JSON.stringify(decl.tag)} must be PascalCase`, decl.at)
    const seen = this.errors.get(decl.tag)
    if (seen && JSON.stringify(toJson(seen.fields)) !== JSON.stringify(toJson(decl.fields))) {
      this.report("NORN-FLOW-TAG", `error tag ${decl.tag} is registered twice with different fields`, decl.at)
    }
    if ("_tag" in decl.fields.properties) this.report("NORN-FLOW-TAG", `error ${decl.tag} declares a field named _tag`, decl.at)
    this.errors.set(decl.tag, decl)
  }

  private binding(name: string, schema: ValueSchema, at: SourceLocation | undefined): void {
    if (!STEP_ID.test(name)) this.report("NORN-FLOW-LITERAL", `binding name ${JSON.stringify(name)} must match [a-z][a-z0-9_]*`, at)
    if (this.bindingSchemas.has(name)) {
      this.report("NORN-FLOW-UNSUPPORTED-SYNTAX", `binding "${name}" is declared twice; v1 bindings are unique per flow`, at)
    }
    this.bindingSchemas.set(name, schema)
  }

  private checkPolicy(policy: RetryPolicy, at: SourceLocation | undefined): void {
    const n = policy.recurs
    if (n === undefined) {
      this.report("NORN-FLOW-UNBOUNDED", "the retry policy has no finite Schedule.recurs(n); an unbounded retry is never admitted", at)
      return
    }
    if (!Number.isInteger(n) || n < 1 || n > FLOW_LIMITS.maxRecurs) {
      this.report("NORN-FLOW-UNBOUNDED", `Schedule.recurs(${n}) must be an integer in [1, ${FLOW_LIMITS.maxRecurs}]`, at)
      return
    }
    for (let attempt = 1; attempt <= n; attempt++) {
      const delay = retryDelay(policy, attempt)
      if (!Number.isInteger(delay) || delay < 0 || delay > FLOW_LIMITS.maxTimerMillis) {
        this.report("NORN-FLOW-UNBOUNDED", `retry delay after attempt ${attempt} is ${delay} ms, outside [0, ${FLOW_LIMITS.maxTimerMillis}]`, at)
        return
      }
    }
  }

  private failureRecord(decl: TaggedErrorDecl): ObjectSchema {
    return record({ _tag: text(), ...decl.fields.properties }, Object.keys(decl.fields.properties).filter((k) => !(decl.fields.required ?? []).includes(k)))
  }

  // -------------------------------------------------------------- lowering

  /** Execute `cont` along `path` until a durable boundary or the end of the flow. */
  private proceed(path: Path, cont: Cont): void {
    this.budget.check()
    if (++this.proceedDepth > 512) {
      this.proceedDepth--
      this.report("NORN-FLOW-UNBOUNDED", "composed flow exceeds lowering continuation depth bound", this.def.at)
      throw new LoweringBound()
    }
    try { this.proceedInner(path, cont) } finally { this.proceedDepth-- }
  }

  private proceedInner(path: Path, cont: Cont): void {
    const top = cont[cont.length - 1]
    const below = cont.slice(0, -1)
    if (!top) {
      this.report("NORN-FLOW-RESULT", "the flow can complete without an explicit result; end every path with a return", this.def.at)
      return
    }
    if (top.kind === "catch") return this.proceed({ ...path, sourceMapContext: top.sourceMapContext }, below)
    if (top.kind === "compose") {
      this.report("NORN-FLOW-RESULT", "a composed effect can complete without returning a value", top.at)
      return
    }
    if (top.index >= top.steps.length) {
      // Leaving a block: its bindings go out of scope.
      const scope = new Map([...path.scope].filter(([name]) => top.outer.has(name)))
      return this.proceed({ ...path, scope, sourceMapContext: top.sourceMapContext }, below)
    }
    const step = top.steps[top.index]!
    const next: Cont = [...below, { ...top, index: top.index + 1 }]
    const names = new Set(path.scope.keys())
    switch (step._tag) {
      case "Compose": {
        const context = this.withSourceMap(path.sourceMapContext, step.sourceMap?.id)
        return this.proceed({ ...path, sourceMapContext: context }, [
          ...next,
          { kind: "compose", name: step.name, schema: step.schema, outer: names, at: step.at, sourceMapContext: path.sourceMapContext },
          { kind: "rest", steps: step.body, index: 0, outer: names, sourceMapContext: context },
        ])
      }
      case "All": {
        const context = this.withSourceMap(path.sourceMapContext, step.sourceMap?.id)
        return this.proceed({ ...path, sourceMapContext: context }, [
          ...next,
          { kind: "rest", steps: step.steps, index: 0, outer: this.bindingsIn(step.steps, names), sourceMapContext: path.sourceMapContext },
        ])
      }
      case "Value": {
        let index = next.length - 1
        while (index >= 0 && next[index]!.kind !== "compose") index--
        const frame = next[index]
        if (!frame || frame.kind !== "compose") {
          this.report("NORN-FLOW-RESULT", "a Value must occur inside Compose", step.at)
          return
        }
        const schema = this.expr(step.at, () => {
          const actual = this.typed(step.value, path.scope)
          const problem = conforms(actual, frame.schema)
          if (problem) throw new ExprError("NORN-FLOW-VALUE", problem)
          return actual
        })
        if (!schema) return
        const scope = new Map([...path.scope].filter(([name]) => frame.outer.has(name)))
        scope.set(frame.name, { inline: step.value, scope: path.scope, schema: frame.schema })
        return this.proceed({ ...path, scope, sourceMapContext: frame.sourceMapContext }, next.slice(0, index))
      }
      case "Perform":
        return this.wait(path, step, next, undefined)
      case "Retry":
        return this.wait(path, step.perform, next, step.policy, [step.sourceMap?.id, step.perform.sourceMap?.id].filter((id): id is string => id !== undefined))
      case "Sleep": {
        const state = `sleeping_${step.step}`
        const timer = `{kind: 'timer', runId: ${path.key}, step: '${step.step}', attempt: 0, timerId: ${this.key(path.key, step.step)}, afterMillis: ${step.millis}}`
        const owners = step.sourceMap?.id ? [step.sourceMap.id] : []
        this.emit(path, state, [], [timer], `sleep ${step.millis} ms`, step.at, undefined, owners)
        const timerKey = this.key("slice.run.run_id", step.step)
        this.resume(state, step.at, () => this.proceed(
          this.resumed(state, "timer", [
            "event.runId == slice.run.run_id",
            `event.step == '${step.step}'`,
            "event.attempt == 0",
            `event.timerId == ${timerKey}`,
          ], names, path.sourceMapContext, owners),
          next,
        ))
        return
      }
      case "Route": {
        const context = this.withSourceMap(path.sourceMapContext, step.sourceMap?.id)
        const conds = step.branches.map((b) => this.expr(step.at, () => this.condition(b.when, path.scope)))
        if (conds.some((c) => c === undefined)) return
        const rendered = conds as string[]
        step.branches.forEach((branch, i) => {
          const guards = [...path.guards, ...rendered.slice(0, i).map((c) => `not (${c})`), `(${rendered[i]!})`]
          this.proceed({ ...path, guards, sourceMapContext: context }, [...next, { kind: "rest", steps: branch.then, index: 0, outer: names, sourceMapContext: path.sourceMapContext }])
        })
        const guards = [...path.guards, ...rendered.map((c) => `not (${c})`)]
        this.proceed({ ...path, guards, sourceMapContext: context }, [...next, { kind: "rest", steps: step.otherwise, index: 0, outer: names, sourceMapContext: path.sourceMapContext }])
        return
      }
      case "CatchTag": {
        const context = this.withSourceMap(path.sourceMapContext, step.sourceMap?.id)
        return this.proceed({ ...path, sourceMapContext: context }, [
          ...next,
          { kind: "catch", node: step, sourceMapContext: path.sourceMapContext },
          { kind: "rest", steps: step.body, index: 0, outer: names, sourceMapContext: context },
        ])
      }
      case "Fail": {
        const fields = step.fields
        if (fields._tag !== "Record") {
          this.report("NORN-FLOW-VALUE", "Effect.fail takes the error's fields as a record literal", step.at)
          return
        }
        const tagField: readonly [string, Expr] = ["_tag", { _tag: "Literal", value: step.error.tag }]
        const failure: Expr = { _tag: "Record", fields: [tagField, ...fields.fields] }
        const ok = this.expr(step.at, () => {
          const problem = conforms(this.typed(fields, path.scope), step.error.fields)
          if (problem) throw new ExprError("NORN-FLOW-VALUE", `fields of ${step.error.tag}: ${problem}`)
          return true
        })
        if (!ok) return
        const schema = this.failureRecord(step.error)
        return this.raise({ ...path, sourceMapOwners: this.withSourceMap(path.sourceMapOwners, step.sourceMap?.id) }, next, step.error.tag, { inline: failure, scope: path.scope, schema }, step.at)
      }
      case "Succeed": {
        const lowered = this.expr(step.at, () => ({ schema: this.typed(step.value, path.scope), value: this.rendered(step.value, path.scope) }))
        if (!lowered) return
        const { schema, value } = lowered
        this.result(schema, step.at)
        this.emit(path, "completed", [], [], "complete", step.at, `{result: ${value}}`, step.sourceMap?.id ? [step.sourceMap.id] : [])
        this.terminal.add("completed")
        return
      }
    }
  }

  /** A ZEN boolean condition from a typed expression. */
  private condition(when: Expr, scope: Scope): string {
    const t = this.typed(when, scope)
    if (t.type !== "boolean") throw new ExprError("NORN-FLOW-EXPRESSION", `a route condition must be boolean, not ${t.type}`)
    return this.rendered(when, scope)
  }

  private typed(value: Expr, scope: Scope): ValueSchema {
    this.budget.assertExpression(value, scope)
    const schema = typeOf(value, scope)
    this.budget.assert(schema)
    return schema
  }

  private rendered(value: Expr, scope: Scope): string {
    this.budget.assertExpression(value, scope)
    return render(value, scope)
  }

  private key(runKey: string, step: string): string {
    return `${runKey} + ':${step}'`
  }

  /** Retry timers have per-attempt identity; the business idempotency key stays stable. */
  private retryTimerKey(runKey: string, step: string, attempt: number): string {
    return `${runKey} + ':${step}:attempt:${attempt}'`
  }

  /** Distinct prefixes keep generated one-shot and retry states injective over step ids. */
  private awaitingState(node: Perform, attempt: number, policy: RetryPolicy | undefined): string {
    return policy
      ? `awaiting_retry_${node.step}_attempt_${attempt}`
      : `awaiting_perform_${node.step}`
  }

  private retryingState(node: Perform, attempt: number): string {
    return `retrying_${node.step}_attempt_${attempt}`
  }

  /** Emit the perform command of `node` and wait for its result. */
  private wait(path: Path, node: Perform, next: Cont, policy: RetryPolicy | undefined, owners = node.sourceMap?.id ? [node.sourceMap.id] : []): void {
    const input = this.expr(node.at, () => {
      const problem = conforms(this.typed(node.input, path.scope), node.fn.input)
      if (problem) throw new ExprError("NORN-FLOW-VALUE", `input of ${node.fn.id}: ${problem}`)
      return this.rendered(node.input, path.scope)
    })
    if (input === undefined) return
    const state = this.awaitingState(node, 1, policy)
    this.emit(path, state, [this.command(path.key, node, "1", input)], [], `perform ${node.fn.id}`, node.at, undefined, owners)
    const names = new Set(path.scope.keys())
    this.resume(state, node.at, () => this.awaiting(node, next, policy, names, 1, path.sourceMapContext, owners))
  }

  private command(runKey: string, node: Perform, attempt: string, input: string): string {
    return `{kind: 'perform', runId: ${runKey}, step: '${node.step}', function: ${zenString(node.fn.id)}, functionVersion: ${zenString(node.fn.version)}, idempotencyKey: ${this.key(runKey, node.step)}, attempt: ${attempt}, input: {${node.step}: ${input}}}`
  }

  /** The rows of one expected attempt's waiting state: result, retry, and failure routes. */
  private awaiting(
    node: Perform,
    next: Cont,
    policy: RetryPolicy | undefined,
    names: ReadonlySet<string>,
    attempt: number,
    sourceMapContext: ReadonlyArray<string>,
    owners: ReadonlyArray<string>,
  ): void {
    const state = this.awaitingState(node, attempt, policy)
    const onStep = `event.step == '${node.step}'`
    const onRun = "event.runId == slice.run.run_id"
    const onAttempt = `event.attempt == ${attempt}`
    // The accepted result binds the next source-level binding.
    const ok = this.resumed(state, "result", [onRun, onStep, onAttempt], names, sourceMapContext, owners)
    const scope = new Map(ok.scope)
    if (node.bind !== undefined) scope.set(node.bind, { zen: `event.results.${node.step}`, schema: node.fn.output })
    this.proceed({ ...ok, scope }, next)
    if (node.fn.errors.length === 0) return
    const willRetry = policy?.recurs !== undefined && attempt <= policy.recurs
    if (willRetry) {
      const nextAttempt = attempt + 1
      const retrying = this.retryingState(node, nextAttempt)
      for (const decl of node.fn.errors) {
        const path = this.resumed(state, "failure", [onRun, onStep, onAttempt, `event.failure._tag == '${decl.tag}'`], names, sourceMapContext, owners)
        const timer = `{kind: 'timer', runId: ${path.key}, step: '${node.step}', attempt: ${nextAttempt}, timerId: ${this.retryTimerKey(path.key, node.step, nextAttempt)}, afterMillis: ${retryDelay(policy!, attempt)}}`
        this.emit(path, retrying, [], [timer], `retry ${node.step} after attempt ${attempt}`, node.at)
      }
      this.resume(retrying, node.at, () => {
        const timerKey = this.retryTimerKey("slice.run.run_id", node.step, nextAttempt)
        const path = this.resumed(
          retrying,
          "timer",
          [onRun, onStep, `event.attempt == ${nextAttempt}`, `event.timerId == ${timerKey}`],
          names,
          sourceMapContext,
          owners,
        )
        const state = this.awaitingState(node, nextAttempt, policy)
        this.emit(path, state, [this.command(path.key, node, String(nextAttempt), this.storedInput(node, path))], [], `perform ${node.fn.id} again`, node.at, undefined, owners)
        this.resume(state, node.at, () => this.awaiting(node, next, policy, names, nextAttempt, sourceMapContext, owners))
      })
    }
    for (const decl of node.fn.errors) {
      if (!willRetry) {
        const path = this.resumed(
          state,
          "failure",
          [onRun, onStep, onAttempt, `event.failure._tag == '${decl.tag}'`],
          names,
          sourceMapContext,
          owners,
        )
        this.raise(path, next, decl.tag, { zen: "event.failure", schema: this.failureRecord(decl) }, node.at)
      }
    }
  }

  /** The perform input again, for a retry: rendered over the stored bindings. */
  private storedInput(node: Perform, path: Path): string {
    return this.expr(node.at, () => this.rendered(node.input, path.scope)) ?? "null"
  }

  /** Route a typed failure to the nearest enclosing catchTag, or end the Run in `failed_<tag>`. */
  private raise(path: Path, cont: Cont, tag: string, failure: Binding, at: SourceLocation | undefined): void {
    for (let i = cont.length - 1; i >= 0; i--) {
      const frame = cont[i]!
      if (frame.kind !== "catch" || frame.node.tag !== tag) continue
      const below = cont.slice(0, i)
      // Bindings of the abandoned body go out of scope; the handler sees the scope at catchTag.
      const kept = this.scopeAt(below, path.scope)
      const scope = new Map(kept)
      if (frame.node.as !== undefined) scope.set(frame.node.as, failure)
      const sourceMapContext = this.withSourceMap(frame.sourceMapContext, frame.node.sourceMap?.id)
      this.proceed(
        { ...path, scope, sourceMapContext },
        [...below, { kind: "rest", steps: frame.node.handler, index: 0, outer: new Set(kept.keys()), sourceMapContext: frame.sourceMapContext }],
      )
      return
    }
    const state = `failed_${snake(tag)}`
    const value = "zen" in failure ? failure.zen : this.expr(at, () => this.rendered(failure.inline, failure.scope))
    if (value === undefined) return
    this.uncaught = this.merge(this.uncaught, failure.schema, at, "uncaught failure")
    this.emit(path, state, [], [], `fail ${tag}`, at, `{failure: ${value}}`)
    this.terminal.add(state)
  }

  /** The bindings visible at the point the frames `below` resume: those of the innermost enclosing block. */
  private scopeAt(below: Cont, scope: Scope): Scope {
    const rest = below[below.length - 1]
    if (!rest || rest.kind !== "rest") return scope
    return new Map([...scope].filter(([name]) => rest.outer.has(name) || this.declaredBefore(rest, name)))
  }

  /** Whether `name` is bound by a step of `frame` before its resume index (a binding of that block). */
  private declaredBefore(frame: Extract<Frame, { kind: "rest" }>, name: string): boolean {
    return frame.steps.slice(0, frame.index).some(
      (s) => (s._tag === "Perform" && s.bind === name) || (s._tag === "Retry" && s.perform.bind === name) || (s._tag === "Compose" && s.name === name) || (s._tag === "All" && this.declaredIn(s.steps, name)),
    )
  }

  private declaredIn(steps: ReadonlyArray<Step>, name: string): boolean {
    return steps.some((step) =>
      (step._tag === "Perform" && step.bind === name) ||
      (step._tag === "Retry" && step.perform.bind === name) ||
      (step._tag === "Compose" && step.name === name) ||
      (step._tag === "All" && this.declaredIn(step.steps, name)),
    )
  }

  private bindingsIn(steps: ReadonlyArray<Step>, outer: Iterable<string>): ReadonlySet<string> {
    const bindings = new Set(outer)
    for (const step of steps) {
      if (step._tag === "Perform" && step.bind !== undefined) bindings.add(step.bind)
      else if (step._tag === "Retry" && step.perform.bind !== undefined) bindings.add(step.perform.bind)
      else if (step._tag === "Compose") bindings.add(step.name)
      else if (step._tag === "All") for (const name of this.bindingsIn(step.steps, [])) bindings.add(name)
    }
    return bindings
  }

  private result(schema: ValueSchema, at: SourceLocation | undefined): void {
    this.resultSchema = this.merge(this.resultSchema, schema, at, "flow result")
  }

  private merge(current: ValueSchema | undefined, next: ValueSchema, at: SourceLocation | undefined, what: string): ValueSchema {
    this.budget.assert(next)
    if (!current) return next
    this.budget.assert(current)
    const merged = mergeSchemas(current, next)
    if (typeof merged === "string") {
      this.report("NORN-FLOW-RESULT", `every ${what} must share one closed shape: ${merged}`, at)
      return current
    }
    this.budget.assert(merged)
    return merged
  }

  /** A path resumed in `state` on an event, with its bindings read from the result slot. */
  private resumed(
    state: string,
    event: string,
    guards: ReadonlyArray<string>,
    names: ReadonlySet<string>,
    sourceMapContext: ReadonlyArray<string> = [],
    sourceMapOwners: ReadonlyArray<string> = [],
  ): Path {
    const scope = new Map<string, Binding>()
    for (const name of names) {
      scope.set(name, { zen: `slice.run.slots.bindings.${name}`, schema: this.bindingSchemas.get(name)! })
    }
    return {
      from: state,
      event,
      guards: [`slice.run.status == '${state}'`, `event.kind == '${event}'`, ...guards],
      scope,
      facts: [],
      key: "slice.run.run_id",
      start: false,
      sourceMapContext,
      sourceMapOwners,
    }
  }

  private resume(state: string, at: SourceLocation | undefined, lower: () => void): void {
    if (!this.states.has(state)) this.states.set(state, at)
    if (this.registered.has(state)) return
    this.registered.add(state)
    this.queue.push(lower)
  }

  /** Close a path: one decision row that moves the Run to `to`. */
  private emit(
    path: Path,
    to: string,
    commands: ReadonlyArray<string>,
    timers: ReadonlyArray<string>,
    what: string,
    at: SourceLocation | undefined,
    slots?: string,
    sourceMapOwners: ReadonlyArray<string> = [],
  ): void {
    if (this.rows.length >= FLOW_LIMITS.maxRows) {
      this.report("NORN-FLOW-UNBOUNDED", `the flow lowers to more than ${FLOW_LIMITS.maxRows} decision rows`, at)
      throw new LoweringBound()
    }
    if (!this.states.has(to)) this.states.set(to, at)
    const mappedIds = [...new Set([...path.sourceMapContext, ...path.sourceMapOwners, ...sourceMapOwners])]
    this.mapState(to, mappedIds)
    const sources = this.transitions.get(to) ?? []
    if (!sources.includes(path.from)) sources.push(path.from)
    this.transitions.set(to, sources)
    const facts = [...path.facts]
    const stored = slots ?? this.slots(path)
    if (stored !== undefined) {
      facts.push(`{op: 'set', objectType: ${zenString(this.runType)}, key: ${path.key}, property: 'slots', value: ${stored}}`)
    }
    facts.push(`{op: 'transition', objectType: ${zenString(this.runType)}, key: ${path.key}, lifecycle: 'status', transition: 'to_${to}'}`)
    const base = `${path.from}/${path.event}`
    const n = (this.rowCounts.get(base) ?? 0) + 1
    this.rowCounts.set(base, n)
    const row = {
      id: `${base}/${n}`,
      from: path.from,
      guard: path.guards.join(" and "),
      to,
      facts,
      commands,
      timers,
      description: `${path.from} on ${path.event}: ${what} → ${to}`,
      at,
    }
    this.mapRow(row.id, mappedIds)
    this.budget.addOutput(row)
    this.rows.push(row)
  }

  private withSourceMap(current: ReadonlyArray<string>, id: string | undefined): ReadonlyArray<string> {
    return id && !current.includes(id) ? [...current, id] : current
  }

  private mapState(state: string, ids: ReadonlyArray<string>): void {
    for (const id of ids) this.sourceMapLinks.get(id)?.states.add(state)
  }

  private mapRow(row: string, ids: ReadonlyArray<string>): void {
    for (const id of ids) this.sourceMapLinks.get(id)?.rows.add(row)
  }

  /** The result-slot record when the path holds a binding not yet stored, else nothing to write. */
  private slots(path: Path): string | undefined {
    const entries = [...path.scope]
    const fresh = entries.some(([name, b]) => !("zen" in b) || b.zen !== `slice.run.slots.bindings.${name}`)
    if (!fresh) return undefined
    const fields = entries.map(([name, b]) => {
      const field = `${name}: ${"zen" in b ? b.zen : this.rendered(b.inline, b.scope)}`
      this.budget.addOutput(field)
      return field
    })
    return `{bindings: {${fields.join(", ")}}}`
  }

  // ------------------------------------------------------------- artifacts

  private envelope(kind: "entity" | "rule", artifactId: string, name: string, payload: unknown, dependencies: unknown): Json {
    const artifact = envelope(
      { kind, artifactId, namespace: this.namespace, name, version: this.def.meta.version, dependencies: toJson(dependencies) as ReadonlyArray<Json> },
      payload,
      this.options,
    )
    this.budget.addOutput(artifact)
    return artifact
  }

  private get entityId(): string {
    return `${this.runType}.entity`
  }

  private slotsSchema(): ObjectSchema {
    const fields: Record<string, ValueSchema> = {}
    if (this.bindingSchemas.size > 0) {
      fields.bindings = record(Object.fromEntries(this.bindingSchemas), [...this.bindingSchemas.keys()])
    }
    if (this.resultSchema) fields.result = this.resultSchema
    if (this.uncaught) fields.failure = this.uncaught
    return record(fields, Object.keys(fields))
  }

  private entity(): Json {
    const states = [...this.states.keys()]
    const objectType = {
      name: this.runType,
      description: `Run of flow ${this.def.meta.id}@${this.def.meta.version} (generated by the norn-ts flow builder; the lifecycle is the flow's only state machine).`,
      properties: [
        { id: "run_id", type: { kind: "text" } },
        { id: "status", type: { kind: "enum", values: states.map((value) => ({ value })) } },
        { id: "slots", type: { kind: "json-ext", schema: this.slotsSchema() }, constraints: { nullable: true } },
      ],
      identity: [{ name: "primary", properties: ["run_id"] }],
      lifecycles: [
        {
          property: "status",
          initial: ["started"],
          terminal: states.filter((s) => this.terminal.has(s)),
          transitions: [...this.transitions].map(([to, from]) => ({ id: `to_${to}`, from, to })),
        },
      ],
    }
    return this.envelope("entity", this.entityId, `${this.def.meta.id} run`, { entityVersion: "2.0", objectType }, [])
  }

  private inputSchema(): ObjectSchema {
    const steps = [...this.steps.keys()]
    const event: Record<string, ValueSchema> = {
      kind: text(["start", ...(this.performs.size > 0 ? ["result"] : []), ...(this.hasFailures() ? ["failure"] : []), ...(this.hasTimers() ? ["timer"] : [])]),
      runId: text(),
    }
    if (steps.length > 0) {
      event.step = text(steps)
      event.attempt = integer
    }
    if (this.performs.size > 0) {
      event.results = record(Object.fromEntries([...this.performs].map(([s, p]) => [s, p.fn.output])), [...this.performs.keys()])
    }
    if (this.hasFailures()) event.failure = this.failureSchema()
    if (this.hasTimers()) {
      event.timerId = text()
      event.afterMillis = integer
    }
    const run = record(
      { run_id: text(), status: text([...this.states.keys()]), slots: this.slotsSchema() },
      ["slots"],
    )
    return record({
      slice: record({ run }, ["run"]),
      event: record(event, Object.keys(event).filter((k) => k !== "kind" && k !== "runId")),
    })
  }

  private hasFailures(): boolean {
    return [...this.performs.values()].some((p) => p.fn.errors.length > 0)
  }

  private hasTimers(): boolean {
    return this.sleeps.size > 0 || this.retried.size > 0
  }

  /** `{_tag, ...fields}` over every error a performed Function declares; fields of one tag are optional for the others. */
  private failureSchema(): ObjectSchema {
    const tags = [...new Set([...this.performs.values()].flatMap((p) => p.fn.errors.map((e) => e.tag)))]
    let fields: Record<string, ValueSchema> = {}
    for (const tag of tags) {
      const decl = this.errors.get(tag)!
      for (const [name, schema] of Object.entries(decl.fields.properties)) {
        const seen = fields[name]
        this.budget.assert(schema)
        if (seen) this.budget.assert(seen)
        const merged = seen ? mergeSchemas(seen, schema) : schema
        if (typeof merged === "string") {
          this.report("NORN-FLOW-TAG", `error field ${name} has incompatible types across tags: ${merged}`, decl.at)
        } else { this.budget.assert(merged); fields = { ...fields, [name]: merged } }
      }
    }
    return record({ _tag: text(tags), ...fields }, Object.keys(fields))
  }

  private outputSchema(): ObjectSchema {
    const transitions = [...this.transitions.keys()].map((to) => `to_${to}`)
    const fact = record(
      {
        op: text(["create", "set", "transition"]),
        objectType: text([this.runType]),
        key: text(),
        values: record({ run_id: text(), status: text(["started"]) }),
        property: text(["slots"]),
        value: this.slotsSchema(),
        lifecycle: text(["status"]),
        transition: text(transitions),
      },
      ["values", "property", "value", "lifecycle", "transition"],
    )
    const commands =
      this.performs.size > 0
        ? record({
            kind: text(["perform"]),
            runId: text(),
            step: text([...this.performs.keys()]),
            function: text([...new Set([...this.performs.values()].map((p) => p.fn.id))]),
            functionVersion: text(),
            idempotencyKey: text(),
            attempt: integer,
            input: record(Object.fromEntries([...this.performs].map(([s, p]) => [s, p.fn.input])), [...this.performs.keys()]),
          })
        : record({})
    const timers = this.hasTimers()
      ? record({
          kind: text(["timer"]),
          runId: text(),
          step: text([...this.steps.keys()].filter((s) => this.sleeps.has(s) || this.retried.has(s))),
          attempt: integer,
          timerId: text(),
          afterMillis: integer,
        })
      : record({})
    return record({ facts: array(fact), commands: array(commands), timers: array(timers) })
  }

  private rule(): Json {
    const cells = (items: ReadonlyArray<string>) => `[${items.join(", ")}]`
    const rules = [
      ...this.rows.map((row) => ({
        _id: row.id,
        _description: row.description,
        c_when: row.guard,
        o_facts: cells(row.facts),
        o_commands: cells(row.commands),
        o_timers: cells(row.timers),
      })),
      { _id: "otherwise", _description: "any other event (duplicate, stale, or late): no change", c_when: "", o_facts: "[]", o_commands: "[]", o_timers: "[]" },
    ]
    const graph = {
      contentType: "application/vnd.gorules.decision",
      nodes: [
        { id: "in", name: "run slice and event", type: "inputNode" },
        {
          id: "route",
          name: "durable steps",
          type: "decisionTableNode",
          content: {
            hitPolicy: "first",
            inputs: [{ id: "c_when", name: "when", type: "expression" }],
            outputs: [
              { id: "o_facts", name: "facts", field: "facts" },
              { id: "o_commands", name: "commands", field: "commands" },
              { id: "o_timers", name: "timers", field: "timers" },
            ],
            rules,
          },
        },
        { id: "out", name: "decision", type: "outputNode" },
      ],
      edges: [
        { id: "e_in", sourceId: "in", targetId: "route", type: "edge" },
        { id: "e_out", sourceId: "route", targetId: "out", type: "edge" },
      ],
    }
    const payload = {
      ruleId: `${this.def.meta.id}.flow`,
      ruleVersion: "2.0",
      contract: "decide",
      inputSchema: this.inputSchema(),
      outputSchema: this.outputSchema(),
      scopeRequired: Object.keys(this.options.scope).sort(),
      graph,
    }
    const dependencies = [{ artifactId: this.entityId, kind: "entity", version: this.def.meta.version, required: true }]
    return this.envelope("rule", `${this.def.meta.id}.flow.rule`, `${this.def.meta.id} flow`, payload, dependencies)
  }

  private sourceMap(): Json {
    const at = (where: SourceLocation | undefined) => (where ? { ...where } : null)
    const map = {
      format: "norn-flow-sourcemap/1",
      flow: this.def.meta.id,
      version: this.def.meta.version,
      sourcePath: this.options.sourcePath,
      note: "non-executable: maps generated ids to the definition sites that produced them",
      flowAt: at(this.def.at),
      steps: Object.fromEntries([...this.steps].map(([id, node]) => [id, at(node.at)])),
      states: Object.fromEntries([...this.states].map(([state, where]) => [state, at(where)])),
      rows: Object.fromEntries(this.rows.map((row) => [row.id, at(row.at)])),
    }
    this.budget.addOutput(map)
    return toJson(map)
  }

  private flowSourceMap(): FlowSourceMapDraft | undefined {
    const graph = this.sourceMapGraph
    if (!graph) return undefined
    const ruleId = `${this.def.meta.id}.flow.rule`
    const steps = graph.steps.map((step) => {
      const links = this.sourceMapLinks.get(step.id)
      const stateIds = [...(links?.states ?? [])].sort()
      const decisionRowIds = [...(links?.rows ?? [])].sort()
      const artifactIds = [
        ...(stateIds.length > 0 ? [this.entityId] : []),
        ...(decisionRowIds.length > 0 ? [ruleId] : []),
      ].sort()
      return { ...step, artifactIds, stateIds, decisionRowIds }
    })
    const map: FlowSourceMapDraft = {
      format: FLOW_SOURCE_MAP_DRAFT_FORMAT,
      flows: [{
        flowId: this.def.meta.id,
        flowVersion: this.def.meta.version,
        sourcePath: this.options.sourcePath,
        artifactRefs: [],
        steps,
        edges: graph.edges,
      }],
    }
    this.budget.addOutput(map)
    return toJson(map) as unknown as FlowSourceMapDraft
  }
}

/** Milliseconds to wait after failed attempt `attempt` (1-based). */
export function retryDelay(policy: RetryPolicy, attempt: number): number {
  const delay = policy.delay
  if (!delay) return 0
  if (delay.kind === "spaced") return delay.millis
  if (delay.kind === "max") return Math.max(...delay.delays.map((item) => retryDelay({ delay: item }, attempt)))
  return delay.baseMillis * delay.factor ** (attempt - 1)
}

/** Why `actual` does not conform to the closed `expected` schema, or undefined. */
export function conforms(actual: ValueSchema, expected: ValueSchema, path = "$"): string | undefined {
  if (expected.type === "number" && (actual.type === "integer" || actual.type === "number")) return undefined
  if (expected.type !== actual.type) return `${path} is ${actual.type}, expected ${expected.type}`
  if (expected.type === "array" && actual.type === "array") return conforms(actual.items, expected.items, `${path}[]`)
  if (expected.type === "object" && actual.type === "object") {
    for (const [name, schema] of Object.entries(actual.properties)) {
      const want = expected.properties[name]
      if (!want) return `${path}.${name} is not declared`
      const problem = conforms(schema, want, `${path}.${name}`)
      if (problem) return problem
    }
    for (const name of expected.required ?? []) {
      if (!(name in actual.properties)) return `${path}.${name} is required`
    }
  }
  return undefined
}

/** Render a JSON value as ZEN (re-exported for builder clients). */
export { zenJson }
