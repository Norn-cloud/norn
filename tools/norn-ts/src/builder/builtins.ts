/**
 * Typed handles of the engine's host built-in Functions (FUNCTION-HOST-1,
 * Core 07). The Rust crate `norn-builtins` owns each contract; these handles
 * restate its schema documents for `perform`, and `test/builtins.test.ts`
 * fails if they drift from `crates/norn-builtins/src/schemas`.
 */
import * as S from "./schema.ts"
import { fn, taggedError, type FnRef } from "./flow.ts"

const at = { file: "tools/norn-ts/src/builder/builtins.ts", line: 1 }

/** One decision option: `id` is `[a-z][a-z0-9_]*`; its effect names exactly one of an action or a binding. */
const OPTION = S.record({
  id: S.text(),
  label: S.text(),
  effect: S.record({ action: S.text(), binding: S.text() }, ["action", "binding"]),
  scope: S.text(),
})

/**
 * `norn.decision@1`: ask a person (D58). Lower it as `perform` followed by `route` on `choice`.
 * Only the cell resolves it, by issuing the receipt for a verified decider.
 * The schemas cannot carry these bounds, so the cell enforces them: question and note at most 500
 * characters, one to nine options, unique option ids, `agentPick` naming an option, `expiresInMs` at
 * most seven days, and `choice` naming an option.
 */
export const DECISION_FN: FnRef = fn({
  id: "norn.decision",
  version: "1",
  input: S.record(
    {
      question: S.text(),
      risk: S.text(["low", "medium", "high"]),
      options: S.array(OPTION),
      agentPick: S.text(),
      decider: S.record({ roles: S.array(S.text()), law: S.text() }, ["law"]),
      expiresInMs: S.integer,
    },
    ["agentPick", "expiresInMs"],
  ),
  output: S.record({ choice: S.text(), note: S.text() }, ["note"]),
  errors: [taggedError("Expired", S.record({}), { at }), taggedError("Withdrawn", S.record({}), { at })],
  at,
})

/** Optional provenance trace context forwarded by `norn.action@1` steps. */
const TRACE_CONTEXT = S.record(
  {
    traceId: S.text(),
    intentRef: S.text(),
    sessionDigest: S.text(),
  },
  ["intentRef", "sessionDigest"],
)

/** Supported explicit pin fields on `norn.action@1` steps (STEP-PIN-1). */
export type ActionStepPinField = "artifactId" | "version" | "digest" | "definitionsVersion"

const ACTION_STEP_PIN_FIELDS: ReadonlyMap<ActionStepPinField, S.ValueSchema> = new Map<
  ActionStepPinField,
  S.ValueSchema
>([
  ["artifactId", S.text()],
  ["version", S.text()],
  ["digest", S.text()],
  ["definitionsVersion", S.integer],
])

function actionStepPinSchema(fields: ReadonlyArray<ActionStepPinField>): S.ObjectSchema {
  if (fields.length === 0) {
    throw new Error("actionFn pinFields must declare at least one pin field (empty pin `{}` is rejected by norn.action@1)")
  }
  const properties: Record<string, S.ValueSchema> = {}
  for (const field of fields) {
    const schema = ACTION_STEP_PIN_FIELDS.get(field)
    if (schema === undefined) {
      throw new Error(`unknown actionFn pin field: ${String(field)}`)
    }
    properties[field] = schema
  }
  return S.record(properties)
}

/**
 * `norn.action@1` (ACTION-STEP-1): a step that calls the governed action `action` with an input of
 * `input`'s shape (the action's own input record) and optional `trace` context (and optional `pin`
 * when `options.pinFields` declares the required pin fields; every declared pin field is required
 * in the closed schema so empty `pin: {}` is rejected statically, matching `action.input.json`).
 * The cell executes it through the ordinary action path, under the run starter's authority for the
 * whole run; a person who decided an earlier step lends it nothing. Its output is the action's
 * target. Its typed failures are `Denied` (the starter may not call the action) and `Refused` (any
 * other refusal, with its `code`). One handle per action: the input schema is the action's, the
 * Function is the same.
 */
