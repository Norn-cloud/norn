/**
 * The generic governed-action client: the catalog a cell offers this caller, and one call.
 * It knows the wire contract and nothing about any domain; `norn codegen --ts` emits the
 * `ActionMap` that types a client for one admitted definition set.
 *
 *   GET  /v1/actions?orgId&environmentId[&objectKind=T[&objectId=I]]
 *        200 { definitionsVersion, items: ActionSummary[] }: only what this caller may call.
 *   POST /v1/actions/{actionId}?orgId&environmentId[&version=N]   (norn_api::actions)
 *        body { input, idempotencyKey }
 *        200 { outcome: "committed" | "replayed", objectType, objectId, version, receipt }
 *        202 { outcome: "pending", decision: { id, expiresAtMs } }: approval-gated (APPROVAL-1)
 *        4xx Refusal@2 { code, category, message, explanation?, diagnostics? }; a pinned
 *            definitions version that is no longer active is 409 `defs.stale`.
 *   A 404 or 503 means the cell does not serve the route.
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

export interface UiEffects { readonly irreversible: boolean; readonly destructive: boolean; readonly money: readonly string[] }

/** One catalog item, as the cell serves it. */
export interface ActionSummary {
  readonly actionId: string
  readonly title: string
  readonly description: string | null
  readonly objectKind: string
  /** `mutate_parent_with_rows` writes a parent and its owned rows; `objectIdField` is the parent's id. */
  readonly effect: "upsert_input_object" | "mutate_parent_with_rows" | "fire_transition"
  readonly objectIdField: string
  readonly inputSchema: unknown
  /** The kernel's effective effects: destructive can be true without being declared. */
  readonly uiEffects: UiEffects
  /** Input fields that name an existing record of `objectKind` (ACTION-CATALOG-LINKS-1). */
  readonly inputLinks?: readonly { readonly field: string; readonly objectKind: string }[]
  /** A transition's target state. */
  readonly toState?: string
}

export interface ActionReceipt {
  readonly outcome: "committed" | "replayed"
  readonly objectType: string
  readonly objectId: string
  readonly version: number
  readonly receipt: { readonly receiptHash: string; readonly commitSeq: number; readonly recordedAtMs: number }
}

/** An approval-gated call (APPROVAL-1): nothing is committed until a decider settles it. */
export interface PendingDecision { readonly id: string; readonly expiresAtMs: number }

/** One finding of a refusal, at its pointer into the request body (`/input/lines/0/sku`), if any. */
export interface Diagnostic { readonly code: string; readonly message: string; readonly pointer: string | null }

/**
 * DIAG-REASON-1: what refused a call, disclosed with the caller's authority: the law, the subject,
 * and the stored values it checked, by path.
 */
export interface RefusalReason {
  readonly law: string
  readonly subject: string
  readonly values: Readonly<Record<string, unknown>>
  readonly pin?: { readonly kind?: string; readonly artifactId?: string; readonly lawId?: string; readonly version?: string }
}

export interface Refusal {
  readonly status: number
  readonly code: string
  readonly message: string
  readonly diagnostics: readonly Diagnostic[]
  readonly explanation?: string
  readonly reason?: RefusalReason
  readonly decidedAt?: string
  readonly incidentId?: string
}

/** What the cell answered one call with. Exactly three arms; anything else throws `NornUnavailable`. */
export type CallResult =
  | { readonly kind: "receipt"; readonly receipt: ActionReceipt }
  | { readonly kind: "pending"; readonly decision: PendingDecision }
  | ({ readonly kind: "refused" } & Refusal)

export type ActionCatalog =
  | { readonly kind: "catalog"; readonly items: readonly ActionSummary[]; readonly definitionsVersion: number | null }
  | ({ readonly kind: "refused" } & Refusal)

/**
 * The cell answered with neither a decision nor a refusal it documents, so nothing was read or
 * saved. `notServed`: a 404 or 503, the cell does not serve the route.
 */
export class NornUnavailable extends Error {
  readonly notServed: boolean
  constructor(readonly status: number, message?: string) {
    super(message ?? `The cell did not answer (${status || "network"}); nothing was read or saved.`)
    this.name = "NornUnavailable"
    this.notServed = status === 404 || status === 503
  }
}

/** A fresh idempotency key for one user intent: a random UUID, well inside the cell's 256-byte bound. */
export const createIntentKey = (): string => crypto.randomUUID()

