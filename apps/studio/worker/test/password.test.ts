import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { hashPassword as nativeHash, verifyPassword as nativeVerify } from "@better-auth/utils/password"
import { mkdtempSync, readFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { authOptions } from "../auth.ts"
import worker, { type Env } from "../index.ts"
import { hashPassword, passwordScrypt, verifyPassword } from "../password.ts"

// A fixed vector (Python hashlib.scrypt, same parameters); the password is NFKC-normalized first.
const VECTOR = {
  password: "Ｃorrect horse battery staple",
  hash: "00112233445566778899aabbccddeeff:a746406d440ca001622bf6461a9a2b8ade6c697e54fb8f6332ecdfccae48539151b139c3c9b10d6a23f119cfd2bd39ec66c1d3dc38ff96b349f1ce63f71daaf7",
}
// Stored by the preview Studio on Cloudflare (native node:crypto) for a throwaway account,
// deleted after the hash was read; the password is a test value, not a secret.
const CLOUDFLARE = {
  password: "celld-scrypt-vector-not-a-secret",
  hash: "242bdfe46aba092511ba8c29e0d7c604:43e02b01eb60c959c05d5aa5fb32b89ecbcbcaa5e7dd3ad9b357a93ebf6ba1df8fe0919efeee421dfcf6a7fa7e002063ea31f4e866750af0b3e3597a3f60f7b7",
}

test("the pure-JS scrypt is byte-identical to the fixed vector and to Cloudflare's stored hash", async () => {
  for (const { password, hash } of [VECTOR, CLOUDFLARE]) {
    expect(await verifyPassword({ hash, password })).toBe(true)
    expect(await verifyPassword({ hash, password: `${password}x` })).toBe(false)
    expect(await nativeVerify(hash, password)).toBe(true)
  }
})

test("each implementation verifies the other's hashes", async () => {
  const fromJs = await hashPassword("a password of fourteen")
  const fromNative = await nativeHash("a password of fourteen")
  expect(fromJs).toMatch(/^[0-9a-f]{32}:[0-9a-f]{128}$/)
  expect(await nativeVerify(fromJs, "a password of fourteen")).toBe(true)
  expect(await verifyPassword({ hash: fromNative, password: "a password of fourteen" })).toBe(true)
  expect(await verifyPassword({ hash: fromNative, password: "another password" })).toBe(false)
  await expect(verifyPassword({ hash: "no-separator", password: "x" })).rejects.toThrow("Invalid password hash")
})

test("AUTH_SCRYPT selects the implementation; Cloudflare's default stays native", () => {
  expect(passwordScrypt(undefined)).toBe("native")
  expect(passwordScrypt("native")).toBe("native")
  expect(passwordScrypt("js")).toBe("js")
  expect(() => passwordScrypt("JS")).toThrow("AUTH_SCRYPT")
  const config = { database: new Database(":memory:") as never, secret: "t".repeat(40), baseURL: "https://studio-preview.norn.cloud" }
  expect(authOptions(config).emailAndPassword).not.toHaveProperty("password")
  expect(authOptions({ ...config, scrypt: "native" }).emailAndPassword).not.toHaveProperty("password")
  expect(authOptions({ ...config, scrypt: "js" }).emailAndPassword).toMatchObject({ password: { hash: hashPassword, verify: verifyPassword } })
})

test("an account stored on Cloudflare signs in on a pure-JS deployment", async () => {
  const ORIGIN = "https://studio-preview.norn.cloud"
  // One database file, opened twice: Workers cache one Better Auth instance per database binding.
  const file = join(mkdtempSync(join(tmpdir(), "studio-scrypt-")), "auth.sqlite")
  const db = new Database(file)
  db.exec(readFileSync(join(import.meta.dir, "../migrations/0001_better_auth.sql"), "utf8"))
  db.exec(readFileSync(join(import.meta.dir, "../migrations/0002_workspace_provisioning.sql"), "utf8"))
  const assets = { fetch: async () => new Response("studio") } as unknown as Fetcher
  const env = (database: Database, scrypt?: string): Env => ({ ASSETS: assets, AUTH_DB: database as unknown as D1Database, BETTER_AUTH_SECRET: "t".repeat(40), BETTER_AUTH_URL: ORIGIN, ...scrypt === undefined ? {} : { AUTH_SCRYPT: scrypt } })
  const post = (target: Env, path: string, body: unknown) => worker.fetch(new Request(`${ORIGIN}${path}`, {
    method: "POST", body: JSON.stringify(body), headers: { "content-type": "application/json", origin: ORIGIN, "cf-connecting-ip": "198.51.100.7" },
  }) as Parameters<typeof worker.fetch>[0], target)
  const email = "moved@example.com"
  expect((await post(env(db), "/api/auth/sign-up/email", { email, password: "a native password", name: "Moved" })).status).toBe(200)
  // The row as Cloudflare stored it, read by a pure-JS deployment of the same database.
  db.query("update account set password = ? where providerId = 'credential'").run(CLOUDFLARE.hash)
  const js = env(new Database(file), "js")
  expect((await post(js, "/api/auth/sign-in/email", { email, password: CLOUDFLARE.password })).status).toBe(200)
  expect((await post(js, "/api/auth/sign-in/email", { email, password: "a native password" })).status).toBe(401)
}, 120_000) // three scrypt runs, two in pure JS
