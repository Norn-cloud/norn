import { expect, test } from "bun:test"
import * as workers from "@distilled.cloud/cloudflare/workers"
import { call, closeTail, d1Databases, openTail, workerDeployments, workerDomains, workerSettings, type CloudflareClient } from "../cloudflare-api.ts"
import { NORN_CLOUDFLARE_ACCOUNT_ID, PROD_DOMAINS, PROD_RECORDED_DEMO, requireWorkerScript } from "../deploy-target.ts"
import { readDeployments, recipeIssues, tailDuring } from "../scripts/prod-parity.ts"
import { discoverD1, SEED_EXIT } from "../scripts/seed-demo-org.ts"

const API = `https://api.cloudflare.com/client/v4/accounts/${NORN_CLOUDFLARE_ACCOUNT_ID}`
const credentials = { accountId: NORN_CLOUDFLARE_ACCOUNT_ID, apiToken: "workers-token" }
const worker = PROD_RECORDED_DEMO.worker

interface Sent { readonly method: string; readonly url: string; readonly authorization: string | null; readonly contentType: string | null; readonly body: string }
type Reply = { readonly status?: number; readonly body: unknown }
const envelope = (result: unknown, resultInfo?: unknown): Reply => ({ body: { success: true, errors: [], messages: [], result, ...(resultInfo ? { result_info: resultInfo } : {}) } })
const refusal = (status: number, code: number, message: string): Reply => ({ status, body: { success: false, errors: [{ code, message }], messages: [], result: null } })

/** A client whose fetch records every request and answers from `replies`, in order. */
function recording(...replies: Reply[]): { client: CloudflareClient; sent: Sent[] } {
  const sent: Sent[] = []
  const send = async (url: RequestInfo | URL, init?: RequestInit) => {
    const headers = new Headers(init?.headers)
    sent.push({ method: init?.method ?? "GET", url: String(url), authorization: headers.get("authorization"), contentType: headers.get("content-type"), body: typeof init?.body === "string" ? init.body : init?.body === undefined || init.body === null ? "" : await new Response(init.body).text() })
    const reply = replies.shift() ?? envelope({})
    return new Response(typeof reply.body === "string" ? reply.body : JSON.stringify(reply.body), { status: reply.status ?? 200, headers: { "content-type": "application/json" } })
  }
  return { client: { credentials, fetch: send as typeof fetch }, sent }
}
const get = (path: string): Sent => ({ method: "GET", url: `${API}${path}`, authorization: "Bearer workers-token", contentType: null, body: "" })

// The requests the hand-written fetches sent (method, URL, auth, body), one each.
test("each operation sends the request the hand-written fetch sent", async () => {
  const settings = recording(envelope({ compatibility_date: "2026-11-01", compatibility_flags: ["nodejs_compat"], bindings: [{ name: "AGENT_BUDGET", type: "durable_object_namespace", class_name: "AgentBudget" }] }))
  expect((await workerSettings(settings.client, worker)).ok).toBe(true)
  expect(settings.sent).toEqual([get(`/workers/scripts/${worker}/settings`)])

  const domains = recording(envelope([], { page: 1, per_page: 20, count: 0, total_count: 0 }))
  expect(await workerDomains(domains.client)).toEqual({ ok: true, value: [] })
  expect(domains.sent).toEqual([get("/workers/domains")])

  const deployments = recording(envelope({ deployments: [] }))
  expect(await workerDeployments(deployments.client, worker)).toEqual({ ok: true, value: [] })
  expect(deployments.sent).toEqual([get(`/workers/scripts/${worker}/deployments`)])

  const tail = recording(envelope({ id: "tail-1", url: "wss://tail.example/1", expires_at: "2026-11-22T00:00:00Z" }), envelope(null))
  expect(await openTail(tail.client, worker)).toEqual({ ok: true, value: { id: "tail-1", url: "wss://tail.example/1", expiresAt: "2026-11-22T00:00:00Z" } })
  await closeTail(tail.client, worker, "tail-1")
  expect(tail.sent).toEqual([
    { method: "POST", url: `${API}/workers/scripts/${worker}/tails`, authorization: "Bearer workers-token", contentType: "application/json", body: "{}" },
    { method: "DELETE", url: `${API}/workers/scripts/${worker}/tails/tail-1`, authorization: "Bearer workers-token", contentType: null, body: "" },
  ])

  const databases = recording(envelope([{ uuid: "db-1", name: "norn-studio-auth-preview" }], { page: 1, per_page: 20, count: 1, total_count: 1 }))
  expect((await d1Databases(databases.client, "norn-studio-auth-preview")).ok).toBe(true)
  expect(databases.sent).toEqual([get("/d1/database?name=norn-studio-auth-preview")])

  const script = recording(envelope({}))
  await requireWorkerScript("norn-cell-preview", credentials, script.client.fetch)
  expect(script.sent).toEqual([get("/workers/scripts/norn-cell-preview/settings")])
})

