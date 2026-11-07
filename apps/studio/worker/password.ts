import { scryptAsync } from "@noble/hashes/scrypt.js"
import { bytesToHex, hexToBytes } from "@noble/hashes/utils.js"

/**
 * Better Auth's password hash in pure JavaScript, for runtimes without `node:crypto.scrypt`
 * (celld v0.6.3). The parameters and the stored form are Better Auth's own
 * (`@better-auth/utils/password`): scrypt N=16384, r=16, p=1, dkLen=64 over the NFKC
 * password, salted with the 32-character hex salt string, stored as `salt:hex(key)`.
 * The two implementations are byte-identical, so either verifies the other's hashes.
 * Cloudflare keeps the native path: pure-JS scrypt there costs far more CPU time.
 */

const PARAMS = { N: 16384, r: 16, p: 1, dkLen: 64, maxmem: 128 * 16384 * 16 * 2 } as const

/** Which scrypt the deployment runs; a deploy variable picks it, the default is native. */
export type PasswordScrypt = "native" | "js"

export function passwordScrypt(value: string | undefined): PasswordScrypt {
  if (value === undefined || value === "native") return "native"
  if (value === "js") return "js"
  throw new Error(`AUTH_SCRYPT must be "native" or "js", got ${JSON.stringify(value)}`)
}

const key = (password: string, salt: string) => scryptAsync(password.normalize("NFKC"), salt, PARAMS)

export async function hashPassword(password: string): Promise<string> {
  const salt = bytesToHex(crypto.getRandomValues(new Uint8Array(16)))
  return `${salt}:${bytesToHex(await key(password, salt))}`
}

export async function verifyPassword({ hash, password }: { hash: string; password: string }): Promise<boolean> {
  const [salt, stored] = hash.split(":")
  if (!salt || !stored) throw new Error("Invalid password hash")
  const expected = hexToBytes(stored)
  const actual = await key(password, salt)
  if (actual.length !== expected.length) return false
  let diff = 0
  for (let index = 0; index < actual.length; index++) diff |= (actual[index] ?? 0) ^ (expected[index] ?? 0)
  return diff === 0
}
