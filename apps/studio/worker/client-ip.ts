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

/**
 * The configured header. Unset refuses as any unknown value does: no deployment
 * gets a default, so a stale or hand-written config cannot fall back to a header
 * its clients write (#1328 r2).
 */
export function clientIpHeader(value: string | undefined): ClientIpHeader {
  if (value === undefined) throw new Error(`STUDIO_CLIENT_IP_HEADER is required: one of ${CLIENT_IP_HEADERS.join(", ")}, the header this deployment's edge sets`)
  const header = CLIENT_IP_HEADERS.find((known) => known === value)
  if (header === undefined) throw new Error(`STUDIO_CLIENT_IP_HEADER must be one of ${CLIENT_IP_HEADERS.join(", ")}, got ${JSON.stringify(value)}`)
  return header
}
