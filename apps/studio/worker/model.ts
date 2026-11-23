/**
 * The dashboard assistant's model, behind one small interface: given a
 * system prompt, a conversation and a JSON Schema, return one JSON document
 * that matches it, with what the call used and cost. Two providers sit
 * behind it, both reached with one run-only gateway token and no provider
 * key: Claude through the Cloudflare AI Gateway (unified billing), and
 * Workers AI as the fallback. Nothing a model returns is trusted: the
 * caller compiles it and the cell's checker decides.
 */

export interface Turn { readonly role: "user" | "assistant"; readonly content: string }
export interface Ask {
  readonly system: string
  readonly turns: readonly Turn[]
  /** The JSON Schema the answer must match (structured output). */
  readonly schema: Record<string, unknown>
  readonly maxTokens: number
}
export interface Answer {
  /** The parsed document, or null when the model gave none (refused, cut off, not JSON). */
  readonly json: unknown
  readonly model: string
  readonly inputTokens: number
  readonly outputTokens: number
  /** What the call cost, in millionths of a dollar, from the provider's published per-token prices. */
  readonly costMicroUsd: number
  /** Why there is no document, when there is none. */
  readonly failure?: string
}
export interface Model {
  readonly name: string
  answer(ask: Ask): Promise<Answer>
}

/** Published prices, in dollars per million tokens (input, output). An unlisted model is priced as the most expensive listed one. */
const CLAUDE_PRICES: Record<string, readonly [number, number]> = {
  "claude-sonnet-5-5": [2, 10],
  "claude-haiku-5-5": [0.1, 0.5],
  "claude-opus-5-5": [4, 20],
}
const priceOf = (model: string) => CLAUDE_PRICES[model] ?? [4, 20]
export const claudeCost = (model: string, input: number, output: number) => {
  const [inPrice, outPrice] = priceOf(model)
  return Math.ceil(input * inPrice + output * outPrice)
}
/** Workers AI bills neurons at $0.011 per thousand. */
export const neuronCost = (neurons: number) => Math.ceil(neurons * 11)

type Fetch = (input: string | URL | Request, init?: RequestInit) => Promise<Response>

/**
 * Claude through the AI Gateway's Anthropic route. The official SDK makes the
 * call; the gateway authenticates with its own token and bills the account,
 * so the request carries no provider key (the SDK's placeholder key is
 * removed before it leaves the Worker).
 */
export function claudeViaGateway(options: { readonly gatewayUrl: string; readonly token: string; readonly model: string; readonly fetch?: Fetch }): Model {
  const send: Fetch = options.fetch ?? ((input, init) => fetch(input, init))
  return {
    name: options.model,
    async answer(ask) {
      // Loaded on first use, never with the Worker: sign-in must not pay for the SDK.
      const { default: Anthropic } = await import("@anthropic-ai/sdk")
      const client = new Anthropic({
        apiKey: "gateway",
        baseURL: `${options.gatewayUrl.replace(/\/$/, "")}/anthropic`,
        maxRetries: 1,
        fetch: (input: string | URL | Request, init?: RequestInit) => {
          const headers = new Headers(init?.headers)
          headers.delete("x-api-key")
          headers.set("cf-aig-authorization", `Bearer ${options.token}`)
          return send(input, { ...init, headers })
        },
      })
      const message = await client.messages.create({
        model: options.model,
        max_tokens: ask.maxTokens,
        system: ask.system,
        messages: ask.turns.map(({ role, content }) => ({ role, content })),
        output_config: { effort: "medium", format: { type: "json_schema", schema: ask.schema } },
      })
      const usage = { inputTokens: message.usage.input_tokens, outputTokens: message.usage.output_tokens }
      const spent = { model: options.model, ...usage, costMicroUsd: claudeCost(options.model, usage.inputTokens, usage.outputTokens) }
      if (message.stop_reason === "refusal") return { ...spent, json: null, failure: "The model declined this request." }
      if (message.stop_reason === "max_tokens") return { ...spent, json: null, failure: "The model's answer was cut off." }
      const text = message.content.flatMap((block) => block.type === "text" ? [block.text] : []).join("")
      try { return { ...spent, json: JSON.parse(text) } } catch { return { ...spent, json: null, failure: "The model's answer was not JSON." } }
    },
  }
}

