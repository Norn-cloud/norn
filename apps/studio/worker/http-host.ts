/**
 * The Norn cell host reached over HTTPS instead of a service binding: a self-hosted host such as
 * the Rivet runners (D131, `crates/norn-cell-rivet`), whose router serves the same `/v1/...` API as
 * the cell Worker. Every request Studio builds for `https://norn-host/...` goes to the same path and
 * query on `NORN_HOST_URL`; method, headers (the host token) and body are kept, and a live socket's
 * upgrade passes through. The host authenticates each call by its host token, as it does behind a
 * binding.
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

/** A `Fetcher` that forwards to the host at `origin` (see {@link hostOrigin}). */
export function httpHost(origin: string, fetcher: typeof fetch = fetch): Fetcher {
  const base = hostOrigin(origin)
  return {
    fetch: (input: RequestInfo | URL, init?: RequestInit) => {
      const request = new Request(input, init)
      const { pathname, search } = new URL(request.url)
      // A redirect from the host is its answer, never followed with the host token attached.
      return fetcher(new Request(`${base}${pathname}${search}`, new Request(request, { redirect: "manual" })))
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
