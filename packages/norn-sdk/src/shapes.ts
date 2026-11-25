/**
 * The documents the client returns as typed values, as the OpenAPI contract
 * (`sdks/openapi/norn-api.openapi.json`) declares them: every required and optional member, its
 * JSON type, and every constraint the contract states on it (string bounds and patterns, enums,
 * integer minimums, closed objects, unions). `test/shapes.test.ts` derives the same description
 * from the contract and requires them equal, so a contract change these shapes do not follow fails
 * the SDK's tests. An open object ignores members it does not declare (a newer cell may add them);
 * a closed one (`additionalProperties: false`) refuses them.
 */

export type Check =
  | { readonly kind: "string"; readonly minLength?: number; readonly maxLength?: number; readonly pattern?: string }
  | { readonly kind: "boolean" }
  /** `type: integer`, `minimum: 0`, within JavaScript's exact integer range. */
  | { readonly kind: "count" }
  /** A string enum; `name` is the contract schema that declares it, `""` when it is inline. */
  | { readonly kind: "enum"; readonly name: string; readonly values: readonly string[] }
  /** An object with any members (`additionalProperties: true` or `{}`). */
  | { readonly kind: "record" }
  /** `type: [X, "null"]`. */
  | { readonly kind: "nullable"; readonly of: Check }
  /** `type: "null"`. */
  | { readonly kind: "null" }
  | { readonly kind: "array"; readonly of: Check }
  /** Exactly one member admits the value. */
  | { readonly kind: "oneOf"; readonly of: readonly Check[] }
  /** At least one member admits the value. */
  | { readonly kind: "anyOf"; readonly of: readonly Check[] }
  /** Every member admits the value. */
  | { readonly kind: "allOf"; readonly of: readonly Check[] }
  | { readonly kind: "object"; readonly shape: Shape }
  | { readonly kind: "shape"; readonly name: ShapeName }

export interface Shape {
  readonly required: Readonly<Record<string, Check>>
  readonly optional: Readonly<Record<string, Check>>
  readonly closed?: boolean
}

export type ShapeName =
  | "ActionCommit" | "TraceContext" | "ActionReceipt" | "PendingDecision"
  | "NornActionUiEffects" | "NornActionInputLink" | "NornActionSummary" | "NornActionCatalog"
  | "Refusal" | "RefusalDiagnostic" | "RefusalSpan" | "NornIrRefusalReason" | "NornIrDataQualityRuleRef" | "NornIrInvariantOwner"

const string: Check = { kind: "string" }
const boolean: Check = { kind: "boolean" }
const count: Check = { kind: "count" }
const shape = (name: ShapeName): Check => ({ kind: "shape", name })
const object = (required: Shape["required"], optional: Shape["optional"] = {}, closed = false): Check => ({ kind: "object", shape: { required, optional, closed } })
const literal = (value: string): Check => ({ kind: "enum", name: "", values: [value] })
const nullOr = (kind: "oneOf" | "anyOf", check: Check): Check => ({ kind, of: [check, { kind: "null" }] })
const TYPE_NAME = "^[a-z][a-z0-9_]*(\\.[a-z][a-z0-9_]*)*\\.[A-Z][A-Za-z0-9]*$"

