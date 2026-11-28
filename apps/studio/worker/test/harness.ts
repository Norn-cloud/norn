import { Database, type SQLQueryBindings } from "bun:sqlite"
import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import worker, { type Env } from "../index.ts"

export const DEFAULT_ORIGIN = "https://studio-preview.norn.cloud"
export const DEFAULT_SECRET = "t".repeat(40)
export const DEFAULT_PASSWORD = "correct horse battery"
export const BASE_MIGRATIONS = ["0001_better_auth.sql", "0002_workspace_provisioning.sql"] as const

const MIGRATIONS_DIR = join(import.meta.dir, "../migrations")
const migrationCache = new Map<string, string>()

export function readMigration(name: string): string {
  let sql = migrationCache.get(name)
  if (sql === undefined) {
    sql = readFileSync(join(MIGRATIONS_DIR, name), "utf8")
    migrationCache.set(name, sql)
  }
  return sql
}

export function createMigratedDb(migrations: readonly string[] | "all" = BASE_MIGRATIONS, filename = ":memory:"): Database {
  const db = new Database(filename)
  const files = migrations === "all"
    ? readdirSync(MIGRATIONS_DIR).filter((name) => name.endsWith(".sql")).sort()
    : migrations
  for (const name of files) db.exec(readMigration(name))
  return db
}

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

/** Wrap a `bun:sqlite` database in a D1-compatible adapter supporting `first`/`all`/`run`/`batch` (including `RETURNING`). */
export function createSqliteD1(
  db: Database,
  options: {
    readonly failOn?: () => RegExp | null
    readonly clearFail?: () => void
    readonly onAll?: (sql: string) => void
  } = {},
): D1Database {
  type Bound = { readonly sql: string; readonly args: SQLQueryBindings[] }
  const checkFail = (sql: string) => {
    const pattern = options.failOn?.()
    if (pattern?.test(sql)) {
      options.clearFail?.()
      throw new Error("D1_ERROR: injected failure")
    }
  }
  const statement = (sql: string, args: SQLQueryBindings[] = []) => ({
    sql,
    args,
    bind: (...next: SQLQueryBindings[]) => statement(sql, next),
    first: async <T = unknown>() => {
      checkFail(sql)
      return (db.query(sql).get(...args) as T | null) ?? null
    },
    all: async <T = unknown>() => {
      checkFail(sql)
      const results = db.query(sql).all(...args) as T[]
      options.onAll?.(sql)
      return { results }
    },
    run: async () => {
      checkFail(sql)
      const outcome = db.query(sql).run(...args)
      return { success: true, meta: { changes: outcome.changes } }
    },
  })
  const batch = async (list: readonly Bound[]) => {
    const results: { results: unknown[] }[] = []
    db.transaction(() => {
      for (const s of list) {
        checkFail(s.sql)
        if (/\breturning\b/i.test(s.sql)) {
          results.push({ results: db.query(s.sql).all(...s.args) })
        } else {
          db.query(s.sql).run(...s.args)
          results.push({ results: [] })
        }
      }
    })()
    return results
  }
  return { prepare: (sql: string) => statement(sql), batch } as unknown as D1Database
}

const activeSqlRaces = new WeakSet<Database>()

/** Run `before` once immediately ahead of the first prepared SQL statement matching `pattern`, restoring `db.prepare` in `finally`. */
export async function withSqlRace<T>(
  db: Database,
  pattern: string | RegExp,
  before: (run: (sql: string, ...params: SQLQueryBindings[]) => void) => void,
  action: () => Promise<T>,
): Promise<{ readonly result: T; readonly raced: boolean }> {
  if (activeSqlRaces.has(db)) {
    throw new Error("withSqlRace: overlapping race hook on the same database")
  }
  activeSqlRaces.add(db)
  const origDescriptor = Object.getOwnPropertyDescriptor(db, "prepare")
  const origPrepare = db.prepare
  const runRaw = (sql: string, ...params: SQLQueryBindings[]) => {
    origPrepare.call(db, sql).run(...params)
  }
  const matches = typeof pattern === "string"
    ? (sql: string) => sql.toLowerCase().includes(pattern.toLowerCase())
    : (sql: string) => pattern.test(sql)
  let raced = false
  try {
    db.prepare = ((sql: string, ...rest: unknown[]) => {
      if (!raced && matches(sql)) {
        raced = true
        before(runRaw)
      }
      return (origPrepare as (this: Database, ...args: unknown[]) => ReturnType<typeof origPrepare>).call(db, sql, ...rest)
    }) as typeof db.prepare
    return { result: await action(), raced }
  } finally {
    if (origDescriptor !== undefined) {
      Object.defineProperty(db, "prepare", origDescriptor)
    } else {
      delete (db as Partial<Database>).prepare
    }
    activeSqlRaces.delete(db)
  }
}

export const cookieOf = (response: Response): string =>
  response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")

export const claimsOf = <T = Record<string, unknown>>(token: string): T =>
  JSON.parse(Buffer.from(token.split(".")[1] ?? "", "base64url").toString()) as T

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

  const post = (env: Env, path: string, body: unknown, headers: HeadersInit = {}) =>
    call(env, path, { method: "POST", body: JSON.stringify(body), headers: { "content-type": "application/json", origin, ...headers } })

  const postJson = (env: Env, path: string, body: unknown, headers: HeadersInit = {}) =>
    call(env, path, { method: "POST", body: JSON.stringify(body), headers: { "content-type": "application/json", accept: "application/json", ...headers } })

  const mutate = (env: Env, method: "POST" | "PATCH" | "DELETE", path: string, body?: unknown, headers: HeadersInit = {}) =>
    call(env, path, {
      method,
      ...body === undefined ? {} : { body: JSON.stringify(body) },
      headers: { origin, ...body === undefined ? {} : { "content-type": "application/json" }, ...headers },
    })

  const enroll = async (env: Env, email: string, name = email.split("@")[0] ?? "User", headers: HeadersInit = {}, password = DEFAULT_PASSWORD) => {
    const signUp = await post(env, "/api/auth/sign-up/email", { email, password, name }, headers)
    if (signUp.status !== 200) return signUp
    return post(env, "/api/auth/sign-in/email", { email, password }, headers)
  }

  return { nextIp, call, post, postJson, mutate, enroll }
}
