/**
 * Studio over a local `norn dev` cell: one loopback-only server that serves the
 * built app and stands in for the two Studio Worker routes the pages read
 * through, `/api/me` and `/api/host/*`, for the developer `norn dev` serves.
 *
 *   bun run build && bun run local --project ../my-project [--port 5180] [--as reviewer]
 *
 * It decides nothing: no permission logic lives here. The token is read on the
 * server and never reaches the browser; `/api/me` reports exactly what it names.
 *
 * The dev host stays the only authority: it grants everything to the
 * manifest's one actor on a loopback cell (DEV-LOOPBACK-1), and this server
 * only carries that session's token to it, read from `.norn/dev.json` on every
 * call so a restarted `norn dev` needs no restart here. Because the token holds
 * every grant, nobody else may reach this server: it binds loopback only, and it
 * refuses a request whose Host is not its own address (DNS rebinding) or that
 * another site sent (Origin or Sec-Fetch-Site), so a cross-site form cannot act
 * as the developer.
 *
 * `--as reviewer` serves the session's second local principal (the reviewer
 * `norn dev` names in its descriptor) instead, so an approval one Studio asks
 * for can be decided in another: the requester never decides their own call.
 *
 * Live reads (UI-LIVE-1, `norn.live.v1`): the Studio pages hold one socket on
 * `/api/host/v1/subscribe`. The dev host has no socket, so this server keeps
 * the subscriptions and answers each by asking the dev host's own GET as the
 * session, exactly as the hosted cell re-asks its handler: after every write
 * it forwarded, and every two seconds for writes that reached `norn dev`
 * another way (a `--load`, a loader script, a CLI call). Only a changed answer
 * is sent. It reads nothing else and decides nothing.
 */
import { existsSync, readFileSync, statSync } from "node:fs"
import { isAbsolute, join, relative, resolve } from "node:path"

export interface DevSession {
  readonly url: string
  readonly token: string
  readonly actor: string
  /** Which of the session's local principals this is. */
  readonly as?: "developer" | "reviewer"
  readonly orgId: string
  readonly environmentId: string
  readonly project: string
}

export class LocalCellError extends Error {}

const LOOPBACK_URL = /^http:\/\/(127\.0\.0\.1|\[::1\]):\d{1,5}$/

/** The running `norn dev` session of `project`, as its descriptor (`norn-dev/1`) names it. */
export function readSession(project: string, as: "developer" | "reviewer" = "developer"): DevSession {
  const descriptorPath = join(project, ".norn/dev.json")
  if (!existsSync(descriptorPath)) throw new LocalCellError(`No running norn dev in ${project}: start it there first (${descriptorPath} is missing).`)
  const descriptor = JSON.parse(readFileSync(descriptorPath, "utf8")) as Record<string, unknown>
  const scope = descriptor["scope"] as Record<string, unknown> | undefined
  const reviewer = descriptor["reviewer"] as Record<string, unknown> | undefined
  const { url } = descriptor
  const tokenFile = as === "reviewer" ? reviewer?.["tokenFile"] : descriptor["tokenFile"]
  const actor = as === "reviewer" ? reviewer?.["actor"] : descriptor["actor"]
  if (as === "reviewer" && reviewer === undefined) throw new LocalCellError(`${descriptorPath} names no reviewer: restart norn dev with this checkout's host.`)
  if (descriptor["format"] !== "norn-dev/1" || typeof url !== "string" || typeof tokenFile !== "string" || typeof actor !== "string"
    || typeof scope?.["orgId"] !== "string" || typeof scope["environmentId"] !== "string") {
    throw new LocalCellError(`${descriptorPath} is not a norn-dev/1 descriptor.`)
  }
  // The token holds every grant: it is only ever sent to a loopback host.
  if (!LOOPBACK_URL.test(url)) throw new LocalCellError(`norn dev reports ${url}, which is not a loopback address.`)
  const tokenPath = resolve(project, tokenFile)
  const inside = relative(resolve(project), tokenPath)
  if (inside.startsWith("..") || isAbsolute(inside)) throw new LocalCellError(`The token file ${tokenFile} is outside the project.`)
  const name = (() => {
    try { const manifest = JSON.parse(readFileSync(join(project, "norn.json"), "utf8")) as { name?: unknown }; return typeof manifest.name === "string" ? manifest.name : undefined }
    catch { return undefined }
  })()
  return { url, token: readFileSync(tokenPath, "utf8").trim(), actor, ...(as === "reviewer" ? { as } : {}), orgId: scope["orgId"], environmentId: scope["environmentId"], project: name ?? scope["orgId"] }
}

