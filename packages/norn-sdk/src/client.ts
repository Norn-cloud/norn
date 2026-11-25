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
 *
 * Any other route of the contract is read and written through `get`, `post` and `download`,
 * typed by the generated `paths`: a route, its path parameters, query, body and success document
 * come from `norn openapi`, so a server field change is a compile error in every caller. A
 * document is read exactly (`ExactDocument`: every number is its source text). A route's
 * Refusal@2 is its `refused` arm, a 404 one included (a record the route does not hold), except
 * `route.not_found`: like a 404 that is no refusal, and a 503 that is none, the route is not
 * served (`NornUnavailable`).
 */
import type { components, paths } from "./generated/norn-api.ts"
import { parseExactJson, type ExactJson } from "./exact-json.ts"
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
/**
 * DIAG-REASON-1: what refused a call, disclosed with the caller's authority. `values` are exact:
 * every number in them is its JSON source text (a string), never a rounded binary64 `number`.
 */
export type RefusalReason = Immutable<Omit<Wire["NornIrRefusalReason"], "values"> & { readonly values: { readonly [path: string]: ExactJson } }>

/** One finding of a refusal, at its pointer into the request body (`/input/lines/0/quantity`), if any. */
export interface Diagnostic {
  readonly code: Wire["RefusalDiagnostic"]["code"]
  readonly message: Wire["RefusalDiagnostic"]["message"]
  readonly pointer: Wire["RefusalSpan"]["pointer"] | null
}

/**
 * A Refusal@2 as read: the whole envelope the contract requires (`conforms("Refusal")`), with each
 * finding's pointer lifted out of its span and a `null` reason omitted.
 */
