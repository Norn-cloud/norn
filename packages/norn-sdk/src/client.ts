/**
 * The generic governed-action client: the catalog a cell offers this caller, and one call.
 * It knows the wire contract and nothing about any domain; `norn codegen --ts` emits the
 * `ActionMap` that types a client for one admitted definition set. Wire shapes come from the
 * generated OpenAPI projection (`Schemas[...]`), so they never drift from `norn_api`.
 *
 *   GET  /v1/actions?orgId&environmentId[&objectKind=T[&objectId=I]]
 *        200 NornActionCatalog { definitionsVersion, items }: only what this caller may call.
 *   POST /v1/actions/{actionId}?orgId&environmentId[&version=N]   (norn_api::actions)
 *        body ActionRequest { input, idempotencyKey, trace? }
 *        200 ActionReceipt; 202 PendingAction (APPROVAL-1); otherwise Refusal@2, among them
 *        409 `defs.stale` when the pinned definitions version is no longer active.
 *   A 404 means the cell does not serve the route; so does a 503 that is not a Refusal@2. A 503
 *   that is one (`action.receipts_unconfigured`, say) is that refusal.
 *   A visibility or permission refusal is the one opaque 403 `authz.denied`. The guarantee is not
 *   universal: a caller the host lets call any action and that pins a stale `version` gets 409
 *   `defs.stale` for a known action but 403 `authz.denied` for an unknown one, until action-level
 *   host authorization closes that gap.
 *
 * The client never validates input and never retries on its own. A call's idempotency key
 * belongs to one user intent: mint it once (`createIntentKey`) and send the same key on every
 * retry of that intent. It is never derived from the payload, so two deliberate identical
 * submissions stay two calls. There is no mock mode; `norn dev` serves the real routes locally.
 */
import type { components } from "./generated/norn-api.ts"
import { conforms } from "./shapes.ts"

type Wire = components["schemas"]

/** A wire document as a reader holds it: nothing in it is changed after it arrives. */
export type Immutable<T> = T extends (...args: never[]) => unknown ? T
  : T extends readonly (infer I)[] ? readonly Immutable<I>[]
  : T extends object ? { readonly [K in keyof T]: Immutable<T[K]> }
  : T

export type Fetch = (input: string, init?: RequestInit) => Promise<Response>

/** One action's typed contract, as `norn codegen --ts` emits it: exact decimals are strings. */
export interface ActionContract {
  readonly input: object
  readonly output: object
}

/** Action id → contract. Generated maps satisfy it; `UntypedActions` admits any id. */
export type ActionMapShape<M> = { readonly [A in keyof M]: ActionContract }

/** Object type id → its stored fields. */
export type ObjectMapShape<O> = { readonly [T in keyof O]: object }

/** A client with no generated map: any action id, any JSON object input. */
export type UntypedActions = { readonly [actionId: string]: { readonly input: { readonly [field: string]: unknown }; readonly output: object } }

/** The action ids of a map. */
export type ActionId<M> = keyof M & string
/** What action `A` takes. */
export type ActionInput<M, A extends keyof M> = M[A] extends ActionContract ? M[A]["input"] : never
/** The output contract action `A` declares. */
export type ActionOutput<M, A extends keyof M> = M[A] extends ActionContract ? M[A]["output"] : never
/** The stored fields of object type `T`. */
export type ObjectFields<O, T extends keyof O> = O[T]

export interface Scope { readonly orgId: string; readonly environmentId: string }

/** One catalog item, as the cell serves it. */
export type ActionSummary = Immutable<Wire["NornActionSummary"]>
export type UiEffects = Immutable<Wire["NornActionUiEffects"]>
/** A committed (or replayed) call. */
export type ActionReceipt = Immutable<Wire["ActionReceipt"]>
/** An approval-gated call: nothing is committed until a decider settles it. */
export type PendingDecision = Immutable<Wire["PendingDecision"]>
export type TraceContext = Immutable<Wire["TraceContext"]>
export type RefusalCategory = Wire["RefusalCategory"]
/** DIAG-REASON-1: what refused a call, disclosed with the caller's authority. */
export type RefusalReason = Immutable<Wire["NornIrRefusalReason"]>

/** One finding of a refusal, at its pointer into the request body (`/input/lines/0/quantity`), if any. */
export interface Diagnostic {
  readonly code: Wire["RefusalDiagnostic"]["code"]
  readonly message: Wire["RefusalDiagnostic"]["message"]
  readonly pointer: Wire["RefusalSpan"]["pointer"] | null
}

