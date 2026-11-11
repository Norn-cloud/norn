import type { Page, Route } from "@playwright/test"
import { ACTIONS } from "./action-fixtures.ts"
import type { ObjectCell } from "./object-cell.ts"

/**
 * A stand-in for CELL-DO's action routes (`GET /v1/actions`, `POST /v1/actions/{id}`) that
 * implements the served contract (apps/studio/src/lib/actions.ts, norn_api::actions) over the
 * object stand-in's canon.Holding population: closed-schema validation, exact decimals only as
 * their text (VAL-EXACT-1, docs/spec/core/01-values-identity.md), idempotent replay, and commits that
 * move the read snapshot. It refuses exactly as the served route does: input the schema refuses is
 * one 400 `action.invalid_input` with no field pointers, rule refusals carry no pointer, and an
 * unknown action, absent target or hidden target is the one opaque 403.
 */

interface Problem { code: string; message: string; pointer: string | null }
type Schema = { type?: unknown; nornType?: { kind?: string; scale?: number }; properties?: Record<string, Schema>; items?: Schema; required?: readonly string[]; enum?: readonly string[]; title?: string; additionalProperties?: boolean }

function check(schema: Schema, value: unknown, at: string, problems: Problem[]) {
  const types = Array.isArray(schema.type) ? schema.type.map(String) : [String(schema.type)]
  if (value === null) { if (!types.includes("null")) problems.push({ code: "action.input_invalid", message: "A value is required.", pointer: at }); return }
  const type = types.find((t) => t !== "null")
  const name = schema.title ?? at.split("/").pop() ?? "value"
  if (type === "object") {
    if (typeof value !== "object" || Array.isArray(value)) return void problems.push({ code: "action.input_invalid", message: `${name} must be a record.`, pointer: at })
    const record = value as Record<string, unknown>
    for (const key of schema.required ?? []) if (!(key in record)) problems.push({ code: "action.input_missing", message: `${schema.properties?.[key]?.title ?? key} is required.`, pointer: `${at}/${key}` })
    for (const [key, member] of Object.entries(record)) {
      const child = schema.properties?.[key]
      if (child === undefined) problems.push({ code: "action.input_unknown", message: `${key} is not an input of this action.`, pointer: `${at}/${key}` })
      else check(child, member, `${at}/${key}`, problems)
    }
    return
  }
  if (type === "array") {
    if (!Array.isArray(value)) return void problems.push({ code: "action.input_invalid", message: `${name} must be a list.`, pointer: at })
    if (value.length === 0) return void problems.push({ code: "action.input_invalid", message: `Add at least one to ${name}.`, pointer: at })
    value.forEach((item, i) => check(schema.items ?? {}, item, `${at}/${i}`, problems))
    return
  }
  if (type === "number" && schema.nornType?.kind === "decimal") {
    // The cell decodes an exact decimal only from its text, at most its declared scale; a number token refuses.
    const exact = typeof value === "string" ? DECIMAL.exec(value) : null
    if (exact === null || (exact[1] ?? "").length > (schema.nornType.scale ?? 18)) problems.push({ code: "action.input_invalid", message: `${name} must be an exact number, like 12.500.`, pointer: at })
    return
  }
  if (type === "number" && typeof value !== "number") return void problems.push({ code: "action.input_invalid", message: `${name} must be an exact number, like 12.500.`, pointer: at })
  if (type === "string") {
    if (typeof value !== "string") return void problems.push({ code: "action.input_invalid", message: `${name} must be text.`, pointer: at })
    if (value.trim() === "") return void problems.push({ code: "action.input_invalid", message: `${name} can't be empty.`, pointer: at })
    if (schema.enum && !schema.enum.includes(value)) problems.push({ code: "action.input_invalid", message: `${value} is not one of ${schema.enum.join(", ")}.`, pointer: at })
  }
}

/** The served route's one opaque denial (norn_api::actions::action_denied). */
const DENIED = { code: "authz.denied", category: "permission", message: "the action request is not authorized", explanation: "access to the requested action is denied" }

/** The served route's input refusal to a caller without the refusal-detail grant: no field pointers. */
const INVALID_INPUT = { format: "norn/refusal@2", retryable: false, reason: null, code: "action.invalid_input", category: "invalid", message: "the request is invalid", explanation: "diagnostic detail is unavailable under the caller's disclosure authority" }