export interface LocalStudio {
  /** The built app (`dist`). */
  readonly dist: string
  /** The session to forward to, read again for every call. */
  readonly session: () => DevSession
  /** This server's own port: the only Host it answers. */
  readonly port: number
  readonly fetcher?: typeof fetch
  /** Called after a write this server forwarded was accepted (a status below 400). */
  readonly committed?: () => void
}

const refuse = (status: number, error: string) => Response.json({ error }, { status, headers: { "cache-control": "no-store" } })

/** Whether the request came from this server's own pages, at its own address. */
function sameSite(request: Request, port: number): boolean {
  const own = [`127.0.0.1:${port}`, `localhost:${port}`, `[::1]:${port}`]
  const host = request.headers.get("host")
  if (host === null || !own.includes(host.toLowerCase())) return false
  const origin = request.headers.get("origin")
  if (origin !== null && !own.some((address) => origin === `http://${address}`)) return false
  const site = request.headers.get("sec-fetch-site")
  return site === null || site === "same-origin" || site === "none"
}

/** Answer one request: the app's files, `/api/me`, or a call forwarded to the dev host. */
export async function localStudio(request: Request, studio: LocalStudio): Promise<Response> {
  const url = new URL(request.url)
  const path = url.pathname
  if (!sameSite(request, studio.port)) return refuse(403, "This local Studio answers only its own pages at its own address.")
  if (path.startsWith("/api/")) {
    let session: DevSession
    try { session = studio.session() } catch (error) { return refuse(503, error instanceof Error ? error.message : String(error)) }
    if (path === "/api/me") {
      return Response.json({
        // Exactly what the dev token names: its one actor and the manifest's scope.
        user: { id: session.actor, name: session.actor, email: "" },
        organization: { id: session.orgId, name: session.project, role: session.as === "reviewer" ? "local reviewer" : "local developer" },
        environmentId: session.environmentId,
      }, { headers: { "cache-control": "no-store" } })
    }
    if (path.startsWith("/api/host/")) {
      const headers = new Headers({ authorization: `Bearer ${session.token}` })
      for (const name of ["content-type", "accept"]) {
        const value = request.headers.get(name)
        if (value !== null) headers.set(name, value)
      }
      const body = request.method === "GET" || request.method === "HEAD" ? undefined : await request.arrayBuffer()
      let answer: Response
      try {
        answer = await (studio.fetcher ?? fetch)(`${session.url}${path.slice("/api/host".length)}${url.search}`, { method: request.method, headers, body })
      } catch {
        return refuse(503, `norn dev at ${session.url} did not answer; is it still running?`)
      }
      if (request.method !== "GET" && request.method !== "HEAD" && answer.status < 400) studio.committed?.()
      return new Response(await answer.arrayBuffer(), {
        status: answer.status,
        headers: { "content-type": answer.headers.get("content-type") ?? "application/json", "cache-control": "no-store" },
      })
    }
    return refuse(404, "No such endpoint.")
  }
  if (request.method !== "GET" && request.method !== "HEAD") return refuse(405, "Only GET reads the app.")
  const root = resolve(studio.dist)
  const file = resolve(root, `.${decodeURIComponent(path)}`)
  const inside = relative(root, file)
  if (!inside.startsWith("..") && !isAbsolute(inside) && inside !== "" && existsSync(file) && statSync(file).isFile()) return new Response(Bun.file(file))
  // Every other path is a client route: the app's own page renders it.
  return new Response(Bun.file(join(root, "index.html")), { headers: { "content-type": "text/html; charset=utf-8" } })
}