/**
 * A Refusal@2 as read: its generated members, with each finding's pointer lifted out of its span.
 * `category`, `retryable` and `explanation` are absent only when a host answered without them.
 */
export interface Refusal {
  readonly status: number
  readonly code: Wire["Refusal"]["code"]
  readonly message: Wire["Refusal"]["message"]
  readonly diagnostics: readonly Diagnostic[]
  readonly category?: RefusalCategory
  readonly retryable?: boolean
  readonly explanation?: string
  readonly reason?: RefusalReason
  readonly decidedAt?: string
  readonly incidentId?: string
}

/**
 * What the cell answered one call with. Exactly three arms; anything else throws `NornUnavailable`
 * (or `NornUnexpectedResponse`), whose `outcome` says whether the request may have been applied.
 */
export type CallResult =
  | { readonly kind: "receipt"; readonly receipt: ActionReceipt }
  | { readonly kind: "pending"; readonly decision: PendingDecision }
  | ({ readonly kind: "refused" } & Refusal)

/**
 * A catalog read. `stale` is this client's own check, not the cell's answer: the catalog was served
 * from another definitions version than the one the client pins, so none of it is trusted.
 */
export type ActionCatalog =
  | { readonly kind: "catalog"; readonly items: readonly ActionSummary[]; readonly definitionsVersion: number }
  | { readonly kind: "stale"; readonly pinned: number; readonly served: number }
  | ({ readonly kind: "refused" } & Refusal)

/**
 * The cell did not answer with a document this client can read: no receipt, decision or refusal.
 * `notServed`: the route is not served (404, or a 503 that is not a Refusal@2). `status` 0: no
 * answer arrived, or it broke off while it was read (see `cause`).
 *
 * `outcome` says what is known of this HTTP request, and of no earlier one. `not-applied` only for
 * a 404: the route does not exist, so this request did nothing. It says nothing of an earlier
 * attempt of the same intent whose answer was lost: that one may still have been applied. Otherwise it is `unknown`: the request may have reached the cell and
 * been applied before its answer was lost or garbled. Retry a write with its original idempotency
 * key, never a fresh one, so the cell replays it instead of applying it twice; a read can simply
 * be repeated.
 */
export class NornUnavailable extends Error {
  readonly status: number
  readonly notServed: boolean
  readonly outcome: "not-applied" | "unknown"
  constructor(status: number, message?: string, options?: { readonly cause?: unknown }) {
    const notApplied = status === 404
    super(message ?? (notApplied
      ? "The cell does not serve this route; nothing was read or saved."
      : `The cell's answer (${status === 0 ? "none arrived" : status}) could not be read, so the outcome is unknown: the request may have been applied. Retry a write with its original idempotency key, never a new one.`), options)
    this.name = "NornUnavailable"
    this.status = status
    this.notServed = status === 404 || status === 503
    this.outcome = notApplied ? "not-applied" : "unknown"
  }
}

/**
 * A success status (2xx) whose document is not the one the contract declares: the cell said it
 * succeeded, so for a write the outcome is unknown, never failed. Retry with the original key.
 */
export class NornUnexpectedResponse extends NornUnavailable {
  constructor(status: number, document: string) {
    super(status, `The cell answered ${status} with a document that is not ${document}, so the outcome is unknown: the request may have been applied. Retry a write with its original idempotency key, never a new one.`)
    this.name = "NornUnexpectedResponse"
  }
}

/** A fresh idempotency key for one user intent: a random UUID, well inside the cell's 256-byte bound. */
export const createIntentKey = (): string => crypto.randomUUID()

/** The parsed body, or null when the text is not JSON. */
function parsed(text: string): unknown {
  try { return JSON.parse(text) } catch { return null }
}

const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value)
const CATEGORIES: readonly RefusalCategory[] = ["visibility", "permission", "precondition", "conflict", "invalid", "limit", "internal"]

