/**
 * Studio over a local `norn dev` cell: one loopback-only server that serves the
 * built app and stands in for the two Studio Worker routes the pages read
 * through, `/api/me` and `/api/host/*`, for the developer `norn dev` serves.
 *
 *   bun run build && bun run local --project ../my-project [--port 5180]
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
 */
import { existsSync, readFileSync, statSync } from "node:fs"
import { isAbsolute, join, relative, resolve } from "node:path"

export interface DevSession {
  readonly url: string
  readonly token: string
  readonly actor: string
  readonly orgId: string
  readonly environmentId: string
  readonly project: string
}

export class LocalCellError extends Error {}

const LOOPBACK_URL = /^http:\/\/(127\.0\.0\.1|\[::1\]):\d{1,5}$/

/** The running `norn dev` session of `project`, as its descriptor (`norn-dev/1`) names it. */
export function readSession(project: string): DevSession {
  const descriptorPath = join(project, ".norn/dev.json")
  if (!existsSync(descriptorPath)) throw new LocalCellError(`No running norn dev in ${project}: start it there first (${descriptorPath} is missing).`)
  const descriptor = JSON.parse(readFileSync(descriptorPath, "utf8")) as Record<string, unknown>
  const scope = descriptor["scope"] as Record<string, unknown> | undefined
  const { url, tokenFile, actor } = descriptor
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
  return { url, token: readFileSync(tokenPath, "utf8").trim(), actor, orgId: scope["orgId"], environmentId: scope["environmentId"], project: name ?? scope["orgId"] }
}

export interface LocalStudio {
  /** The built app (`dist`). */
  readonly dist: string
  /** The session to forward to, read again for every call. */
  readonly session: () => DevSession
  /** This server's own port: the only Host it answers. */
  readonly port: number
  readonly fetcher?: typeof fetch
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
        organization: { id: session.orgId, name: session.project, role: "local developer" },
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
  const first = readSession(project)
  Bun.serve({ hostname: host, port, fetch: (request) => localStudio(request, { dist, port, session: () => readSession(project) }) })
  console.log(`Studio on http://127.0.0.1:${port} over norn dev at ${first.url} (${first.orgId}/${first.environmentId}, as ${first.actor})`)
}