test("the typed answers reach the parity checks under Cloudflare's wire names", async () => {
  const live = recording(
    envelope({ compatibility_date: "2026-11-01", compatibility_flags: ["nodejs_compat"], bindings: [
      { name: "AGENT_BUDGET", type: "durable_object_namespace", class_name: "AgentBudget" },
      { name: "AI_GATEWAY_TOKEN", type: "secret_text" },
      { name: "AI_GATEWAY_URL", type: "secret_text" },
      { name: "ASSETS", type: "assets" },
    ] }),
    envelope(PROD_DOMAINS.map((hostname, i) => ({ id: `d${i}`, cert_id: "c", zone_id: "z", zone_name: "norn.cloud", hostname, service: worker, environment: "production", enabled: true })), { page: 1, per_page: 20, count: 2, total_count: 2 }),
  )
  expect(await recipeIssues("norn", live.client)).toEqual([])

  const drifted = recording(envelope({ compatibility_date: "2026-01-01", compatibility_flags: ["nodejs_compat"], bindings: [{ name: "AGENT_BUDGET", type: "durable_object_namespace", class_name: "Other" }] }), envelope([], { page: 1, per_page: 20, count: 0, total_count: 0 }))
  const issues = await recipeIssues("norn", drifted.client)
  expect(issues?.some((issue) => issue.includes("2026-01-01"))).toBe(true)
  expect(issues?.some((issue) => issue.includes("Other"))).toBe(true)

  const deployments = recording(envelope({ deployments: [{ id: "dep", created_on: "2026-11-20T00:00:00Z", source: "api", strategy: "percentage", versions: [{ version_id: "v1", percentage: 100 }] }] }))
  expect(await readDeployments("norn", deployments.client)).toEqual([{ id: "dep", created_on: "2026-11-20T00:00:00Z", versions: [{ version_id: "v1", percentage: 100 }] }])
})

