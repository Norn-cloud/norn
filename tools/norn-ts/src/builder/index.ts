/**
 * The closure-free Norn data builder (T7-FLOW-1): the single lowering target
 * of hand-written definitions and of the syntax compiler (ruling 2026-09-27
 * authoring-effect-profile §2).
 */
export * as S from "./schema.ts"
export * as X from "./expr.ts"
export * from "./flow.ts"
export { type BuildOptions, type BuildResult, FLOW_LIMITS, conforms, lowerFlow, retryDelay } from "./lower-flow.ts"
export type { BuildDiagnostic, SourceLocation } from "./diagnostics.ts"
export * as E from "./entity.ts"
export { type EnvelopeOptions, envelope } from "./envelope.ts"
export * as A from "./action.ts"
export * as P from "./access.ts"
export * as R from "./rule.ts"
export * as M from "./mapping.ts"

export * as L from "./law.ts"
export * as C from "./connector.ts"

export * as SS from "./source-schema.ts"

export * as RU from "./ruling.ts"
