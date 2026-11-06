import { constantTimeEqual, verifyPassword as verifyScrypt } from "better-auth/crypto"

/**
 * CONTINGENCY (#817, not merged): password hashing with WebCrypto PBKDF2-SHA256 instead of
 * Better Auth's scrypt, for a Workers plan whose CPU limit scrypt exceeds. New hashes are
 * `pbkdf2-sha256$<iterations>$<salt>$<key>` (base64url, 16-byte salt, 32-byte key); existing
 * scrypt hashes (`<salt hex>:<key hex>`) still verify, so current accounts keep working.
 */
export const PBKDF2_PREFIX = "pbkdf2-sha256"
/** OWASP 2023 minimum for PBKDF2-HMAC-SHA256; a stored hash below it never verifies. */
export const MIN_ITERATIONS = 600_000
/** Bound on a stored iteration count, so a tampered row cannot make verification unbounded. */
const MAX_ITERATIONS = 10_000_000

const encoder = new TextEncoder()
const b64 = (bytes: Uint8Array) => btoa(String.fromCharCode(...bytes)).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "")
const unb64 = (text: string) => Uint8Array.from(atob(text.replaceAll("-", "+").replaceAll("_", "/")), (char) => char.charCodeAt(0))

async function derive(password: string, salt: Uint8Array<ArrayBuffer>, iterations: number): Promise<Uint8Array> {
  const key = await crypto.subtle.importKey("raw", encoder.encode(password.normalize("NFKC")), "PBKDF2", false, ["deriveBits"])
  return new Uint8Array(await crypto.subtle.deriveBits({ name: "PBKDF2", hash: "SHA-256", salt, iterations }, key, 256))
}

export async function hashPassword(password: string, iterations = MIN_ITERATIONS): Promise<string> {
  const salt = crypto.getRandomValues(new Uint8Array(16))
  return `${PBKDF2_PREFIX}$${iterations}$${b64(salt)}$${b64(await derive(password, salt, iterations))}`
}

export async function verifyPassword({ hash, password }: { hash: string; password: string }): Promise<boolean> {
  if (!hash.startsWith(`${PBKDF2_PREFIX}$`)) return verifyScrypt({ hash, password })
  const [, rounds, salt, key, ...rest] = hash.split("$")
  if (rest.length > 0 || rounds === undefined || salt === undefined || key === undefined || !/^\d+$/.test(rounds)) return false
  const iterations = Number(rounds)
  if (iterations < MIN_ITERATIONS || iterations > MAX_ITERATIONS) return false
  let expected: Uint8Array
  let saltBytes: Uint8Array<ArrayBuffer>
  try {
    expected = unb64(key)
    saltBytes = unb64(salt)
  } catch {
    return false
  }
  if (saltBytes.length !== 16 || expected.length !== 32) return false
  return constantTimeEqual(await derive(password, saltBytes, iterations), expected)
}