export const LIVE_PATH = "/api/host/v1/subscribe"
export const LIVE_PROTOCOL = "norn.live.v1"
/** Subscriptions per socket, as on the hosted cell. */
export const MAX_SUBSCRIPTIONS = 8
const MAX_MESSAGE = 4096
/** How often the subscriptions are asked again for writes this server did not forward. */
export const LIVE_INTERVAL_MS = 2_000

/** Whether a live upgrade may be accepted: this server's own pages only, as every other call. */
export function liveUpgrade(request: Request, port: number): Response | null {
  if (!sameSite(request, port)) return refuse(403, "This local Studio answers only its own pages at its own address.")
  if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") return refuse(426, "Live reads are a WebSocket.")
  return null
}

/** A path a subscription may name: one of the cell's GET routes, never the live route itself. */
export function subscribable(path: unknown): string | null {
  if (typeof path !== "string" || path.length > 512 || !path.startsWith("/v1/")) return null
  // Decided on the normalised path, so a dot segment (`..`, `%2e%2e`) cannot leave `/v1/` or reach the live route.
  const url = new URL(path, "http://cell")
  if (url.origin !== "http://cell" || !url.pathname.startsWith("/v1/") || url.pathname.startsWith("/v1/subscribe")) return null
  return `${url.pathname}${url.search}`
}

/** The slice of a server WebSocket the live hub uses. */
export interface LiveSocket {
  send(data: string): unknown
  close(code?: number, reason?: string): void
}

/** The dev host's GET of `path` (with its query), as the session. */
export type Ask = (path: string) => Promise<{ status: number; body: string }>

async function digest(text: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)))
  return Array.from(bytes.slice(0, 12), (b) => b.toString(16).padStart(2, "0")).join("")
}

/** The live subscriptions of this server's sockets (`norn.live.v1`, as `apps/norn-cell-do/subscribe.mjs`). */
export class LiveHub {
  readonly #sockets = new Map<LiveSocket, Map<string, { path: string; digest: string }>>()
  constructor(private readonly ask: Ask) {}

