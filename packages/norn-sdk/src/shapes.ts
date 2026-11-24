/**
 * The success documents the client returns as typed values, as the OpenAPI contract
 * (`sdks/openapi/norn-api.openapi.json`) declares them: each required member, each optional one,
 * and the JSON type of both. `test/shapes.test.ts` compares every shape with the contract, so a
 * contract change that these shapes do not follow fails the SDK's tests. Members the contract does
 * not declare are ignored (a newer cell may add them).
 */

export type Check =
  | { readonly kind: "string" | "boolean" | "object" }
  /** `type: integer`, `minimum: 0`, within JavaScript's exact integer range. */
  | { readonly kind: "count" }
  | { readonly kind: "enum"; readonly name: string; readonly values: readonly string[] }
  | { readonly kind: "nullable"; readonly of: Check }
  | { readonly kind: "array"; readonly of: Check }
  | { readonly kind: "shape"; readonly name: ShapeName }

export type ShapeName =
  | "ActionCommit" | "TraceContext" | "ActionReceipt" | "PendingDecision"
  | "NornActionUiEffects" | "NornActionInputLink" | "NornActionSummary" | "NornActionCatalog"

export interface Shape { readonly required: Readonly<Record<string, Check>>; readonly optional: Readonly<Record<string, Check>> }

const string: Check = { kind: "string" }
const boolean: Check = { kind: "boolean" }
const count: Check = { kind: "count" }
const object: Check = { kind: "object" }
const shape = (name: ShapeName): Check => ({ kind: "shape", name })

export const SHAPES: Readonly<Record<ShapeName, Shape>> = {
  ActionCommit: { required: { commitSeq: count, receiptHash: string, recordedAtMs: count }, optional: {} },
  TraceContext: { required: { traceId: string }, optional: { intentRef: string, sessionDigest: string } },
  ActionReceipt: {
    required: {
      objectId: string, objectType: string, version: count, receipt: shape("ActionCommit"),
      outcome: { kind: "enum", name: "ActionCallOutcome", values: ["committed", "replayed"] },
    },
    optional: { trace: shape("TraceContext") },
  },
  PendingDecision: { required: { id: string, expiresAtMs: count }, optional: {} },
  NornActionUiEffects: { required: { destructive: boolean, irreversible: boolean, money: { kind: "array", of: string } }, optional: {} },
  NornActionInputLink: { required: { field: string, objectKind: string }, optional: {} },
  NornActionSummary: {
    required: {
      actionId: string, title: string, objectKind: string, objectIdField: string, inputSchema: object, uiEffects: shape("NornActionUiEffects"),
      effect: { kind: "enum", name: "NornActionEffect", values: ["upsert_input_object", "fire_transition", "mutate_parent_with_rows"] },
    },
    optional: {
      description: { kind: "nullable", of: string },
      toState: { kind: "nullable", of: string },
      inputLinks: { kind: "array", of: shape("NornActionInputLink") },
    },
  },
  NornActionCatalog: { required: { definitionsVersion: count, items: { kind: "array", of: shape("NornActionSummary") } }, optional: {} },
}

const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value)

function admits(check: Check, value: unknown): boolean {
  switch (check.kind) {
    case "string": return typeof value === "string"
    case "boolean": return typeof value === "boolean"
    case "object": return isRecord(value)
    case "count": return typeof value === "number" && Number.isSafeInteger(value) && value >= 0
    case "enum": return typeof value === "string" && check.values.includes(value)
    case "nullable": return value === null || admits(check.of, value)
    case "array": return Array.isArray(value) && value.every((item) => admits(check.of, item))
    case "shape": return conforms(check.name, value)
  }
}

/** Whether `value` is the document `name` declares: every required member present, every member of its type. */
export function conforms(name: ShapeName, value: unknown): boolean {
  if (!isRecord(value)) return false
  const { required, optional } = SHAPES[name]
  return Object.entries(required).every(([member, check]) => member in value && admits(check, value[member]))
    && Object.entries(optional).every(([member, check]) => value[member] === undefined || admits(check, value[member]))
}