export function actionFn(
  action: string,
  input: S.ObjectSchema,
  options?: { readonly pinFields?: readonly [ActionStepPinField, ...ActionStepPinField[]] },
): FnRef {
  const stepInput = options?.pinFields !== undefined
    ? S.record(
        { action: S.text([action]), input, trace: TRACE_CONTEXT, pin: actionStepPinSchema(options.pinFields) },
        ["trace", "pin"],
      )
    : S.record({ action: S.text([action]), input, trace: TRACE_CONTEXT }, ["trace"])
  return fn({
    id: "norn.action",
    version: "1",
    input: stepInput,
    output: S.record({ objectType: S.text(), objectId: S.text(), version: S.integer }),
    errors: [taggedError("Denied", S.record({}), { at }), taggedError("Refused", S.record({ code: S.text() }, ["code"]), { at })],
    at,
  })
}

/** Supported explicit pin fields on `norn.question@1` steps (STEP-PIN-1). */
export type QuestionStepPinField = ActionStepPinField

/** Supported monotonic freshness guard fields on `norn.question@1` steps (QUERY-FRESH-1). */
export type QuestionFreshnessField = "maxAgeMs" | "requireExact"

const QUESTION_FRESHNESS_FIELDS: ReadonlyMap<QuestionFreshnessField, S.ValueSchema> = new Map<
  QuestionFreshnessField,
  S.ValueSchema
>([
  ["maxAgeMs", S.integer],
  ["requireExact", S.boolean],
])

function questionStepPinSchema(fields: ReadonlyArray<QuestionStepPinField>): S.ObjectSchema {
  if (fields.length === 0) {
    throw new Error("questionFn pinFields must declare at least one pin field (empty pin `{}` is rejected by norn.question@1)")
  }
  const properties: Record<string, S.ValueSchema> = {}
  for (const field of fields) {
    const schema = ACTION_STEP_PIN_FIELDS.get(field)
    if (schema === undefined) {
      throw new Error(`unknown questionFn pin field: ${String(field)}`)
    }
    properties[field] = schema
  }
  return S.record(properties)
}

function questionFreshnessSchema(fields: ReadonlyArray<QuestionFreshnessField>): S.ObjectSchema {
  if (fields.length === 0) {
    throw new Error("questionFn freshnessFields must declare at least one freshness field (empty freshness `{}` is rejected by norn.question@1)")
  }
  const properties: Record<string, S.ValueSchema> = {}
  for (const field of fields) {
    const schema = QUESTION_FRESHNESS_FIELDS.get(field)
    if (schema === undefined) {
      throw new Error(`unknown questionFn freshness field: ${String(field)}`)
    }
    properties[field] = schema
  }
  return S.record(properties)
}

const QUESTION_REVISION_SCHEMA = S.record({
  seq: S.integer,
  hash: S.text(),
  commits: S.integer,
})

const QUESTION_ROW_SCHEMA = S.record({
  bucketKey: S.text(),
  value: S.nullable(S.text()),
  totals: S.array(S.text()),
  memberCount: S.integer,
  suppressed: S.boolean,
})

/** Closed `Answered` output schema matching `crates/norn-builtins/src/schemas/question.output.json`. */
const QUESTION_OUTPUT_SCHEMA = S.record({
  status: S.text(["Answered"]),
  question: S.text(),
  grain: S.text(),
  engine: S.text(["cell", "ivm", "duckdb", "datafusion"]),
  freshness: S.text(["exact", "fresh"]),
  complete: S.boolean,
  queryPin: S.text(),
  definitionPin: S.text(),
  authorityPin: S.text(),
  readToken: S.text(),
  revision: QUESTION_REVISION_SCHEMA,
  effectiveAsOf: S.text(),
  knownAsOf: S.integer,
  nowMs: S.integer,
  rowCount: S.integer,
  value: S.nullable(S.text()),
  rows: S.array(QUESTION_ROW_SCHEMA),
})

