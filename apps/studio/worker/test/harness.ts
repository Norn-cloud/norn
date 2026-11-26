import { Database } from "bun:sqlite"
import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import worker, { type Env } from "../index.ts"

export const DEFAULT_ORIGIN = "https://studio-preview.norn.cloud"
export const DEFAULT_SECRET = "t".repeat(40)
export const DEFAULT_PASSWORD = "correct horse battery"
export const BASE_MIGRATIONS = ["0001_better_auth.sql", "0002_workspace_provisioning.sql"] as const

const MIGRATIONS_DIR = join(import.meta.dir, "../migrations")
const migrationCache = new Map<string, string>()

/** Read a Studio Worker SQL migration by file name (cached across tests). */
export function readMigration(name: string): string {
  let sql = migrationCache.get(name)
  if (sql === undefined) {
    sql = readFileSync(join(MIGRATIONS_DIR, name), "utf8")
    migrationCache.set(name, sql)
  }
  return sql
}

/** Open a bun:sqlite database and apply either the listed migrations or every `.sql` migration in order. */
export function createMigratedDb(migrations: readonly string[] | "all" = BASE_MIGRATIONS, filename = ":memory:"): Database {
  const db = new Database(filename)
  const files = migrations === "all"
    ? readdirSync(MIGRATIONS_DIR).filter((name) => name.endsWith(".sql")).sort()
    : migrations
  for (const name of files) db.exec(readMigration(name))
  return db
}

/** Build a Worker `Env` backed by a migrated `bun:sqlite` database and stubbed static assets. */
export function createWorkerEnv(options: {
  readonly db?: Database
  readonly migrations?: readonly string[] | "all"
  readonly origin?: string
  readonly secret?: string
  readonly assetsHtml?: string
  readonly overrides?: Partial<Env>
} = {}): { readonly db: Database; readonly env: Env } {
  const db = options.db ?? createMigratedDb(options.migrations)
  const assetsHtml = options.assetsHtml ?? "<!doctype html>studio"
  const env: Env = {
    ASSETS: { fetch: async () => new Response(assetsHtml) } as unknown as Fetcher,
    AUTH_DB: db as unknown as D1Database,
    BETTER_AUTH_SECRET: options.secret ?? DEFAULT_SECRET,
    BETTER_AUTH_URL: options.origin ?? DEFAULT_ORIGIN,
    STUDIO_CLIENT_IP_HEADER: "cf-connecting-ip",
    ...options.overrides,
  }
  return { db, env }
}

export const cookieOf = (response: Response): string =>
  response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")

export const claimsOf = <T = Record<string, unknown>>(token: string): T =>
  JSON.parse(Buffer.from(token.split(".")[1] ?? "", "base64url").toString()) as T

/** Create request helpers bound to `origin` with rotating client IPs for Worker rate-limit isolation. */
export function createWorkerClient(origin = DEFAULT_ORIGIN, options: {
  readonly ipPrefix?: string
  readonly redirect?: RequestInit["redirect"]
} = {}) {
  const ipPrefix = options.ipPrefix ?? "198.51.100"
  let address = 0

  const nextIp = () => `${ipPrefix}.${++address % 250}`

  const call = (env: Env, path: string, init: RequestInit = {}) => {
    const headers = new Headers(init.headers)
    if (!headers.has("cf-connecting-ip")) headers.set("cf-connecting-ip", nextIp())
    const url = path.startsWith("http") ? path : `${origin}${path}`
    return worker.fetch(new Request(url, {
      ...options.redirect === undefined ? {} : { redirect: options.redirect },
      ...init,
      headers,
    }) as Parameters<typeof worker.fetch>[0], env)
  }

  /** JSON POST with `origin` set by default (can be overridden via `headers`). */
  const post = (env: Env, path: string, body: unknown, headers: HeadersInit = {}) =>
    call(env, path, { method: "POST", body: JSON.stringify(body), headers: { "content-type": "application/json", origin, ...headers } })

  /** JSON POST without an implicit `origin` header (for server-to-server endpoints like OAuth DCR). */
  const postJson = (env: Env, path: string, body: unknown, headers: HeadersInit = {}) =>
    call(env, path, { method: "POST", body: JSON.stringify(body), headers: { "content-type": "application/json", accept: "application/json", ...headers } })

  /** State-changing request (`POST`/`PATCH`/`DELETE`) with `origin` and optional JSON body. */
  const mutate = (env: Env, method: "POST" | "PATCH" | "DELETE", path: string, body?: unknown, headers: HeadersInit = {}) =>
    call(env, path, {
      method,
      ...body === undefined ? {} : { body: JSON.stringify(body) },
      headers: { origin, ...body === undefined ? {} : { "content-type": "application/json" }, ...headers },
    })

  /** Sign up (no session is issued), then sign in with password; returns the sign-in response (or failed sign-up response). */
  const enroll = async (env: Env, email: string, name = email.split("@")[0] ?? "User", headers: HeadersInit = {}, password = DEFAULT_PASSWORD) => {
    const signUp = await post(env, "/api/auth/sign-up/email", { email, password, name }, headers)
    if (signUp.status !== 200) return signUp
    return post(env, "/api/auth/sign-in/email", { email, password }, headers)
  }

  return { nextIp, call, post, postJson, mutate, enroll }
}
