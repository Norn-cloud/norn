/**
 * The typed client for a Norn cell's HTTP routes (#1374): every route, its
 * parameters, body and success document are the generated contract's
 * (`norn openapi` → `src/generated/norn-api.ts`), so a server field change is a
 * compile error in every caller.
 *
 * - Scope is explicit: every call carries the org and environment the client
 *   was made for. The server still authorizes each call; the client decides
 *   nothing and holds no permission logic.
 * - Exact decimals by default: a document comes with the exact text of each of
 *   its numbers (#1271), keyed by RFC 6901 pointer.
 * - A refusal is a typed {@link NornRefusalError} carrying the server's code,
 *   category and checker diagnostics; an answer that is no Norn refusal (a
 *   proxy's error page, a dropped connection) is a {@link NornUnavailableError}.
 * - `fetch`, the base URL and auth are pluggable, so the same client runs in a
 *   browser (cookie auth through a fronting app), Bun and Workers (a host token).
 */
import type { components, paths } from "./generated/norn-api.ts"
import { readExact } from "./exact.ts"

type Schemas = components["schemas"]
export type Refusal = Schemas["Refusal"]
export type RefusalCategory = Schemas["RefusalCategory"]
export type RefusalDiagnostic = Schemas["RefusalDiagnostic"]

/** The organization and cell environment every call names. */
export interface NornScope { readonly orgId: string; readonly environmentId: string }

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>

export type Method = "get" | "post" | "put" | "patch" | "delete"
type Defined<T> = Exclude<T, undefined>
/** The generated operation behind `route` and `method`. */
export type Operation<P extends keyof paths, M extends Method> = Defined<paths[P][M]>
/** Every route that serves `method`. */
export type RoutesWith<M extends Method> = { [P in keyof paths]: [Operation<P, M>] extends [never] ? never : P }[keyof paths]
export type GetRoute = RoutesWith<"get">
export type PostRoute = RoutesWith<"post">

/** The query members the client binds itself: the scope. */
type Bound = "orgId" | "environmentId"
type ParamsOf<O> = O extends { readonly parameters: infer X } ? X : never
type Member<X, K extends string> = X extends { readonly [P in K]?: infer V } ? Defined<V> : never
type Present<T> = [T] extends [never] ? Record<never, never> : T
export type PathParams<O> = Present<Member<ParamsOf<O>, "path">>
export type QueryParams<O> = Omit<Present<Member<ParamsOf<O>, "query">>, Bound>
type RequiredKeys<T> = { [K in keyof T]-?: Record<never, never> extends Pick<T, K> ? never : K }[keyof T]
type Slot<K extends string, T> = [keyof T] extends [never] ? { readonly [P in K]?: undefined }
  : [RequiredKeys<T>] extends [never] ? { readonly [P in K]?: T } : { readonly [P in K]: T }
type Json<C> = C extends { readonly content: { readonly "application/json": infer B } } ? B : never
type BodySlot<O> = O extends { readonly requestBody: infer R } ? [Json<R>] extends [never] ? { readonly body?: undefined } : { readonly body: Json<R> }
  : O extends { readonly requestBody?: infer R } ? [Json<Defined<R>>] extends [never] ? { readonly body?: undefined } : { readonly body?: Json<Defined<R>> }
  : { readonly body?: undefined }

/** What one call takes: its path parameters, query (without the bound scope), body, and an abort signal. */
export type CallOptions<O> = Slot<"path", PathParams<O>> & Slot<"query", QueryParams<O>> & BodySlot<O> & {
  readonly signal?: AbortSignal
  readonly headers?: Readonly<Record<string, string>>
}
/** `[options]`, optional when the call needs nothing. */
export type CallArgs<O> = [RequiredKeys<CallOptions<O>>] extends [never] ? [options?: CallOptions<O>] : [options: CallOptions<O>]

