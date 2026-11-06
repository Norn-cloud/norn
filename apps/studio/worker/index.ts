import { gatewayModel, respond, type AgentRequest } from "./agent.ts"
import type { AgentBudget } from "./budget.ts"
import type { Scope } from "./limits.ts"

export { AgentBudget } from "./budget.ts"

export interface Env {
  readonly ASSETS: Fetcher
  readonly AGENT_BUDGET?: DurableObjectNamespace<AgentBudget>
  /** Deploy-time secrets; absent when the stage runs without the assistant. */
  readonly AI_GATEWAY_URL?: string
  readonly AI_GATEWAY_TOKEN?: string
  readonly AGENT_MODEL?: string
}

const MAX_BODY = 64 * 1024
const SESSION = /^[0-9a-f-]{36}$/

const refuse = (status: number, reason: string, headers: HeadersInit = {}) =>
  Response.json({ error: reason }, { status, headers: { "cache-control": "no-store", ...headers } })

const digest = async (text: string) =>
  [...new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)))].map((byte) => byte.toString(16).padStart(2, "0")).join("")

async function agent(request: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
  if (request.method !== "POST") return refuse(405, "Use POST.", { allow: "POST" })
  const { AGENT_BUDGET: budget, AI_GATEWAY_URL: url, AI_GATEWAY_TOKEN: token } = env
  if (budget === undefined || url === undefined || token === undefined) return refuse(503, "The assistant is not configured on this deployment.")
  const session = request.headers.get("x-norn-session") ?? ""
  if (!SESSION.test(session)) return refuse(400, "Missing session.")
  const length = Number(request.headers.get("content-length") ?? "0")
  if (length > MAX_BODY) return refuse(413, "The conversation is too long.")
  const text = await request.text()
  if (text.length > MAX_BODY) return refuse(413, "The conversation is too long.")
  let body: AgentRequest
  try {
    body = JSON.parse(text) as AgentRequest
    if (!Array.isArray(body.messages)) throw new Error("messages")
  } catch {
    return refuse(400, "Expected {messages: [...]}.")
  }
  const visitor = await digest(request.headers.get("cf-connecting-ip") ?? "unknown")
  const scopes: [Scope, string][] = [["session", `session:${session}`], ["visitor", `visitor:${visitor}`], ["global", "global"]]
  const ledgers = scopes.map(([scope, name]) => [scope, budget.get(budget.idFromName(name))] as const)
  for (const [scope, ledger] of ledgers) {
    const admission = await ledger.admit(scope)
    if (!admission.ok) return refuse(429, admission.reason, { "retry-after": String(admission.retryAfterSeconds) })
  }
  const model = gatewayModel(url, token, env.AGENT_MODEL ?? "claude-opus-5-5")
  return respond(model, body, (tokens) => ctx.waitUntil(Promise.all(ledgers.map(([, ledger]) => ledger.spend(tokens)))), request.signal)
}

/** One recorded example proposal, checked by the server's checker without the model: `{chip}` names it. */
async function example(request: Request): Promise<Response> {
  if (request.method !== "POST") return refuse(405, "Use POST.", { allow: "POST" })
  const body = await request.json().catch(() => null) as { chip?: unknown } | null
  if (typeof body?.chip !== "string") return refuse(400, "Expected {chip}.")
  // Loaded on first use, like the assistant: the checker's WebAssembly is not needed elsewhere.
  const [{ exampleVerdict }, { check }] = await Promise.all([import("./examples.ts"), import("./checker.ts")])
  const verdict = exampleVerdict(body.chip, check)
  return verdict === undefined ? refuse(404, "No such example.") : Response.json(verdict, { headers: { "cache-control": "no-store" } })
}

export default {
  async fetch(request, env, ctx) {
    const { pathname } = new URL(request.url)
    if (pathname === "/api/agent") return agent(request, env, ctx)
    if (pathname === "/api/agent/example") return example(request)
    if (pathname.startsWith("/api/")) return refuse(404, "No such endpoint.")
    return env.ASSETS.fetch(request)
  },
} satisfies ExportedHandler<Env>