// The messages and exit codes the scripts printed before, from distilled's typed errors.
test("typed errors map to the scripts' messages", async () => {
  expect(await recipeIssues("norn", recording(refusal(404, 10007, "workers.api.error.script_not_found")).client)).toBeUndefined()
  await expect(recipeIssues("norn", recording(refusal(403, 10000, "Authentication error")).client)).rejects.toThrow(/^norn: Cloudflare answered 403$/)
  await expect(recipeIssues("legacy", recording({ status: 522, body: "origin timed out" }).client)).rejects.toThrow(/^legacy: Cloudflare answered 522$/)
  await expect(recipeIssues("norn", recording(envelope({}), refusal(500, 10013, "internal")).client)).rejects.toThrow(/^norn: Cloudflare answered 500 for the custom domains$/)
  await expect(readDeployments("norn", recording(refusal(404, 10007, "not found")).client)).rejects.toThrow(/^norn: Cloudflare answered 404 for the deployments$/)
  await expect(tailDuring("norn", recording(refusal(409, 10057, "too many tails")).client, "https://x/", async () => {})).rejects.toThrow(/^norn: Cloudflare answered 409 opening a tail$/)

  await expect(requireWorkerScript("norn-cell-preview", credentials, recording(refusal(404, 10007, "not found")).client.fetch)).rejects.toThrow("The cell Worker norn-cell-preview does not exist on this account; deploy it before binding Studio to it as NORN_HOST")
  await expect(requireWorkerScript("norn-cell-preview", credentials, recording(refusal(403, 10000, "Authentication error")).client.fetch)).rejects.toThrow(/^Could not check the cell Worker norn-cell-preview: Cloudflare answered 403$/)

  await expect(discoverD1(recording(refusal(404, 7003, "Could not route to /accounts/x/d1/database")).client, "norn-studio-auth-preview")).rejects.toMatchObject({ exitCode: SEED_EXIT.refused, message: 'could not list D1 databases: [{"code":7003,"message":"Could not route to /accounts/x/d1/database"}]' })
  expect(await discoverD1(recording(envelope([{ uuid: "db-2", name: "norn-studio-auth-preview-old" }], { page: 1, per_page: 20, count: 1, total_count: 1 })).client, "norn-studio-auth-preview")).toBeUndefined()
  expect(await discoverD1(recording(envelope([{ uuid: "db-1", name: "norn-studio-auth-preview" }], { page: 1, per_page: 20, count: 1, total_count: 1 })).client, "norn-studio-auth-preview")).toEqual({ uuid: "db-1", name: "norn-studio-auth-preview" })
  const unreachable: CloudflareClient = { credentials, fetch: (async () => { throw new TypeError("Unable to connect") }) as unknown as typeof fetch }
  await expect(discoverD1(unreachable, "norn-studio-auth-preview")).rejects.toMatchObject({ exitCode: SEED_EXIT.refused, message: "D1 discovery failed" })
})

test("no call retries: a throttled or failing answer is one request", async () => {
  for (const reply of [refusal(429, 10000, "Rate limited"), refusal(503, 0, "unavailable"), refusal(403, 10000, "Authentication error")]) {
    const { client, sent } = recording(reply, envelope({}))
    expect(await workerSettings(client, worker)).toMatchObject({ ok: false, status: reply.status })
    expect(sent.length).toBe(1)
  }
})

test("a stalled call rejects with the TimeoutError AbortSignal.timeout raised; a dead connection with fetch's own error", async () => {
  const stalled: CloudflareClient = { credentials, fetch: ((_url: RequestInfo | URL, init?: RequestInit) => new Promise<Response>((_resolve, reject) => init?.signal?.addEventListener("abort", () => reject(init.signal?.reason)))) as typeof fetch }
  await expect(call(stalled, workers.getScriptScriptAndVersionSetting({ accountId: NORN_CLOUDFLARE_ACCOUNT_ID, scriptName: worker }), 30)).rejects.toMatchObject({ name: "TimeoutError" })
  const dead: CloudflareClient = { credentials, fetch: (async () => { throw new TypeError("Unable to connect") }) as unknown as typeof fetch }
  await expect(workerSettings(dead, worker)).rejects.toThrow("Unable to connect")
})

test("the tail is deleted after the probe, even when the probe fails or the delete is refused", async () => {
  const { client, sent } = recording(envelope({ id: "tail-9", url: "wss://tail.example/9", expires_at: "x" }), refusal(500, 0, "delete failed"))
  const socket = new EventTarget() as EventTarget & { send(data: string): void; close(): void }
  socket.send = () => {}
  socket.close = () => {}
  const opened = tailDuring("norn", client, "https://x/", async () => { throw new Error("probe failed") }, () => { queueMicrotask(() => socket.dispatchEvent(new Event("open"))); return socket as unknown as WebSocket })
  await expect(opened).rejects.toThrow("probe failed")
  expect(sent.map(({ method, url }) => `${method} ${url}`)).toEqual([`POST ${API}/workers/scripts/${worker}/tails`, `DELETE ${API}/workers/scripts/${worker}/tails/tail-9`])
})