/** Read a Refusal@2 body; null when the text is not one. A finding's pointer is read at `span.pointer` or `pointer`. */
export function readRefusal(status: number, text: string): Refusal | null {
  const body = parsed(text)
  const { code, message, diagnostics, explanation, reason, decidedAt, incidentId } = (body ?? {}) as Record<string, unknown>
  if (typeof code !== "string") return null
  const list = Array.isArray(diagnostics)
    ? diagnostics.map((d: { code?: unknown; message?: unknown; pointer?: unknown; span?: { pointer?: unknown } | null }) => ({
      code: String(d.code ?? ""), message: String(d.message ?? ""),
      pointer: typeof d.span?.pointer === "string" ? d.span.pointer : typeof d.pointer === "string" ? d.pointer : null,
    }))
    : []
  const disclosed = reason !== null && typeof reason === "object" && typeof (reason as { law?: unknown }).law === "string" ? reason as RefusalReason : undefined
  return {
    status,
    code,
    message: typeof message === "string" ? message : code,
    diagnostics: list,
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
   * read from another version is refused the same way, before any item is trusted.
   */
  readonly definitionsVersion?: number
  /** Extra headers on every request (an `authorization` bearer). */
  readonly headers?: () => Record<string, string> | Promise<Record<string, string>>
}

export interface CallOptions {
  /** The intent's key: the same value on every retry of one intent, a new one for a new intent. */
  readonly idempotencyKey: string
  readonly signal?: AbortSignal
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
  /** The catalog; with `objectId`, only what can act on that record now (ACTION-CATALOG-STATE-1). */
  listActions(query?: { readonly objectKind?: string; readonly objectId?: string }, signal?: AbortSignal): Promise<ActionCatalog>
  /** Call `actionId` with typed `input` under the intent's key. */
  call<A extends ActionId<M>>(actionId: A, input: ActionInput<M, A>, options: CallOptions): Promise<CallResult>
  /** Call with `input` already encoded as JSON text (a form that keeps exact digits as typed). */
  callEncoded(actionId: ActionId<M>, input: string, options: CallOptions): Promise<CallResult>
  /** Fix one intent (body and a fresh key) to send, and resend, as one call. */
  intent<A extends ActionId<M>>(actionId: A, input: ActionInput<M, A>, idempotencyKey?: string): Intent
}

/** The parsed body, or null when the text is not JSON. */
function parsed(text: string): unknown {
  try { return JSON.parse(text) } catch { return null }
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
    const response = await fetcher(`${base}${path}?${search}`, { ...init, headers: { ...extra, ...init.headers as Record<string, string> } })
    return { status: response.status, ok: response.ok, text: await response.text() }
  }

  function stale(found: number | null): ActionCatalog {
    return {
      kind: "refused", status: 409, code: "defs.stale", diagnostics: [],
      message: `the client pins definitions version ${String(pin)}, the cell serves ${found === null ? "an unversioned catalog" : String(found)}`,
    }
  }

  async function send(actionId: string, input: string, call: CallOptions): Promise<CallResult> {
    const { status, ok, text } = await request(`/v1/actions/${encodeURIComponent(actionId)}`, pin === undefined ? {} : { version: String(pin) }, {
      method: "POST",
      headers: { accept: "application/json", "content-type": "application/json" },
      body: `{"input":${input},"idempotencyKey":${JSON.stringify(call.idempotencyKey)}}`,
      ...call.signal === undefined ? {} : { signal: call.signal },
    })
    if (status === 202) {
      const pending = parsed(text) as { outcome?: unknown; decision?: { id?: unknown; expiresAtMs?: unknown } } | null
      const decision = pending?.decision
      if (pending?.outcome === "pending" && typeof decision?.id === "string" && typeof decision.expiresAtMs === "number") {
        return { kind: "pending", decision: { id: decision.id, expiresAtMs: decision.expiresAtMs } }
      }
      throw new NornUnavailable(status, "The cell answered 202 without a pending decision; nothing was saved.")
    }
    if (ok) {
      const receipt = parsed(text)
      if (receipt === null || typeof receipt !== "object") throw new NornUnavailable(status, "The cell's receipt was not a document.")
      return { kind: "receipt", receipt: receipt as ActionReceipt }
    }
    if (status === 404 || status === 503) throw new NornUnavailable(status)
    const refusal = readRefusal(status, text)
    if (refusal === null) throw new NornUnavailable(status)
    return { kind: "refused", ...refusal }
  }

  return {
    scope: options.scope,
    definitionsVersion: pin,
    async listActions(query = {}, signal) {
      const params: Record<string, string> = {}
      if (query.objectKind !== undefined) {
        params["objectKind"] = query.objectKind
        if (query.objectId !== undefined) params["objectId"] = query.objectId
      }
      const { status, ok, text } = await request("/v1/actions", params, { headers: { accept: "application/json" }, ...signal === undefined ? {} : { signal } })
      if (status === 404 || status === 503) throw new NornUnavailable(status)
      if (ok) {
        const document = parsed(text) as { items?: unknown; definitionsVersion?: unknown } | null
        if (!Array.isArray(document?.items)) throw new NornUnavailable(status, "The cell's action catalog was not a list.")
        const served = typeof document.definitionsVersion === "number" ? document.definitionsVersion : null
        if (pin !== undefined && served !== pin) return stale(served)
        return { kind: "catalog", items: document.items as ActionSummary[], definitionsVersion: served }
      }
      const refusal = readRefusal(status, text)
      if (refusal === null) throw new NornUnavailable(status)
      return { kind: "refused", ...refusal }
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