type SuccessStatus = 200 | 201 | 202 | 203 | 204
type ResponseJson<R> = R extends { readonly content: { readonly "application/json": infer B } } ? B : undefined
/** The document a route answers with when it succeeds (a union when it has several successes). */
export type Success<O> = O extends { readonly responses: infer R } ? ResponseJson<R[Extract<keyof R, SuccessStatus>]> : never

/** A document as the server sent it. */
export interface NornDocument<T> {
  readonly status: number
  readonly document: T
  /** Every number's exact text by RFC 6901 pointer: `numbers.get("/values/price")` is `"19.990"`. */
  readonly numbers: ReadonlyMap<string, string>
  /** The response text, byte for byte. */
  readonly text: string
  /** The server's `Cache-Control`, if it sent one: whether a client may store this document is the server's to say. */
  readonly cacheControl: string | null
  readonly etag: string | null
}

/** The server refused the call; nothing was committed. */
export class NornRefusalError extends Error {
  override readonly name = "NornRefusalError"
  readonly code: string
  readonly category: RefusalCategory
  readonly diagnostics: readonly RefusalDiagnostic[]
  readonly retryable: boolean

  constructor(readonly status: number, readonly refusal: Refusal) {
    super(refusal.message)
    this.code = refusal.code
    this.category = refusal.category
    this.diagnostics = refusal.diagnostics ?? []
    this.retryable = refusal.retryable
  }
}

/**
 * The call got no Norn answer: `status` 0 when the fetch itself failed (its error is the `cause`),
 * otherwise a body that is not a Norn document (`body` keeps it).
 */
export class NornUnavailableError extends Error {
  override readonly name = "NornUnavailableError"
  constructor(readonly status: number, readonly body: string | null, message?: string, options?: ErrorOptions) {
    super(message ?? (status === 0 ? "The cell did not answer." : `The cell did not answer with a Norn document (${status}).`), options)
  }
}

const CATEGORIES: ReadonlySet<string> = new Set<RefusalCategory>(["visibility", "permission", "precondition", "conflict", "invalid", "limit", "internal"])

/** The refusal `text` holds, if it is one (`norn/refusal@2`: a code, a category and a message). */
export function refusalOf(text: string): Refusal | null {
  let body: unknown
  try { body = JSON.parse(text) } catch { return null }
  if (body === null || typeof body !== "object" || Array.isArray(body)) return null
  const { code, category, message } = body as Record<string, unknown>
  if (typeof code !== "string" || typeof message !== "string" || typeof category !== "string" || !CATEGORIES.has(category)) return null
  const refusal = body as Refusal
  return { ...refusal, retryable: refusal.retryable === true }
}