/** Read a Refusal@2 body (`format: "norn/refusal@2"`); null when the text is not one. A finding's pointer is read at `span.pointer` or `pointer`. */
export function readRefusal(status: number, text: string): Refusal | null {
  const body = parsed(text)
  // Only the cell's own document is a refusal: a gateway's or proxy's `{ code, message }` is not,
  // since the request behind it may have reached the cell and been applied.
  if (!isRecord(body) || body["format"] !== "norn/refusal@2" || typeof body["code"] !== "string") return null
  const { code, message, diagnostics, category, retryable, explanation, reason, decidedAt, incidentId } = body
  const list = Array.isArray(diagnostics)
    ? diagnostics.filter(isRecord).map((d: { code?: unknown; message?: unknown; pointer?: unknown; span?: unknown }) => ({
      code: String(d.code ?? ""), message: String(d.message ?? ""),
      pointer: isRecord(d.span) && typeof d.span["pointer"] === "string" ? d.span["pointer"] : typeof d.pointer === "string" ? d.pointer : null,
    }))
    : []
  const disclosed = isRecord(reason) && typeof reason["law"] === "string" && typeof reason["subject"] === "string" && isRecord(reason["values"]) ? reason as RefusalReason : undefined
  return {
    status,
    code,
    message: typeof message === "string" ? message : code,
    diagnostics: list,
    ...CATEGORIES.includes(category as RefusalCategory) ? { category: category as RefusalCategory } : {},
    ...typeof retryable === "boolean" ? { retryable } : {},
    ...typeof explanation === "string" ? { explanation } : {},
    ...disclosed ? { reason: disclosed } : {},
    ...typeof decidedAt === "string" && decidedAt !== "" ? { decidedAt } : {},
    ...typeof incidentId === "string" && incidentId !== "" ? { incidentId } : {},
  }
}

export interface NornClientOptions {
  /** Prefix of every route (`https://cell.example`, `/api/host`); default none. */
  readonly baseUrl?: string
  /** The org and environment every request names; never inferred. */
  readonly scope: Scope
  readonly fetch?: Fetch
  /**
   * The cell definitions version this client was built against. Each call pins it (`version=N`),
   * so a cell that has since installed other definitions refuses with 409 `defs.stale`; a catalog
   * served from another version reads as `stale`. Read it from a catalog with `pinned(version)`.
   */
  readonly definitionsVersion?: number
  /** Extra headers on every request (an `authorization` bearer). */
  readonly headers?: () => Record<string, string> | Promise<Record<string, string>>
}

export interface CallOptions {
  /** The intent's key: the same value on every retry of one intent, a new one for a new intent. */
  readonly idempotencyKey: string
  readonly signal?: AbortSignal
  /** Pin this call to a definitions version (overrides the client's). */
  readonly definitionsVersion?: number
  /** W3C-style trace context the cell records with the call. */
  readonly trace?: TraceContext
}

/** One user intent: its body and key are fixed, so `send` may be repeated after a lost answer. */
export interface Intent {
  readonly actionId: string
  readonly idempotencyKey: string
  /** The `input` member's JSON text, exactly as sent. */
  readonly input: string
  send(signal?: AbortSignal): Promise<CallResult>
}

export interface NornClient<M extends ActionMapShape<M> = UntypedActions> {
  readonly scope: Scope
  readonly definitionsVersion: number | undefined
  /** This client pinned to `version` (a catalog's `definitionsVersion`, say). */
  pinned(version: number): NornClient<M>
  /** The catalog; with `objectId`, only what can act on that record now (ACTION-CATALOG-STATE-1). */
  listActions(query?: { readonly objectKind?: string; readonly objectId?: string }, signal?: AbortSignal): Promise<ActionCatalog>
  /** Call `actionId` with typed `input` under the intent's key. */
  call<A extends ActionId<M>>(actionId: A, input: ActionInput<M, A>, options: CallOptions): Promise<CallResult>
  /** Call with `input` already encoded as JSON text (a form that keeps exact digits as typed). */
  callEncoded(actionId: ActionId<M>, input: string, options: CallOptions): Promise<CallResult>
  /** Fix one intent (body and a fresh key) to send, and resend, as one call. */
  intent<A extends ActionId<M>>(actionId: A, input: ActionInput<M, A>, idempotencyKey?: string): Intent
}

const json = (value: unknown): string => {
  const text = JSON.stringify(value)
  if (text === undefined) throw new TypeError("action input is not JSON")
  return text
}

