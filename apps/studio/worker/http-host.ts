/**
 * The Norn cell host reached over HTTPS instead of a service binding: a self-hosted host such as
 * the Rivet runners (D131, `crates/norn-cell-rivet`), whose router serves the same `/v1/...` API as
 * the cell Worker. Every request Studio builds for `https://norn-host/...` goes to the same path and
 * query on `NORN_HOST_URL`; method, headers (the host token) and body are kept, and a live socket's
 * upgrade passes through. The host authenticates each call by its host token, as it does behind a
 * binding.
 *
 * Unlike a binding, the host is across the internet, so every call is bounded: the host has
 * {@link RESPONSE_DEADLINE_MS} to start answering a request and {@link HANDSHAKE_DEADLINE_MS} to
 * answer a socket upgrade. The deadline covers the wait for the response head only; a body
 * already streaming is not cut, and the caller's own cancellation still reaches the host while
 * the body is read. An unreachable host answers `502 host.unreachable`, a late one
 * `504 host.timeout`. Nothing here retries: a write that timed out or lost its connection may or
 * may not have been applied, so its refusal is not retryable and says to read back first.
 *
 * Transport seam (S1, a self-hosted Studio next to its host): the `fetcher` is the transport.
 * A private endpoint is reached by passing a fetcher that carries internal TLS (or a private
 * network's own transport) for the same https origin; the origin check itself stays https-only.
 */

/** An https origin, exactly as written: no path, query, fragment or credentials. */
export function hostOrigin(value: string): string {
  let url: URL
  try { url = new URL(value) } catch { throw new Error("NORN_HOST_URL must be an https origin") }
  if (url.protocol !== "https:" || url.origin !== value || url.username !== "" || url.password !== "") {
    throw new Error("NORN_HOST_URL must be an https origin without a path, query or credentials")
  }
  return url.origin
}

/** How long the host has to answer a request's head (headers), in milliseconds. */
export const RESPONSE_DEADLINE_MS = 30_000
/** How long the host has to answer a socket upgrade, in milliseconds. */
export const HANDSHAKE_DEADLINE_MS = 10_000

/** Deadlines for {@link httpHost}; tests shorten them. */
export interface HostDeadlines {
  readonly responseMs: number
  readonly handshakeMs: number
}

const DEADLINES: HostDeadlines = { responseMs: RESPONSE_DEADLINE_MS, handshakeMs: HANDSHAKE_DEADLINE_MS }
const SAFE = new Set(["GET", "HEAD", "OPTIONS"])

/** A `norn/refusal@2` answer from this adapter, in the shape the host's own refusals have. */
function hostRefusal(status: 502 | 504, code: string, retryable: boolean, message: string, explanation: string): Response {
  return new Response(JSON.stringify({ format: "norn/refusal@2", retryable, reason: null, code, category: "host", message, explanation }), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store" },
  })
}

/** The refusal for a call the host did not answer: `timedOut` after its deadline, else unreachable. */
export function unanswered(method: string, timedOut: boolean): Response {
  const safe = SAFE.has(method)
  const unknown = "The write may or may not have been applied, and it was not retried: read the data back before trying again."
  if (timedOut) {
    return hostRefusal(504, "host.timeout", safe, "The Norn host did not answer in time.", safe ? "Retry the request." : unknown)
  }
  return hostRefusal(502, "host.unreachable", safe, "The Norn host could not be reached.", safe ? "Retry the request." : unknown)
}

/** A `Fetcher` that forwards to the host at `origin` (see {@link hostOrigin}), within its deadlines. */
export function httpHost(origin: string, fetcher: typeof fetch = fetch, deadlines: HostDeadlines = DEADLINES): Fetcher {
  const base = hostOrigin(origin)
  return {
    fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = new Request(input, init)
      const { pathname, search } = new URL(request.url)
      const upgrade = request.headers.get("upgrade")?.toLowerCase() === "websocket"
      const limit = upgrade ? deadlines.handshakeMs : deadlines.responseMs
      // One controller for the whole exchange: the caller's cancellation aborts it at any time
      // (the body included); the deadline only until the response head arrives.
      const controller = new AbortController()
      const caller = request.signal
      const cancel = () => controller.abort(caller.reason)
      if (caller.aborted) cancel()
      else caller.addEventListener("abort", cancel, { once: true })
      let timedOut = false
      const timer = setTimeout(() => {
        timedOut = true
        controller.abort(new Error("the Norn host did not answer in time"))
      }, limit)
      // A redirect from the host is its answer, never followed with the host token attached.
      const outbound: Request = new Request(`${base}${pathname}${search}`, new Request(request, { redirect: "manual", signal: controller.signal }))
      try {
        return await fetcher(outbound)
      } catch (error) {
        // The caller went away: nobody is waiting for a refusal.
        if (caller.aborted && !timedOut) throw error
        return unanswered(request.method, timedOut)
      } finally {
        clearTimeout(timer)
      }
    },
    connect: () => { throw new Error("The HTTPS cell host takes requests, not sockets") },
  } as unknown as Fetcher
}

/** The cell host a request uses: the service binding when there is one, else `NORN_HOST_URL`. */
export function cellHostOf(env: { readonly NORN_HOST?: Fetcher; readonly NORN_HOST_URL?: string }, fetcher: typeof fetch = fetch): Fetcher | undefined {
  if (env.NORN_HOST !== undefined) {
    if (env.NORN_HOST_URL !== undefined) throw new Error("NORN_HOST and NORN_HOST_URL are both set; a stage has one cell host")
    return env.NORN_HOST
  }
  return env.NORN_HOST_URL === undefined ? undefined : httpHost(env.NORN_HOST_URL, fetcher)
}
