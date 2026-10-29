/**
 * The closure-free flow data builder (T7-FLOW-1, #2649).
 *
 * The lowering target of ruling 2026-09-27 authoring-effect-profile §2: plain,
 * first-order data constructors for the sequential flow subset of §3. Both a
 * hand-written definition and the syntax compiler (T7-EFX-2) produce these
 * nodes; {@link lowerFlow} (in `lower-flow.ts`) turns them into ordinary
 * artifacts. No constructor takes or returns a function, proxy, or Effect
 * value, and nothing here runs a flow: a definition is data from the start.
 *
 * | Constructor | Effect syntax it stands for |
 * |---|---|
 * | {@link flow} | `Norn.flow(meta, Effect.gen(function* () { … }))` |
 * | {@link fn} | `Norn.fn(...)` — a versioned Function handle with I/O schemas and declared errors |
 * | {@link taggedError} | `class X extends Data.TaggedError("X")<{ … }> {}` |
 * | {@link perform} | `const x = yield* Norn.perform("step", fn, input)` |
 * | {@link retry} | `.pipe(Effect.retry(Schedule.max([Schedule.spaced(d), Schedule.recurs(n)])))` around one perform |
 * | {@link sleep} | `yield* Effect.sleep(d)` |
 * | {@link route} | `if`/`else if`/`else` and `switch` (first hit) |
 * | {@link catchTag} | `.pipe(Effect.catchTag("Tag", (e) => …))` |
 * | {@link fail} | `yield* Effect.fail(new X({ … }))` |
 * | {@link succeed} | `return value` / `Effect.succeed(value)` at the end of the flow |
 */
import { type SourceLocation, located } from "./diagnostics.ts"
import type { Expr } from "./expr.ts"
import type { ObjectSchema, ValueSchema } from "./schema.ts"
import type { FlowSourceMapAnnotation } from "../flow-source-map.ts"

/** A registered typed error: `{ _tag, ...fields }` on the wire, never an Error instance. */
export interface TaggedErrorDecl {
  readonly _tag: "TaggedError"
  readonly tag: string
  readonly fields: ObjectSchema
  readonly at?: SourceLocation
}

/** A generated, versioned Function handle (compute-boundary ruling §2). */
export interface FnRef {
  readonly _tag: "Fn"
  readonly id: string
  readonly version: string
  readonly input: ObjectSchema
  readonly output: ValueSchema
  readonly errors: ReadonlyArray<TaggedErrorDecl>
  readonly at?: SourceLocation
}

export type Duration = { readonly millis: number }

export type RetryDelay =
  | { readonly kind: "spaced"; readonly millis: number }
  | { readonly kind: "exponential"; readonly baseMillis: number; readonly factor: number }
  | { readonly kind: "max"; readonly delays: ReadonlyArray<RetryDelay> }

/** A finite retry policy: `recurs` retries (so `recurs + 1` attempts) and an optional delay. */
export interface RetryPolicy {
  readonly recurs?: number
  readonly delay?: RetryDelay
}

export type Step = Perform | Retry | Sleep | Route | CatchTag | Fail | Succeed | Compose | Value | All

/** Compiler metadata consumed only by the non-executable authoring projection. */
export interface StepSourceMap {
  readonly sourceMap?: FlowSourceMapAnnotation
}

/** A scoped, first-order expression sequence. Value returns to this sequence's caller. */
export interface Compose extends StepSourceMap {
  readonly _tag: "Compose"
  readonly name: string
  readonly schema: ValueSchema
  readonly body: ReadonlyArray<Step>
  readonly at?: SourceLocation
}

/** Return pure data from the nearest Compose; never carries a callback. */
export interface Value extends StepSourceMap {
  readonly _tag: "Value"
  readonly value: Expr
  readonly at?: SourceLocation
}

export interface Perform extends StepSourceMap {
  readonly _tag: "Perform"
  readonly step: string
  readonly fn: FnRef
  readonly input: Expr
  readonly bind?: string
  readonly at?: SourceLocation
}

export interface Retry extends StepSourceMap {
  readonly _tag: "Retry"
  readonly perform: Perform
  readonly policy: RetryPolicy
  readonly at?: SourceLocation
}

export interface Sleep extends StepSourceMap {
  readonly _tag: "Sleep"
  readonly step: string
  readonly millis: number
  readonly at?: SourceLocation
}

