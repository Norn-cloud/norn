import { readFileSync } from "node:fs"
import type { Page, Route } from "@playwright/test"
import { ACTIONS } from "./action-fixtures.ts"
import type { ObjectCell } from "./object-cell.ts"

/**
 * A stand-in for CELL-DO's action routes (`GET /v1/actions`, `POST /v1/actions/{id}`) that
 * implements the served contract (apps/studio/src/lib/actions.ts, norn_api::actions) over the
 * object stand-in's canon.Holding population: closed-schema validation, exact decimals only as
 * their text (VAL-EXACT-1, docs/spec/core/01-values-identity.md), idempotent replay, and commits that
 * move the read snapshot. Every refusal it serves on `POST` is the served route's own document and
 * status (test/fixtures/action-call.json, which crates/norn-cell-do/tests/actions.rs pins to the
 * route): input the schema refuses is one 400 `action.invalid_input` with no field pointers, a
 * precondition refusal is one 422 with no diagnostics, and an unknown action, absent or hidden
 * target, or foreign scope is the one opaque 403. `GET /v1/actions` has no served route yet, so the
 * catalog and its scope refusal are this stand-in's own.
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
    // The cell decodes an exact decimal only from its text; a number token refuses.
    if (typeof value !== "string" || !servedDecimal(value, schema.nornType.scale ?? 18)) problems.push({ code: "action.input_invalid", message: `${name} must be an exact number, like 12.500.`, pointer: at })
    return
  }
  if (type === "number" && typeof value !== "number") return void problems.push({ code: "action.input_invalid", message: `${name} must be an exact number, like 12.500.`, pointer: at })
  if (type === "string") {
    if (typeof value !== "string") return void problems.push({ code: "action.input_invalid", message: `${name} must be text.`, pointer: at })
    if (value.trim() === "") return void problems.push({ code: "action.input_invalid", message: `${name} can't be empty.`, pointer: at })
    if (schema.enum && !schema.enum.includes(value)) problems.push({ code: "action.input_invalid", message: `${value} is not one of ${schema.enum.join(", ")}.`, pointer: at })
  }
}

/** The refusals the cell serves to a caller without the refusal-detail grant; `routeNotFound` answers a route it does not serve. */
type Served = "denied" | "invalidInput" | "precondition" | "conflict" | "routeNotFound"
const SERVED = JSON.parse(readFileSync(new URL("../test/fixtures/action-call.json", import.meta.url), "utf8")) as Record<Served, object> & { readonly status: Record<Served, number> }
/** The cell's refusal `kind`, verbatim with its status: none of them names the request. */
export const refuse = (route: Route, kind: Served) => route.fulfill({ status: SERVED.status[kind], json: SERVED[kind] })

/** One JSON number token (RFC 8259): sign, integer digits, fraction and exponent captured. */
const JSON_NUMBER = /^-?(0|[1-9]\d*)(?:\.(\d+))?(?:[eE]([+-]?\d+))?$/
/** rust_decimal's largest mantissa, 2^96 - 1. */
const MAX_MANTISSA = 2n ** 96n - 1n

/**
 * Whether the served route decodes `text` as a declared decimal of `scale`, by its own rule:
 * `norn_ir::Decimal::parse` (crates/norn-ir/src/runtime.rs) takes one JSON number token, exponent
 * included, and refuses as inexact any value outside a 96-bit mantissa with at most 28 fractional
 * digits once normalized; then `Schema::decode_action_input` (crates/norn-expr/src/schema.rs) checks it
 * through `ScaledArithmetic::rescale` (vendor/zen/core/expression/src/scaled.rs), which refuses more
 * fractional digits than `scale` and a coefficient at `scale` beyond that mantissa.
 */
function servedDecimal(text: string, scale: number): boolean {
  const token = JSON_NUMBER.exec(text)
  if (token === null) return false
  const fraction = token[2] ?? ""
  let digits = BigInt(`${token[1]}${fraction}`)
  if (digits === 0n) return true
  let exponent = Number(token[3] ?? "0") - fraction.length
  while (digits % 10n === 0n) { digits /= 10n; exponent += 1 }
  const fractional = Math.max(0, -exponent)
  // Past 29 digits of scaling no coefficient fits the mantissa, so the bound also stops a huge exponent.
  if (fractional > 28 || fractional > scale || exponent + scale > 29) return false
  return digits * 10n ** BigInt(exponent + scale) <= MAX_MANTISSA
}

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
    // A foreign scope: the call route's opaque denial; the catalog has no served route to follow.
    if (url.searchParams.get("orgId") !== "org_demo" || url.searchParams.get("environmentId") !== "analytics") return request.method() === "GET" ? route.fulfill({ status: 403, json: { code: "scope.denied", message: "Not your workspace." } }) : refuse(route, "denied")
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
    if (action === undefined) return refuse(route, "denied")
    const { input, idempotencyKey } = JSON.parse(body) as { input: Record<string, unknown>; idempotencyKey: string }
    const inputText = JSON.stringify(input)
    const known = keys.get(idempotencyKey)
    if (known) {
      if (known.actionId !== actionId || known.input !== inputText) return refuse(route, "conflict")
      return route.fulfill({ json: { ...(known.receipt as object), outcome: "replayed" } })
    }
    const problems: Problem[] = []
    check(action.inputSchema as unknown as Schema, input, "/input", problems)
    if (problems.length > 0) return refuse(route, "invalidInput")
    const id = String(input["id"])
    const index = objects.rows.findIndex((row) => row.id === id)
    const existing = objects.rows[index]
    if (action.effect === "fire_transition") {
      // An absent or hidden target is the one opaque denial, like an unknown or ungranted action.
      if (existing === undefined || existing.withheld) return refuse(route, "denied")
      // A retired holding fails the transitions' declared precondition.
      if (existing.values["retired"] === "true") return refuse(route, "precondition")
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