/** Options for constructing a typed `norn.question@1` handle. */
export interface QuestionFnOptions {
  /** Closed schema for optional parameter bindings (`params`). */
  readonly params?: S.ObjectSchema
  /** Whether the step may pass an `asOf` RFC 3339 cutoff string. */
  readonly asOf?: boolean
  /** Freshness guard fields required when `freshness` is supplied (rejects empty `{}`). */
  readonly freshnessFields?: readonly [QuestionFreshnessField, ...QuestionFreshnessField[]]
  /** Explicit STEP-PIN-1 fields required when `pin` is supplied (rejects empty `{}`). */
  readonly pinFields?: readonly [QuestionStepPinField, ...QuestionStepPinField[]]
}

/**
 * `norn.question@1` (QUESTION-STEP-1): a read-only flow step that evaluates the admitted question
 * `question` through `QuestionRuntime` under the run starter's authority and markings disclosure,
 * enforcing `STEP-PIN-1` pins and `#1905` `RequiredSources` freshness. Its output is `Answered`
 * (structured rows/value plus pins). Its typed failures are `StaleSource` (stale or incomplete
 * required sources, carrying `code`, `freshness`, `complete`, and `sources`), `Denied` (permission,
 * visibility, or markings disclosure refusal), and `Refused` (pin mismatch or other deterministic
 * refusal with `code`).
 */
function isQuestionParamScalar(schema: S.ValueSchema): boolean {
  const isScalarType = (t: string): boolean =>
    t === "string" || t === "integer" || t === "number" || t === "boolean" || t === "null"
  if (typeof schema.type === "string") return isScalarType(schema.type)
  return schema.type.length > 0 && schema.type.every(isScalarType)
}

function validateQuestionParamsSchema(params: S.ObjectSchema): S.ObjectSchema {
  if (params.type !== "object") {
    throw new Error("questionFn params schema must have type object")
  }
  const extra = params.additionalProperties as unknown
  if (extra !== false) {
    if (typeof extra !== "object" || extra === null || !isQuestionParamScalar(extra as S.ValueSchema)) {
      throw new Error("questionFn params schema must set additionalProperties to false or a scalar schema")
    }
  }
  if (params.properties !== undefined) {
    for (const [name, field] of Object.entries(params.properties)) {
      if (!isQuestionParamScalar(field)) {
        throw new Error(`questionFn params field ${JSON.stringify(name)} must have a scalar schema`)
      }
    }
  }
  return params
}

export function questionFn(question: string, options?: QuestionFnOptions): FnRef {
  const properties: Record<string, S.ValueSchema> = {
    question: S.text([question]),
  }
  const optional: string[] = []
  if (options?.params !== undefined) {
    properties.params = validateQuestionParamsSchema(options.params)
    optional.push("params")
  }
  if (options?.asOf === true) {
    properties.asOf = S.text()
    optional.push("asOf")
  }
  if (options?.freshnessFields !== undefined) {
    properties.freshness = questionFreshnessSchema(options.freshnessFields)
    optional.push("freshness")
  }
  properties.trace = TRACE_CONTEXT
  optional.push("trace")
  if (options?.pinFields !== undefined) {
    properties.pin = questionStepPinSchema(options.pinFields)
    optional.push("pin")
  }
  return fn({
    id: "norn.question",
    version: "1",
    input: S.record(properties, optional),
    output: QUESTION_OUTPUT_SCHEMA,
    errors: [
      taggedError(
        "StaleSource",
        S.record(
          {
            code: S.text(),
            freshness: S.text(["stale", "incomplete", "fresh"]),
            complete: S.boolean,
            sources: S.array(S.text()),
          },
          ["code", "freshness", "complete", "sources"],
        ),
        { at },
      ),
      taggedError("Denied", S.record({}), { at }),
      taggedError("Refused", S.record({ code: S.text() }, ["code"]), { at }),
    ],
    at,
  })
}