export function createNornClient<M extends ActionMapShape<M> = UntypedActions>(options: NornClientOptions): NornClient<M> {
  const fetcher: Fetch = options.fetch ?? ((input, init) => fetch(input, init))
  const base = options.baseUrl ?? ""
  const pin = options.definitionsVersion

  async function request(path: string, query: Record<string, string>, init: RequestInit): Promise<{ status: number; text: string; ok: boolean }> {
    const search = new URLSearchParams({ orgId: options.scope.orgId, environmentId: options.scope.environmentId, ...query })
    const extra = options.headers === undefined ? {} : await options.headers()
    let response: Response
    try {
      response = await fetcher(`${base}${path}?${search}`, { ...init, headers: { ...extra, ...init.headers as Record<string, string> } })
    } catch (error) {
      // `fetch` rejects with a TypeError when no answer arrives; an abort, or a host's own error, passes through.
      if (error instanceof TypeError && !init.signal?.aborted) throw new NornUnavailable(0, undefined, { cause: error })
      throw error
    }
    let text: string
    try {
      text = await response.text()
    } catch (error) {
      // A body that stops arriving is no answer either; an abort passes through.
      if (error instanceof TypeError && !init.signal?.aborted) throw new NornUnavailable(0, undefined, { cause: error })
      throw error
    }
    return { status: response.status, ok: response.ok, text }
  }

  /** A non-2xx answer: its refusal, else the route is not served (404/503) or the cell did not answer. */
  function refusalOrThrow(status: number, text: string): Refusal {
    // A 404 is the route's absence whatever its body (`route.not_found`). Any other status is a
    // refusal only when it is a Refusal@2 document (`readRefusal`).
    const refusal = status === 404 ? null : readRefusal(status, text)
    if (refusal === null) throw new NornUnavailable(status)
    return refusal
  }

  async function send(actionId: string, input: string, call: CallOptions): Promise<CallResult> {
    const version = call.definitionsVersion ?? pin
    const trace = call.trace === undefined ? "" : `,"trace":${JSON.stringify(call.trace)}`
    const { status, ok, text } = await request(`/v1/actions/${encodeURIComponent(actionId)}`, version === undefined ? {} : { version: String(version) }, {
      method: "POST",
      headers: { accept: "application/json", "content-type": "application/json" },
      body: `{"input":${input},"idempotencyKey":${JSON.stringify(call.idempotencyKey)}${trace}}`,
      ...call.signal === undefined ? {} : { signal: call.signal },
    })
    if (status === 202) {
      const pending = parsed(text)
      const decision = isRecord(pending) ? pending["decision"] : undefined
      if (isRecord(pending) && pending["outcome"] === "pending" && conforms("PendingDecision", decision)) {
        const { id, expiresAtMs } = decision as PendingDecision
        return { kind: "pending", decision: { id, expiresAtMs } }
      }
      throw new NornUnexpectedResponse(status, "a pending decision")
    }
    if (ok) {
      const receipt = parsed(text)
      if (!conforms("ActionReceipt", receipt)) throw new NornUnexpectedResponse(status, "an action receipt")
      return { kind: "receipt", receipt: receipt as ActionReceipt }
    }
    return { kind: "refused", ...refusalOrThrow(status, text) }
  }

  return {
    scope: options.scope,
    definitionsVersion: pin,
    pinned: (version) => createNornClient<M>({ ...options, definitionsVersion: version }),
    async listActions(query = {}, signal) {
      const params: Record<string, string> = {}
      if (query.objectKind !== undefined) {
        params["objectKind"] = query.objectKind
        if (query.objectId !== undefined) params["objectId"] = query.objectId
      }
      const { status, ok, text } = await request("/v1/actions", params, { headers: { accept: "application/json" }, ...signal === undefined ? {} : { signal } })
      if (ok) {
        const document = parsed(text)
        if (!conforms("NornActionCatalog", document)) throw new NornUnexpectedResponse(status, "an action catalog")
        const catalog = document as { readonly definitionsVersion: number; readonly items: readonly ActionSummary[] }
        const served = catalog.definitionsVersion
        if (pin !== undefined && served !== pin) return { kind: "stale", pinned: pin, served }
        return { kind: "catalog", items: catalog.items, definitionsVersion: served }
      }
      return { kind: "refused", ...refusalOrThrow(status, text) }
    },
    call(actionId, input, call) {
      return send(actionId, json(input), call)
    },
    callEncoded(actionId, input, call) {
      return send(actionId, input, call)
    },
    intent(actionId, input, idempotencyKey = createIntentKey()) {
      const body = json(input)
      return {
        actionId,
        idempotencyKey,
        input: body,
        send: (signal) => send(actionId, body, { idempotencyKey, ...signal === undefined ? {} : { signal } }),
      }
    },
  }
}
