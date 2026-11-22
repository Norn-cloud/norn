import { createRemoteJWKSet, jwtVerify } from "jose"

export const ACCESS_PREVIEW_URL = "https://studio-preview.norn.cloud"
export const ACCESS_DEMO_ORG = "org_demo"
export const ACCESS_DEMO_EMAIL = "analyst@demo.norn.cloud"

const keys = new Map<string, ReturnType<typeof createRemoteJWKSet>>()

export function validAccessConfig(issuer: string | undefined, audience: string | undefined): boolean {
  return issuer !== undefined && /^https:\/\/[a-z0-9-]+\.cloudflareaccess\.com$/.test(issuer)
    && audience !== undefined && /^[a-f0-9]{64}$/.test(audience)
}

/** Cloudflare's application JWT is the credential. Cookie and actor headers cannot stand in for it. */
export async function verifyPreviewAccess(request: Request, issuer: string, audience: string): Promise<boolean> {
  const assertion = request.headers.get("Cf-Access-Jwt-Assertion")
  if (assertion === null || assertion.length > 8192 || !validAccessConfig(issuer, audience)) return false
  try {
    let jwks = keys.get(issuer)
    if (jwks === undefined) {
      jwks = createRemoteJWKSet(new URL("/cdn-cgi/access/certs", issuer), { timeoutDuration: 4000 })
      keys.set(issuer, jwks)
    }
    await jwtVerify(assertion, jwks, { algorithms: ["RS256"], issuer, audience, requiredClaims: ["exp", "iat"] })
    return true
  } catch {
    return false
  }
}
