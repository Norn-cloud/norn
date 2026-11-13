/**
 * The header Better Auth reads a client's address from, for its rate limits.
 * It must be one the deployment's own edge sets and never passes through from
 * the client: Cloudflare sets `cf-connecting-ip`; Traefik (with no trusted
 * forwarded-header sources, its default) drops a client's `X-Real-Ip` and sets
 * it to the connection's address. `X-Forwarded-For` is never read: a client
 * writes its first entry.
 */
export const CLIENT_IP_HEADERS = ["cf-connecting-ip", "x-real-ip"] as const
export type ClientIpHeader = (typeof CLIENT_IP_HEADERS)[number]

/** The configured header, `cf-connecting-ip` when unset; any other value refuses. */
export function clientIpHeader(value: string | undefined): ClientIpHeader {
  if (value === undefined) return "cf-connecting-ip"
  const header = CLIENT_IP_HEADERS.find((known) => known === value)
  if (header === undefined) throw new Error(`STUDIO_CLIENT_IP_HEADER must be one of ${CLIENT_IP_HEADERS.join(", ")}, got ${JSON.stringify(value)}`)
  return header
}