/** Workers AI over its REST API, with JSON mode against the same schema. */
export function workersAi(options: { readonly accountId: string; readonly token: string; readonly model: string; readonly fetch?: Fetch }): Model {
  const send: Fetch = options.fetch ?? ((input, init) => fetch(input, init))
  return {
    name: options.model,
    async answer(ask) {
      const response = await send(`https://api.cloudflare.com/client/v4/accounts/${options.accountId}/ai/run/${options.model}`, {
        method: "POST",
        headers: { authorization: `Bearer ${options.token}`, "content-type": "application/json" },
        body: JSON.stringify({
          messages: [{ role: "system", content: ask.system }, ...ask.turns],
          response_format: { type: "json_schema", json_schema: ask.schema },
          max_tokens: ask.maxTokens,
        }),
      })
      const body = await response.json().catch(() => null) as { success?: boolean; result?: { response?: unknown; usage?: { prompt_tokens?: number; completion_tokens?: number; neurons?: number } } } | null
      const usage = body?.result?.usage
      const spent = { model: options.model, inputTokens: usage?.prompt_tokens ?? 0, outputTokens: usage?.completion_tokens ?? 0, costMicroUsd: neuronCost(usage?.neurons ?? 0) }
      if (!response.ok || body?.success !== true) return { ...spent, json: null, failure: `Workers AI did not answer (${response.status}).` }
      const answer = body.result?.response
      if (typeof answer !== "string") return { ...spent, json: answer ?? null, ...answer == null ? { failure: "Workers AI gave no answer." } : {} }
      try { return { ...spent, json: JSON.parse(answer) } } catch { return { ...spent, json: null, failure: "Workers AI's answer was not JSON." } }
    },
  }
}

/** The primary model, and the fallback when the primary throws or gives no document; both calls are paid for and reported. */
export function withFallback(primary: Model, fallback: Model): Model & { answerAll(ask: Ask): Promise<readonly Answer[]> } {
  const answerAll = async (ask: Ask): Promise<readonly Answer[]> => {
    let first: Answer
    try { first = await primary.answer(ask) } catch (error) {
      first = { json: null, model: primary.name, inputTokens: 0, outputTokens: 0, costMicroUsd: 0, failure: error instanceof Error ? error.message : String(error) }
    }
    return first.json !== null ? [first] : [first, await fallback.answer(ask)]
  }
  return { name: primary.name, answerAll, answer: async (ask) => (await answerAll(ask)).at(-1) as Answer }
}

/** The configured model, or why there is none: the gateway URL names the account; one token reaches both providers. */
export function configuredModel(env: { readonly AI_GATEWAY_URL?: string; readonly AI_GATEWAY_TOKEN?: string; readonly AGENT_MODEL?: string; readonly AGENT_FALLBACK_MODEL?: string }) {
  const { AI_GATEWAY_URL: gatewayUrl, AI_GATEWAY_TOKEN: token } = env
  if (gatewayUrl === undefined || token === undefined || gatewayUrl === "" || token === "") return undefined
  const accountId = /\/v1\/([0-9a-f]{32})\//.exec(gatewayUrl)?.[1]
  if (accountId === undefined) return undefined
  return withFallback(
    claudeViaGateway({ gatewayUrl, token, model: env.AGENT_MODEL || "claude-sonnet-5-5" }),
    workersAi({ accountId, token, model: env.AGENT_FALLBACK_MODEL || "@cf/meta/llama-3.3-70b-instruct-fp8-fast" }),
  )
}
