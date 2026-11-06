import { expect, test } from "bun:test"
import { hashPassword as scryptHash } from "better-auth/crypto"
import { MIN_ITERATIONS, PBKDF2_PREFIX, hashPassword, verifyPassword } from "../password.ts"

test("a new hash is versioned PBKDF2-SHA256 with the OWASP iteration floor and a 16-byte salt", async () => {
  const hash = await hashPassword("correct horse battery")
  const [prefix, rounds, salt, key, ...rest] = hash.split("$")
  expect([prefix, Number(rounds), rest.length]).toEqual([PBKDF2_PREFIX, MIN_ITERATIONS, 0])
  expect(Buffer.from(salt ?? "", "base64url")).toHaveLength(16)
  expect(Buffer.from(key ?? "", "base64url")).toHaveLength(32)
  expect(await hashPassword("correct horse battery")).not.toBe(hash)
  expect(await verifyPassword({ hash, password: "correct horse battery" })).toBe(true)
  expect(await verifyPassword({ hash, password: "correct horse batterY" })).toBe(false)
}, 30_000)

test("existing scrypt hashes still verify, so current accounts keep working", async () => {
  const legacy = await scryptHash("correct horse battery")
  expect(legacy.startsWith(PBKDF2_PREFIX)).toBe(false)
  expect(await verifyPassword({ hash: legacy, password: "correct horse battery" })).toBe(true)
  expect(await verifyPassword({ hash: legacy, password: "wrong horse battery" })).toBe(false)
}, 30_000)

test("a weakened, tampered or malformed stored hash never verifies", async () => {
  const weak = await hashPassword("pw-correct-horse", 1_000)
  expect(await verifyPassword({ hash: weak, password: "pw-correct-horse" })).toBe(false)
  const good = await hashPassword("pw-correct-horse")
  const [prefix, rounds, salt, key] = good.split("$")
  const cases = [
    `${prefix}$${rounds}$${salt}$${key}$extra`,
    `${prefix}$99999999$${salt}$${key}`,
    `${prefix}$6e5$${salt}$${key}`,
    `${prefix}$${rounds}$${salt}`,
    `${prefix}$${rounds}$not*base64$${key}`,
    `${prefix}$${rounds}$${salt}$${(key ?? "").slice(0, -4)}`,
  ]
  for (const hash of cases) expect(await verifyPassword({ hash, password: "pw-correct-horse" })).toBe(false)
}, 60_000)