/** The exact decimal text grammar: optional `-`, no leading zeros, an optional fraction (captured), no exponent. */
const DECIMAL = /^-?(?:0|[1-9]\d*)(?:\.(\d+))?$/

export interface ActionCell {
  /** Every call body the form sent, for assertions on what it asked. */
  readonly calls: { readonly actionId: string; readonly body: string }[]
  /** The next call fails as an outage (500, no refusal document). */
  failNext(): void
}

export async function actionCell(page: Page, objects: ObjectCell): Promise<ActionCell> {
  const calls: { actionId: string; body: string }[] = []
  const keys = new Map<string, { actionId: string; input: string; receipt: unknown }>()
  let seq = 100
  let fail = false
  await page.route("**/api/host/v1/actions**", async (route: Route) => {
    const request = route.request()
    const url = new URL(request.url())
    if (url.searchParams.get("orgId") !== "org_demo" || url.searchParams.get("environmentId") !== "analytics") return route.fulfill({ status: 403, json: { code: "scope.denied", message: "Not your workspace." } })
    if (request.method() === "GET") {
      const kind = url.searchParams.get("objectKind")
      return route.fulfill({ json: { items: ACTIONS.filter((a) => kind === null || a.objectKind === kind) } })
    }
    const actionId = decodeURIComponent(url.pathname.split("/v1/actions/")[1] ?? "")
    const body = request.postData() ?? ""
    calls.push({ actionId, body })
    if (fail) { fail = false; return route.fulfill({ status: 500, body: "upstream reset" }) }
    const action = ACTIONS.find((a) => a.actionId === actionId)
    // An unknown action is the same opaque denial as an absent or hidden target.
    if (action === undefined) return route.fulfill({ status: 403, json: DENIED })
    const { input, idempotencyKey } = JSON.parse(body) as { input: Record<string, unknown>; idempotencyKey: string }
    const inputText = JSON.stringify(input)
    const known = keys.get(idempotencyKey)
    if (known) {
      if (known.actionId !== actionId || known.input !== inputText) return route.fulfill({ status: 409, json: { code: "action.idempotency_conflict", message: "That key was already used for a different call.", diagnostics: [{ code: "action.idempotency_conflict", message: "Key reused.", pointer: "/idempotencyKey" }] } })
      return route.fulfill({ json: { ...(known.receipt as object), outcome: "replayed" } })
    }
    const problems: Problem[] = []
    check(action.inputSchema as unknown as Schema, input, "/input", problems)
    if (problems.length > 0) return route.fulfill({ status: 400, json: INVALID_INPUT })
    const id = String(input["id"])
    const index = objects.rows.findIndex((row) => row.id === id)
    const existing = objects.rows[index]
    if (action.effect === "fire_transition") {
      // An absent or hidden target is the one opaque denial, like an unknown or ungranted action.
      if (existing === undefined || existing.withheld) return route.fulfill({ status: 403, json: DENIED })
      if (existing.values["retired"] === "true") return route.fulfill({ status: 422, json: { code: "action.precondition_failed", category: "rule", message: "This holding is retired; it can't change any more.", diagnostics: [{ code: "action.precondition_failed", message: "The holding is retired.", pointer: null }] } })
    }
    const version = (existing?.version ?? 1) + (existing ? 1 : 0)
    const values = { ...(existing?.values ?? {}) }
    if (actionId === "canon.holding.open") {
      Object.assign(values, { resource_code: String(input["resource_code"]), quantity: String(input["quantity"]), unit: "g", currency: "USD", valuation_method: String(input["valuation_method"]), location: String((input["location"] as { site: string }).site), quantity_basis: "on_hand", stock_cost: "0.000000", cost_currency: "USD" })
    } else if (actionId === "canon.holding.restock") values["lots"] = String((input["lots"] as unknown[]).length)
    else if (actionId === "canon.holding.adjust") values["quantity"] = String(input["quantity"])
    else if (actionId === "canon.holding.retire") values["retired"] = "true"
    const row = { id, withheld: false, withheldProperties: [], values, version }
    if (existing) objects.rows[index] = row
    else objects.rows.push(row)
    objects.commit()
    seq += 1
    const receipt = { outcome: "committed", objectType: "canon.Holding", objectId: id, version, receipt: { receiptHash: seq.toString(16).padStart(64, "c"), commitSeq: seq, recordedAtMs: Date.now() } }
    keys.set(idempotencyKey, { actionId, input: inputText, receipt })
    return route.fulfill({ json: receipt })
  })
  return { calls, failNext: () => { fail = true } }
}