export interface Refusal {
  readonly status: number
  readonly code: Wire["Refusal"]["code"]
  readonly message: Wire["Refusal"]["message"]
  readonly diagnostics: readonly Diagnostic[]
  readonly category: RefusalCategory
  readonly retryable: boolean
  readonly explanation: string
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

/**
 * Read a Refusal@2 body: null unless it is the whole envelope the contract requires
 * (`format: "norn/refusal@2"`, code, category, message, explanation, retryable, reason, and
 * well-formed findings), since anything less (a gateway's `{ code, message }`, a truncated body) is
 * not the cell's answer and the request behind it may have been applied.
 */
export function readRefusal(status: number, text: string): Refusal | null {
  return refusalOf(status, parsed(text), () => parseExactJson(text))
}

/** {@link readRefusal} over a body already read: `body` as `JSON.parse` reads it, `exact` the same body as `parseExactJson` does. */
function refusalOf(status: number, body: unknown, exact: () => ExactJson | undefined): Refusal | null {
  // The envelope is validated on its wire types first; only then are a disclosed reason's values
  // read exactly, every number as its source text (`"9007199254740993"`, `"12.500"`), as the cell's
  // runtime values are exact decimals. Nothing else is stringified.
  if (!conforms("Refusal", body)) return null
  const wire = body as Wire["Refusal"]
  const values = wire.reason === null ? undefined : (exact() as { reason?: { values?: ExactJson } } | undefined)?.reason?.values
  if (wire.reason !== null && (values === undefined || values === null || typeof values !== "object" || Array.isArray(values))) return null
  const refusal = wire.reason === null ? wire : { ...wire, reason: { ...wire.reason, values: values as { readonly [path: string]: ExactJson } } }
  const diagnostics = (refusal.diagnostics ?? []).map((d) => ({ code: d.code, message: d.message, pointer: d.span?.pointer ?? null }))
  return {
    status,
    code: refusal.code,
    message: refusal.message,
    diagnostics,
    category: refusal.category,
    retryable: refusal.retryable,
    explanation: refusal.explanation,
    ...refusal.reason === null ? {} : { reason: refusal.reason as RefusalReason },
    ...typeof refusal.decidedAt === "string" && refusal.decidedAt !== "" ? { decidedAt: refusal.decidedAt } : {},
    ...typeof refusal.incidentId === "string" && refusal.incidentId !== "" ? { incidentId: refusal.incidentId } : {},
  }
}

// ---- Any route of the contract -----------------------------------------------------------------

/**
 * A route's non-2xx answer read as its refusal: a Refusal@2, a 404 one included (a record the
 * route does not hold), except the cell's `route.not_found`, which says the route is not served.
 * Null for anything else. Both transports apply this one check: a route's HTTP answer, and an
 * answer the cell pushes over the live socket (`body` and `exact` as `refusalOf` takes them).
 */
export function routeRefusalOf(status: number, body: unknown, exact: () => ExactJson | undefined): Refusal | null {
  const refusal = refusalOf(status, body, exact)
  return refusal === null || (status === 404 && refusal.code === "route.not_found") ? null : refusal
}

type Defined<T> = Exclude<T, undefined>
/** The generated operation behind `route` and `method`. */
export type Operation<P extends keyof paths, M extends "get" | "post"> = Defined<paths[P][M]>
type RoutesWith<M extends "get" | "post"> = { [P in keyof paths]: [Operation<P, M>] extends [never] ? never : P }[keyof paths]
type SuccessStatus = 200 | 201 | 202 | 203 | 204
type MediaOf<R> = R extends { readonly content: infer C } ? keyof C & string : never
/** The media types a route answers with when it succeeds. */
export type SuccessMedia<O> = O extends { readonly responses: infer R } ? MediaOf<R[Extract<keyof R, SuccessStatus>]> : never
/** Whether `O` answers a success with something other than a JSON document (a Parquet export, …). */
type ServesFile<O> = [Exclude<SuccessMedia<O>, "application/json">] extends [never] ? false : true
type DocumentRoutesWith<M extends "get" | "post"> = { [P in RoutesWith<M>]: ServesFile<Operation<P, M>> extends true ? never : P }[RoutesWith<M>]
/** Every route whose GET answers with a JSON document (or nothing): what `get` reads. */
export type GetRoute = DocumentRoutesWith<"get">
/** Every route whose POST answers with a JSON document (or nothing): what `post` calls. */
export type PostRoute = DocumentRoutesWith<"post">
/** Every route whose GET answers with a file (`application/vnd.apache.parquet`, …): what `download` reads. */
export type FileRoute = { [P in RoutesWith<"get">]: ServesFile<Operation<P, "get">> extends true ? P : never }[RoutesWith<"get">]

/**
 * The media types each file route declares for its success: `download` accepts an answer only in
 * one of these. The compiler holds the table to the contract both ways: every file route is
 * listed, with every media type it declares and no other.
 */
export const FILE_MEDIA = {
  "/v1/objects/{type}:export": ["application/vnd.apache.parquet", "application/x-ndjson"],
} as const satisfies { readonly [P in FileRoute]: readonly SuccessMedia<Operation<P, "get">>[] }
type Unlisted = { [P in FileRoute]: Exclude<SuccessMedia<Operation<P, "get">>, (typeof FILE_MEDIA)[P][number]> }[FileRoute]
const everyMediaListed: [Unlisted] extends [never] ? true : never = true
void everyMediaListed

type ParamsOf<O> = O extends { readonly parameters: infer X } ? X : never
type Member<X, K extends string> = X extends { readonly [P in K]?: infer V } ? Defined<V> : never
type Present<T> = [T] extends [never] ? Record<never, never> : T
type RequiredKeys<T> = { [K in keyof T]-?: Record<never, never> extends Pick<T, K> ? never : K }[keyof T]
type Slot<K extends string, T> = [keyof T] extends [never] ? { readonly [P in K]?: undefined }
  : [RequiredKeys<T>] extends [never] ? { readonly [P in K]?: T } : { readonly [P in K]: T }
type Json<C> = C extends { readonly content: { readonly "application/json": infer B } } ? B : never
type BodySlot<O> = O extends { readonly requestBody: infer R } ? [Json<R>] extends [never] ? { readonly body?: undefined } : { readonly body: Json<R> }
  : O extends { readonly requestBody?: infer R } ? [Json<Defined<R>>] extends [never] ? { readonly body?: undefined } : { readonly body?: Json<Defined<R>> }
  : { readonly body?: undefined }

/** What one route call takes: its path parameters, query (never the scope: the client names it), body, and an abort signal. */
export type RouteCall<O> = Slot<"path", Present<Member<ParamsOf<O>, "path">>>
  & Slot<"query", Omit<Present<Member<ParamsOf<O>, "query">>, keyof Scope>>
  & BodySlot<O>
  & { readonly signal?: AbortSignal }
/** `[call]`, optional when the route needs nothing. */
export type RouteArgs<O> = [RequiredKeys<RouteCall<O>>] extends [never] ? [call?: RouteCall<O>] : [call: RouteCall<O>]

type ResponseJson<R> = R extends { readonly content: { readonly "application/json": infer B } } ? B : undefined
/** The document a route answers with when it succeeds (a union when it has several successes). */
export type Success<O> = O extends { readonly responses: infer R } ? ResponseJson<R[Extract<keyof R, SuccessStatus>]> : never

/** A contract document as `parseExactJson` reads it: every number in it is its JSON source text. */
export type ExactDocument<T> = T extends number ? string
  : T extends readonly (infer I)[] ? readonly ExactDocument<I>[]
  : T extends object ? { readonly [K in keyof T]: ExactDocument<T[K]> }
  : T

/** What a route answered: its document, read exactly, or its refusal. Anything else throws `NornUnavailable`. */
export type RouteResult<T> =
  | { readonly kind: "document"; readonly status: number; readonly document: ExactDocument<T> }
  | ({ readonly kind: "refused" } & Refusal)

/** What a file route answered: the file in a media type the route declares, never decoded, or its refusal. */
export type FileResult =
  | { readonly kind: "file"; readonly status: number; readonly contentType: string; readonly bytes: ArrayBuffer }
  | ({ readonly kind: "refused" } & Refusal)

/** `route` with its `{name}` segments filled, each percent-encoded. One linear pass, no pattern. */
function fillRoute(route: string, path: Readonly<Record<string, unknown>> | undefined): string {
  let filled = ""
  let at = 0
  for (let open = route.indexOf("{"); open !== -1; open = route.indexOf("{", at)) {
    const close = route.indexOf("}", open + 1)
    if (close === -1) throw new TypeError(`${route} has an unclosed path parameter`)
    const name = route.slice(open + 1, close)
    const value = path?.[name]
    if (value === undefined || value === null) throw new TypeError(`${route} needs the path parameter ${name}`)
    filled += `${route.slice(at, open)}${encodeURIComponent(String(value))}`
    at = close + 1
  }
  return filled + route.slice(at)
}

/** The media type `contentType` names, parameters such as `charset` aside. */
const mediaOf = (contentType: string): string => contentType.split(";", 1)[0]?.trim().toLowerCase() ?? ""

/** Whether a success of `contentType` may be read as a JSON document: JSON, text, or no type. A file's media type never is. */
function readsAsJson(contentType: string | null): boolean {
  if (contentType === null) return true
  const media = mediaOf(contentType)
  return media === "" || media === "application/json" || media.endsWith("+json") || media.startsWith("text/")
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
  /** Read a route that answers with a JSON document. */
  get<P extends GetRoute>(route: P, ...args: RouteArgs<Operation<P, "get">>): Promise<RouteResult<Success<Operation<P, "get">>>>
  /** Call a route that answers with a JSON document (or nothing). */
  post<P extends PostRoute>(route: P, ...args: RouteArgs<Operation<P, "post">>): Promise<RouteResult<Success<Operation<P, "post">>>>
  /** Read a route that answers with a file (an export): its bytes, never decoded as JSON. */
  download<P extends FileRoute>(route: P, ...args: RouteArgs<Operation<P, "get">>): Promise<FileResult>
  /** The scoped path a GET of `route` asks, without the base URL: what a live subscription names. */
  path<P extends GetRoute>(route: P, ...args: RouteArgs<Operation<P, "get">>): string
}

const json = (value: unknown, what = "action input"): string => {
  const text = JSON.stringify(value)
  if (text === undefined) throw new TypeError(`${what} is not JSON`)
  return text
}

/** A request's own query members, in order (the scope is added before them). */
type Query = readonly (readonly [string, string])[]

/** A route call as the client reads it, past its types. */
interface LooseCall {
  readonly path?: Readonly<Record<string, unknown>>
  readonly query?: Readonly<Record<string, unknown>>
  readonly body?: unknown
  readonly signal?: AbortSignal
}

/** A call's route and query members, in order; absent members are left out and a list repeats its key. The scope is never the call's. */
function routeOf(template: string, call: LooseCall): { readonly path: string; readonly query: Query } {
  const query: [string, string][] = []
  for (const [key, value] of Object.entries(call.query ?? {})) {
    if (key === "orgId" || key === "environmentId") continue
    for (const item of Array.isArray(value) ? value : [value]) {
      if (item !== undefined && item !== null) query.push([key, String(item)])
    }
  }
  return { path: fillRoute(template, call.path), query }
}

export function createNornClient<M extends ActionMapShape<M> = UntypedActions>(options: NornClientOptions): NornClient<M> {
  const fetcher: Fetch = options.fetch ?? ((input, init) => fetch(input, init))
  const base = options.baseUrl ?? ""
  const pin = options.definitionsVersion
  /** `path` with the scope, then its own query members: every request names the client's scope. */
  const scoped = (path: string, query: Query): string =>
    `${path}?${new URLSearchParams([["orgId", options.scope.orgId], ["environmentId", options.scope.environmentId], ...query.map(([key, value]) => [key, value])])}`

  /**
   * One request. A signal aborted before the request is sent throws its abort as is: nothing was
   * sent. Once `fetch` is called, a write (`write`, a POST that may change the cell) that gets no
   * readable answer may have been applied, whatever went wrong (the network, the body stream, the
   * caller's abort, the host's own fetch): `NornUnavailable` with outcome `unknown`, the original
   * error as its `cause`. A read's lost answer (`TypeError`) is `NornUnavailable` too; a read's
   * abort or host error passes through as is.
   */
  async function exchange<T>(path: string, query: Query, init: RequestInit, write: boolean, read: (response: Response) => Promise<T>): Promise<T> {
    const extra = options.headers === undefined ? {} : await options.headers()
    init.signal?.throwIfAborted()
    const lost = (error: unknown): never => {
      if (write || (error instanceof TypeError && init.signal?.aborted !== true)) throw new NornUnavailable(0, undefined, { cause: error })
      throw error
    }
    let response: Response
    try {
      response = await fetcher(`${base}${scoped(path, query)}`, { ...init, headers: { ...extra, ...init.headers as Record<string, string> } })
    } catch (error) {
      return lost(error)
    }
    try {
      return await read(response)
    } catch (error) {
      return lost(error)
    }
  }

  const request = (path: string, query: Query, init: RequestInit, write: boolean) =>
    exchange(path, query, init, write, async (response) => ({ status: response.status, ok: response.ok, text: await response.text() }))

  /** A non-2xx answer: its refusal, else the route is not served (404/503) or the cell did not answer. */
  function refusalOrThrow(status: number, text: string): Refusal {
    // A 404 is the route's absence whatever its body (`route.not_found`). Any other status is a
    // refusal only when it is a Refusal@2 document (`readRefusal`).
    const refusal = status === 404 ? null : readRefusal(status, text)
    if (refusal === null) throw new NornUnavailable(status)
    return refusal
  }

  /** A route's non-2xx answer: its refusal (`routeRefusalOf`), else the route is not served (404/503) or the cell did not answer. */
  function routeRefusal(status: number, text: string): Refusal {
    const refusal = routeRefusalOf(status, parsed(text), () => parseExactJson(text))
    if (refusal === null) throw new NornUnavailable(status)
    return refusal
  }

  async function routeDocument(method: "GET" | "POST", template: string, call: LooseCall = {}): Promise<RouteResult<unknown>> {
    const { path, query } = routeOf(template, call)
    const body = call.body === undefined ? undefined : json(call.body, "the call's body")
    const { status, ok, text, contentType } = await exchange(path, query, {
      method,
      headers: { accept: "application/json", ...body === undefined ? {} : { "content-type": "application/json" } },
      ...body === undefined ? {} : { body },
      ...call.signal === undefined ? {} : { signal: call.signal },
    }, method === "POST", async (response) => ({ status: response.status, ok: response.ok, contentType: response.headers.get("content-type"), text: await response.text() }))
    if (!ok) return { kind: "refused", ...routeRefusal(status, text) }
    // A file's bytes are never decoded as a document; a success the route declares without content
    // (a `202` to a notification) has no document.
    if (!readsAsJson(contentType)) throw new NornUnexpectedResponse(status, "a JSON document")
    if (text.trim() === "") return { kind: "document", status, document: undefined }
    const document = parseExactJson(text)
    if (document === undefined) throw new NornUnexpectedResponse(status, "a JSON document")
    return { kind: "document", status, document }
  }

  async function download(template: string, call: LooseCall = {}): Promise<FileResult> {
    const declared: readonly string[] = Object.hasOwn(FILE_MEDIA, template) ? FILE_MEDIA[template as FileRoute] : []
    const { path, query } = routeOf(template, call)
    const answer = await exchange(path, query, { headers: { accept: "*/*" }, ...call.signal === undefined ? {} : { signal: call.signal } }, false, async (response) => {
      if (!response.ok) return { ok: false, status: response.status, text: await response.text() } as const
      const contentType = response.headers.get("content-type") ?? ""
      // Only a media type the route declares is the file. A JSON document, no type, or a proxy's
      // or sign-in page's `200 text/html` is not.
      if (!declared.includes(mediaOf(contentType))) throw new NornUnexpectedResponse(response.status, declared.join(" or "))
      return { ok: true, status: response.status, contentType, bytes: await response.arrayBuffer() } as const
    })
    if (!answer.ok) return { kind: "refused", ...routeRefusal(answer.status, answer.text) }
    return { kind: "file", status: answer.status, contentType: answer.contentType, bytes: answer.bytes }
  }

  async function send(actionId: string, input: string, call: CallOptions): Promise<CallResult> {
    const version = call.definitionsVersion ?? pin
    const trace = call.trace === undefined ? "" : `,"trace":${JSON.stringify(call.trace)}`
    const { status, ok, text } = await request(`/v1/actions/${encodeURIComponent(actionId)}`, version === undefined ? [] : [["version", String(version)]], {
      method: "POST",
      headers: { accept: "application/json", "content-type": "application/json" },
      body: `{"input":${input},"idempotencyKey":${JSON.stringify(call.idempotencyKey)}${trace}}`,
      ...call.signal === undefined ? {} : { signal: call.signal },
    }, true)
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
      const params: [string, string][] = []
      if (query.objectKind !== undefined) {
        params.push(["objectKind", query.objectKind])
        if (query.objectId !== undefined) params.push(["objectId", query.objectId])
      }
      const { status, ok, text } = await request("/v1/actions", params, { headers: { accept: "application/json" }, ...signal === undefined ? {} : { signal } }, false)
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
    get: <P extends GetRoute>(route: P, ...args: RouteArgs<Operation<P, "get">>) =>
      routeDocument("GET", route, args[0] as LooseCall | undefined) as Promise<RouteResult<Success<Operation<P, "get">>>>,
    post: <P extends PostRoute>(route: P, ...args: RouteArgs<Operation<P, "post">>) =>
      routeDocument("POST", route, args[0] as LooseCall | undefined) as Promise<RouteResult<Success<Operation<P, "post">>>>,
    download: <P extends FileRoute>(route: P, ...args: RouteArgs<Operation<P, "get">>) => download(route, args[0] as LooseCall | undefined),
    path: <P extends GetRoute>(route: P, ...args: RouteArgs<Operation<P, "get">>) => {
      const { path, query } = routeOf(route, (args[0] ?? {}) as LooseCall)
      return scoped(path, query)
    },
  }
}