  get size(): number { return this.#sockets.size }
  open(ws: LiveSocket): void { this.#sockets.set(ws, new Map()) }
  close(ws: LiveSocket): void { this.#sockets.delete(ws) }

  /** One client message: subscribe or unsubscribe. */
  async message(ws: LiveSocket, raw: unknown): Promise<void> {
    const subs = this.#sockets.get(ws)
    if (subs === undefined) return
    if (typeof raw !== "string" || raw.length > MAX_MESSAGE) return ws.close(1009, "message too large")
    let msg: { type?: unknown; id?: unknown; path?: unknown }
    try { msg = JSON.parse(raw) } catch { return ws.close(1007, "not JSON") }
    const id = typeof msg?.id === "string" && /^[A-Za-z0-9_.:-]{1,64}$/.test(msg.id) ? msg.id : null
    const refused = (code: string, message: string) => { ws.send(JSON.stringify({ type: "refused", id, code, message })) }
    if (id === null) return refused("live.invalid_id", "a subscription id is 1 to 64 letters, digits or _.:-")
    if (msg.type === "unsubscribe") { subs.delete(id); return }
    if (msg.type !== "subscribe") return refused("live.unknown_message", "messages are subscribe or unsubscribe")
    const path = subscribable(msg.path)
    if (path === null) return refused("live.not_subscribable", "a subscription names one of this cell's GET routes")
    if (!subs.has(id) && subs.size >= MAX_SUBSCRIPTIONS) return refused("live.too_many_subscriptions", `at most ${MAX_SUBSCRIPTIONS} subscriptions per socket`)
    subs.set(id, { path, digest: "" })
    await this.#evaluate(ws, id)
  }

  /** Ask every subscription again; send the answers that changed. */
  async publish(): Promise<void> {
    for (const ws of [...this.#sockets.keys()]) await this.#evaluate(ws)
  }

  async #evaluate(ws: LiveSocket, only?: string): Promise<void> {
    const subs = this.#sockets.get(ws)
    if (subs === undefined) return
    for (const [id, sub] of subs) {
      if (only !== undefined && id !== only) continue
      let reply: { status: number; body: string }
      try { reply = await this.ask(sub.path) } catch { continue }
      if (reply.status === 401) { ws.close(4401, "token expired"); this.close(ws); return }
      const value = await digest(`${reply.status}\n${reply.body}`)
      // A subscription removed while its answer was read sends nothing.
      if (value === sub.digest || subs.get(id) !== sub) continue
      sub.digest = value
      let body: unknown = null
      try { body = JSON.parse(reply.body) } catch { body = null }
      ws.send(JSON.stringify({ type: "snapshot", id, status: reply.status, digest: value, body }))
    }
  }
}

/** The dev host's GET, as `session()` names it now. */
export function askAs(session: () => DevSession, fetcher: typeof fetch = fetch): Ask {
  return async (path) => {
    const current = session()
    const answer = await fetcher(`${current.url}${path}`, { headers: { authorization: `Bearer ${current.token}`, accept: "application/json" } })
    return { status: answer.status, body: await answer.text() }
  }
}

/** Serve the local Studio: the app, `/api/*`, and its live socket. */
export function serveLocal(studio: Omit<LocalStudio, "committed"> & { readonly hostname?: string; readonly intervalMs?: number }) {
  const live = new LiveHub(askAs(studio.session, studio.fetcher))
  const publish = () => { if (live.size > 0) void live.publish() }
  const timer = setInterval(publish, studio.intervalMs ?? LIVE_INTERVAL_MS)
  const server = Bun.serve<undefined, never>({
    hostname: studio.hostname ?? "127.0.0.1",
    port: studio.port,
    fetch(request, server) {
      if (new URL(request.url).pathname === LIVE_PATH) {
        const refused = liveUpgrade(request, server.port ?? studio.port)
        if (refused !== null) return refused
        const asked = (request.headers.get("sec-websocket-protocol") ?? "").split(",").map((name) => name.trim()).includes(LIVE_PROTOCOL)
        if (server.upgrade(request, { data: undefined, ...(asked ? { headers: { "sec-websocket-protocol": LIVE_PROTOCOL } } : {}) })) return undefined
        return refuse(400, "The live socket could not be opened.")
      }
      return localStudio(request, { ...studio, port: server.port ?? studio.port, committed: publish })
    },
    websocket: {
      open: (ws) => live.open(ws),
      message: (ws, message) => live.message(ws, typeof message === "string" ? message : null),
      close: (ws) => live.close(ws),
    },
  })
  return { server, live, stop: () => { clearInterval(timer); server.stop(true) } }
}

function option(args: readonly string[], name: string): string | undefined {
  const at = args.indexOf(name)
  return at === -1 ? undefined : args[at + 1]
}

if (import.meta.main) {
  const args = process.argv.slice(2)
  const project = resolve(option(args, "--project") ?? ".")
  const port = Number(option(args, "--port") ?? 5180)
  const host = option(args, "--host") ?? "127.0.0.1"
  if (host !== "127.0.0.1") throw new LocalCellError(`--host ${host} refused: the local Studio carries an all-grants token and binds 127.0.0.1 only.`)
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new LocalCellError(`--port ${option(args, "--port")} is not a port.`)
  const dist = resolve(import.meta.dir, "../dist")
  if (!existsSync(join(dist, "index.html"))) throw new LocalCellError(`No built app at ${dist}: run \`bun run build\` first.`)
  const as = option(args, "--as") ?? "developer"
  if (as !== "developer" && as !== "reviewer") throw new LocalCellError(`--as ${as} refused: use developer or reviewer.`)
  const first = readSession(project, as)
  serveLocal({ hostname: host, dist, port, session: () => readSession(project, as) })
  console.log(`Studio on http://127.0.0.1:${port} over norn dev at ${first.url} (${first.orgId}/${first.environmentId}, as ${first.actor}), live reads on`)
}