export interface Route extends StepSourceMap {
  readonly _tag: "Route"
  readonly branches: ReadonlyArray<{ readonly when: Expr; readonly then: ReadonlyArray<Step> }>
  readonly otherwise: ReadonlyArray<Step>
  readonly at?: SourceLocation
}

export interface CatchTag extends StepSourceMap {
  readonly _tag: "CatchTag"
  readonly body: ReadonlyArray<Step>
  readonly tag: string
  /** The handler parameter: the failure record `{ _tag, ...fields }`. */
  readonly as?: string
  readonly handler: ReadonlyArray<Step>
  readonly at?: SourceLocation
}

export interface Fail extends StepSourceMap {
  readonly _tag: "Fail"
  readonly error: TaggedErrorDecl
  readonly fields: Expr
  readonly at?: SourceLocation
}

export interface Succeed extends StepSourceMap {
  readonly _tag: "Succeed"
  readonly value: Expr
  readonly at?: SourceLocation
}

/** Ordered children from a literal `Effect.all`; no parallel runtime is introduced. */
export interface All extends StepSourceMap {
  readonly _tag: "All"
  readonly steps: ReadonlyArray<Step>
  readonly at?: SourceLocation
}

export interface FlowMeta {
  /** Dotted lower-case id; the namespace is everything before the last segment. */
  readonly id: string
  readonly version: string
}

export interface FlowDef {
  readonly _tag: "Flow"
  readonly meta: FlowMeta
  readonly body: ReadonlyArray<Step>
  readonly at?: SourceLocation
}

/**
 * Optional source location: the compiler passes the span it lowered; a
 * hand-written call leaves it out and the call site is captured.
 */
type At = { readonly at?: SourceLocation | undefined }


export function taggedError(tag: string, fields: ObjectSchema, options: At = {}): TaggedErrorDecl {
  return located({ _tag: "TaggedError" as const, tag, fields }, options.at)
}

export function fn(
  meta: {
    readonly id: string
    readonly version: string
    readonly input: ObjectSchema
    readonly output: ValueSchema
    readonly errors?: ReadonlyArray<TaggedErrorDecl>
  } & At,
): FnRef {
  const { at, errors = [], ...rest } = meta
  return located({ _tag: "Fn" as const, ...rest, errors: [...errors] }, at)
}

export function perform(step: string, handle: FnRef, input: Expr, options: { readonly bind?: string } & At = {}): Perform {
  const node: Perform = { _tag: "Perform", step, fn: handle, input }
  return located(options.bind === undefined ? node : { ...node, bind: options.bind }, options.at)
}

export function retry(target: Perform, policy: RetryPolicy, options: At = {}): Retry {
  return located({ _tag: "Retry" as const, perform: target, policy }, options.at)
}

export function sleep(step: string, duration: Duration, options: At = {}): Sleep {
  return located({ _tag: "Sleep" as const, step, millis: duration.millis }, options.at)
}

export function route(
  branches: ReadonlyArray<{ readonly when: Expr; readonly then: ReadonlyArray<Step> }>,
  otherwise: ReadonlyArray<Step> = [],
  options: At = {},
): Route {
  return located({ _tag: "Route" as const, branches: branches.map((b) => ({ when: b.when, then: [...b.then] })), otherwise: [...otherwise] }, options.at)
}

export function catchTag(
  body: ReadonlyArray<Step>,
  tag: string,
  handler: ReadonlyArray<Step>,
  options: { readonly as?: string } & At = {},
): CatchTag {
  const node: CatchTag = { _tag: "CatchTag", body: [...body], tag, handler: [...handler] }
  return located(options.as === undefined ? node : { ...node, as: options.as }, options.at)
}

export function fail(error: TaggedErrorDecl, fields: Expr, options: At = {}): Fail {
  return located({ _tag: "Fail" as const, error, fields }, options.at)
}

export function succeed(value: Expr, options: At = {}): Succeed {
  return located({ _tag: "Succeed" as const, value }, options.at)
}

export function flow(meta: FlowMeta, body: ReadonlyArray<Step>, options: At = {}): FlowDef {
  return located({ _tag: "Flow" as const, meta: { id: meta.id, version: meta.version }, body: [...body] }, options.at)
}

/** Duration constructors (Effect duration literals lower to these). */
export const millis = (n: number): Duration => ({ millis: n })
export const seconds = (n: number): Duration => ({ millis: n * 1000 })
export const minutes = (n: number): Duration => ({ millis: n * 60_000 })
export const hours = (n: number): Duration => ({ millis: n * 3_600_000 })