export const SHAPES: Readonly<Record<ShapeName, Check>> = {
  ActionCommit: object({ commitSeq: count, receiptHash: string, recordedAtMs: count }),
  TraceContext: object(
    { traceId: { kind: "string", minLength: 1, maxLength: 256 } },
    { intentRef: { kind: "string", minLength: 1, maxLength: 512 }, sessionDigest: { kind: "string", pattern: "^sha256:[0-9a-f]{64}$" } },
    true,
  ),
  ActionReceipt: object(
    {
      objectId: string, objectType: string, version: count, receipt: shape("ActionCommit"),
      outcome: { kind: "enum", name: "ActionCallOutcome", values: ["committed", "replayed"] },
    },
    { trace: shape("TraceContext") },
  ),
  PendingDecision: object({ id: string, expiresAtMs: count }),
  NornActionUiEffects: object({ destructive: boolean, irreversible: boolean, money: { kind: "array", of: string } }),
  NornActionInputLink: object({ field: string, objectKind: string }),
  NornActionSummary: object(
    {
      actionId: string, title: string, objectKind: string, objectIdField: string, inputSchema: { kind: "record" }, uiEffects: shape("NornActionUiEffects"),
      effect: { kind: "enum", name: "NornActionEffect", values: ["upsert_input_object", "fire_transition", "mutate_parent_with_rows"] },
    },
    {
      description: { kind: "nullable", of: string },
      toState: { kind: "nullable", of: string },
      inputLinks: { kind: "array", of: shape("NornActionInputLink") },
    },
  ),
  NornActionCatalog: object({ definitionsVersion: count, items: { kind: "array", of: shape("NornActionSummary") } }),
  /** Refusal@2: `allOf` the optional host details (an object, or null) and the required envelope. */
  Refusal: {
    kind: "allOf",
    of: [
      nullOr("oneOf", object({}, { decidedAt: string, incidentId: string, restatement: string, restatementSha256: string })),
      object(
        {
          format: { kind: "enum", name: "RefusalFormat", values: ["norn/refusal@2"] },
          code: string, message: string, explanation: string, retryable: boolean,
          category: { kind: "enum", name: "RefusalCategory", values: ["visibility", "permission", "precondition", "conflict", "invalid", "limit", "internal"] },
          reason: nullOr("anyOf", shape("NornIrRefusalReason")),
        },
        { diagnostics: { kind: "array", of: shape("RefusalDiagnostic") } },
      ),
    ],
  },
  RefusalDiagnostic: object(
    { code: string, severity: string, message: string, span: nullOr("oneOf", shape("RefusalSpan")) },
    { law: { kind: "nullable", of: string } },
  ),
  RefusalSpan: object({ pointer: string }),
  NornIrRefusalReason: object(
    { law: string, subject: string, values: { kind: "record" } },
    { pin: nullOr("anyOf", shape("NornIrDataQualityRuleRef")) },
    true,
  ),
  NornIrDataQualityRuleRef: {
    kind: "oneOf",
    of: [
      object({ kind: literal("law"), artifactId: string, lawId: string, version: string }, {}, true),
      object({ kind: literal("invariant"), owner: shape("NornIrInvariantOwner"), invariantId: { kind: "string", pattern: "^[a-z][a-z0-9_]*$" }, version: string }, {}, true),
      object({ kind: literal("builtin"), id: string, version: string }, {}, true),
    ],
  },
  NornIrInvariantOwner: {
    kind: "oneOf",
    of: [
      object({ kind: literal("object"), name: { kind: "string", pattern: TYPE_NAME } }),
      object({ kind: literal("interface"), name: { kind: "string", pattern: TYPE_NAME } }),
    ],
  },
}

const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value)
const patterns = new Map<string, RegExp>()
const matches = (pattern: string, value: string): boolean => {
  let compiled = patterns.get(pattern)
  if (compiled === undefined) { compiled = new RegExp(pattern, "u"); patterns.set(pattern, compiled) }
  return compiled.test(value)
}
/** JSON Schema counts a string's length in code points. */
const length = (value: string): number => [...value].length

function admits(check: Check, value: unknown): boolean {
  switch (check.kind) {
    case "string":
      return typeof value === "string"
        && (check.minLength === undefined || length(value) >= check.minLength)
        && (check.maxLength === undefined || length(value) <= check.maxLength)
        && (check.pattern === undefined || matches(check.pattern, value))
    case "boolean": return typeof value === "boolean"
    case "count": return typeof value === "number" && Number.isSafeInteger(value) && value >= 0
    case "enum": return typeof value === "string" && check.values.includes(value)
    case "record": return isRecord(value)
    case "nullable": return value === null || admits(check.of, value)
    case "null": return value === null
    case "array": return Array.isArray(value) && value.every((item) => admits(check.of, item))
    case "oneOf": return check.of.filter((member) => admits(member, value)).length === 1
    case "anyOf": return check.of.some((member) => admits(member, value))
    case "allOf": return check.of.every((member) => admits(member, value))
    case "object": return objectAdmits(check.shape, value)
    case "shape": return admits(SHAPES[check.name], value)
  }
}

function objectAdmits({ required, optional, closed }: Shape, value: unknown): boolean {
  if (!isRecord(value)) return false
  // Own members only: `constructor`, `toString` or `__proto__` are members like any other.
  if (closed && Object.keys(value).some((member) => !Object.hasOwn(required, member) && !Object.hasOwn(optional, member))) return false
  return Object.entries(required).every(([member, check]) => Object.hasOwn(value, member) && admits(check, value[member]))
    && Object.entries(optional).every(([member, check]) => !Object.hasOwn(value, member) || admits(check, value[member]))
}

/** Whether `value` is the document `name` declares, member for member and constraint for constraint. */
export function conforms(name: ShapeName, value: unknown): boolean {
  return admits(SHAPES[name], value)
}