/** `route` with its `{name}` segments filled, each percent-encoded. One linear pass. */
export function fillRoute(route: string, path: Readonly<Record<string, unknown>> | undefined): string {
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

/** `url` without its trailing slashes. */
function trimSlashes(url: string): string {
  let end = url.length
  while (end > 0 && url.charCodeAt(end - 1) === 47) end -= 1
  return url.slice(0, end)
}

/** The query string: the call's own members in order, then the scope. Absent members are left out; a list repeats its key. */
export function scopedQuery(query: Readonly<Record<string, unknown>> | undefined, scope: NornScope): string {
  const search = new URLSearchParams()
  for (const [key, value] of Object.entries(query ?? {})) {
    if (key === "orgId" || key === "environmentId") continue
    for (const item of Array.isArray(value) ? value : [value]) {
      if (item !== undefined && item !== null) search.append(key, String(item))
    }
  }
  search.set("orgId", scope.orgId)
  search.set("environmentId", scope.environmentId)
  return search.toString()
}

export interface NornClientOptions {
  /** Where the cell's `/v1` routes are served: a host origin (`https://cell.example`) or a fronting proxy's prefix (`/api/host`). */
  readonly baseUrl: string
  /** The scope every call names, read before each call. */
  readonly scope: NornScope | (() => NornScope | Promise<NornScope>)
  readonly fetch?: FetchLike
  /** A host token, read before each call and sent as `Authorization: Bearer`; none sends no header. */
  readonly token?: () => string | null | undefined | Promise<string | null | undefined>
  /** Cookie auth through a fronting application (`same-origin` in a browser app). */
  readonly credentials?: RequestCredentials
}

export interface NornClient {
  /** The scoped route a call would ask, without the base URL (`/v1/runs/r1?orgId=…`): what a live subscription names. */
  readonly route: <P extends GetRoute>(route: P, ...args: CallArgs<Operation<P, "get">>) => Promise<string>
  readonly get: <P extends GetRoute>(route: P, ...args: CallArgs<Operation<P, "get">>) => Promise<NornDocument<Success<Operation<P, "get">>>>
  readonly post: <P extends PostRoute>(route: P, ...args: CallArgs<Operation<P, "post">>) => Promise<NornDocument<Success<Operation<P, "post">>>>
}

interface LooseOptions {
  readonly path?: Readonly<Record<string, unknown>>
  readonly query?: Readonly<Record<string, unknown>>
  readonly body?: unknown
  readonly signal?: AbortSignal
  readonly headers?: Readonly<Record<string, string>>
}

/** A client for one cell scope. */
export function createNornClient(options: NornClientOptions): NornClient {
  const fetcher: FetchLike = options.fetch ?? ((input, init) => fetch(input, init))
  const base = trimSlashes(options.baseUrl)
  const scopeOf = async () => typeof options.scope === "function" ? options.scope() : options.scope

  async function route(template: string, call: LooseOptions = {}): Promise<string> {
    return `${fillRoute(template, call.path)}?${scopedQuery(call.query, await scopeOf())}`
  }

  async function request<T>(method: "GET" | "POST", template: string, call: LooseOptions = {}): Promise<NornDocument<T>> {
    const target = `${base}${await route(template, call)}`
    const headers: Record<string, string> = { accept: "application/json", ...call.headers }
    if (call.body !== undefined) headers["content-type"] = "application/json"
    const token = await options.token?.()
    if (token) headers["authorization"] = `Bearer ${token}`
    let response: Response
    let text: string
    try {
      response = await fetcher(target, {
        method,
        headers,
        ...(call.body === undefined ? {} : { body: JSON.stringify(call.body) }),
        ...(call.signal === undefined ? {} : { signal: call.signal }),
        ...(options.credentials === undefined ? {} : { credentials: options.credentials }),
      })
      text = await response.text()
    } catch (error) {
      if (call.signal?.aborted) throw error
      throw new NornUnavailableError(0, null, undefined, { cause: error })
    }
    if (response.ok) {
      // A success the route declares without content (`202` to a notification) has no document.
      if (text.trim() === "") return { status: response.status, document: undefined as T, numbers: new Map(), text, cacheControl: response.headers.get("cache-control"), etag: response.headers.get("etag") }
      let exact
      try { exact = readExact<T>(text) } catch { throw new NornUnavailableError(response.status, text, "The cell's answer was not a document.") }
      return { status: response.status, ...exact, text, cacheControl: response.headers.get("cache-control"), etag: response.headers.get("etag") }
    }
    const refusal = refusalOf(text)
    if (refusal !== null) throw new NornRefusalError(response.status, refusal)
    throw new NornUnavailableError(response.status, text)
  }

  const loose = (args: readonly unknown[]) => args[0] as LooseOptions | undefined
  return {
    route: <P extends GetRoute>(template: P, ...args: CallArgs<Operation<P, "get">>) => route(template, loose(args)),
    get: <P extends GetRoute>(template: P, ...args: CallArgs<Operation<P, "get">>) =>
      request<Success<Operation<P, "get">>>("GET", template, loose(args)),
    post: <P extends PostRoute>(template: P, ...args: CallArgs<Operation<P, "post">>) =>
      request<Success<Operation<P, "post">>>("POST", template, loose(args)),
  }
}
