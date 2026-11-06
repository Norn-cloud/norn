import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { HOST_TOKEN_SECONDS, hostToken, resolveActor } from "../actor.ts"
import { createAuth } from "../auth.ts"
import worker, { type Env } from "../index.ts"

const ORIGIN = "https://studio-preview.norn.cloud"
const MIGRATION: string = readFileSync(join(import.meta.dir, "../migrations/0001_better_auth.sql"), "utf8")

function stage(host?: (request: Request) => Response) {
  const db = new Database(":memory:")
  db.exec(MIGRATION)
  const seen: Request[] = []
  const env: Env = {
    ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher,
    AUTH_DB: db as unknown as D1Database,
    BETTER_AUTH_SECRET: "t".repeat(40),
    BETTER_AUTH_URL: ORIGIN,
    ...host === undefined ? {} : { NORN_HOST: { fetch: async (request: Request) => { seen.push(request); return host(request) } } as unknown as Fetcher },
  }
  return { env, db, seen, auth: createAuth({ database: db, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN }) }
}

let address = 0
const call = (env: Env, path: string, init: RequestInit = {}) => {
  const headers = new Headers(init.headers)
  headers.set("cf-connecting-ip", `198.51.100.${++address % 250}`)
  return worker.fetch(new Request(`${ORIGIN}${path}`, { ...init, headers }) as Parameters<typeof worker.fetch>[0], env)
}
const signUp = async (env: Env, email: string) => {
  const response = await call(env, "/api/auth/sign-up/email", { method: "POST", headers: { "content-type": "application/json", origin: ORIGIN }, body: JSON.stringify({ email, password: "correct horse battery", name: "Ada" }) })
  return response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")
}
const claims = (token: string) => JSON.parse(Buffer.from(token.split(".")[1] ?? "", "base64url").toString()) as Record<string, unknown>

test("a password session resolves to a user actor in its organization", async () => {
  const { env, auth } = stage()
  const cookie = await signUp(env, "ada@example.com")
  const actor = await resolveActor(new Request(ORIGIN, { headers: { cookie } }), auth)
  expect(actor).toMatchObject({ actorKind: "user", role: "owner", authMethod: "password" })
  expect(actor?.orgId).toBeString()
  expect(await resolveActor(new Request(ORIGIN), auth)).toBeNull()
})

test("the host token carries exactly the actor, for the host's audience, for five minutes", async () => {
  const { env, auth } = stage()
  const cookie = await signUp(env, "bo@example.com")
  const actor = await resolveActor(new Request(ORIGIN, { headers: { cookie } }), auth)
  if (actor === null) throw new Error("no actor")
  const body = claims(await hostToken(auth, actor, 1_800_000_000))
  expect(body).toMatchObject({ iss: ORIGIN, aud: "norn-host", sub: actor.subjectId, iat: 1_800_000_000, exp: 1_800_000_000 + HOST_TOKEN_SECONDS })
  expect(body.norn).toEqual({ kind: "user", org: actor.orgId, role: "owner", amr: "password" })
})

test("an organization API key acts as a service, never with an approving role", async () => {
  const { env, auth } = stage()
  const cookie = await signUp(env, "cy@example.com")
  const owner = await resolveActor(new Request(ORIGIN, { headers: { cookie } }), auth)
  const create = (metadata?: Record<string, string>) => auth.api.createApiKey({ body: { name: "svc", organizationId: owner?.orgId, ...metadata === undefined ? {} : { metadata } }, headers: new Headers({ cookie }) })
  const viewer = await create()
  const service = await resolveActor(new Request(ORIGIN, { headers: { "x-api-key": viewer.key } }), auth)
  expect(service).toEqual({ actorKind: "service", subjectId: viewer.id, orgId: owner?.orgId ?? "", role: "viewer", authMethod: "api_key" })
  const admin = await create({ role: "admin" })
  expect(await resolveActor(new Request(ORIGIN, { headers: { "x-api-key": admin.key } }), auth)).toBeNull()
  expect(await resolveActor(new Request(ORIGIN, { headers: { "x-api-key": "not-a-key" } }), auth)).toBeNull()
})

test("Norn calls go to the host with the host token only, never the client's cookie or key", async () => {
  const { env, seen } = stage(() => Response.json({ ok: true }))
  const cookie = await signUp(env, "di@example.com")
  const response = await call(env, "/api/host/v1/defs?orgId=o", { headers: { cookie, authorization: "Bearer forged", accept: "application/json" } })
  expect(response.status).toBe(200)
  const forwarded = seen[0]
  expect(forwarded?.url).toBe("https://norn-host/v1/defs?orgId=o")
  expect(forwarded?.headers.get("cookie")).toBeNull()
  expect(forwarded?.headers.get("authorization")).not.toBe("Bearer forged")
  expect(claims(forwarded?.headers.get("authorization")?.slice("Bearer ".length) ?? "").aud).toBe("norn-host")
})

test("without a session or a connected host, Norn calls refuse", async () => {
  expect((await call(stage(() => new Response()).env, "/api/host/v1/defs")).status).toBe(401)
  const { env } = stage()
  const cookie = await signUp(env, "ed@example.com")
  expect((await call(env, "/api/host/v1/defs", { headers: { cookie } })).status).toBe(503)
})
