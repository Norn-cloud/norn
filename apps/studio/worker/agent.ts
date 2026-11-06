import { createAnthropic } from "@ai-sdk/anthropic"
import { convertToModelMessages, stepCountIs, streamText, tool, type UIMessage } from "ai"
import { z } from "zod"
import { admitQuestion, check } from "./checker.ts"
import { ANALYTICS, questionVerdict } from "./questions.ts"
import { ProposalError, applyProposal, verdictOf, worldIndex } from "./proposal.ts"
import { CONTEXT, WORLD } from "./world.ts"

/** The model, reached through the AI gateway with the gateway's own credential; no provider key exists in the Worker. */
export function gatewayModel(gatewayUrl: string, gatewayToken: string, model: string) {
  const anthropic = createAnthropic({
    baseURL: `${gatewayUrl.replace(/\/compat\/?$/, "")}/anthropic/v1`,
    apiKey: "unused",
    // Typed as the runtime's fetch: with bun's types also loaded (the Worker tests), fetch carries extras this wrapper does not need.
    fetch: ((input: Parameters<typeof fetch>[0], init?: RequestInit) => {
      const headers = new Headers(init?.headers)
      headers.delete("x-api-key")
      headers.set("cf-aig-authorization", `Bearer ${gatewayToken}`)
      return fetch(input, { ...init, headers })
    }) as typeof fetch,
  })
  return anthropic(model)
}

export const SYSTEM = `You help an economist change Norn's business model of a small jewelry refinery: melts of gold scrap, measured, approved and posted, and the law that fine gold mass is conserved through each melt.
The world's definitions are artifacts: object types (entities), laws and actions. Read them with the world and artifact tools.
You cannot change anything yourself. To suggest a change, call propose with the artifact, a JSON pointer inside it, and the new value.
The checker on the server decides every proposal. Never say a change is accepted unless propose returned admitted: true.
When the economist asks for a concrete change, propose it as asked in one call, even if you expect a refusal; the checker decides, and its refusal names the law. Ask a question only when the request names no change at all.
When the checker refuses, say which law or rule refused it in plain words, quote the law's own statement, and suggest what would satisfy it.
Nothing you propose is installed: a proposal is checked, not applied.
To define a business question (a measure over time, such as weekly yield), call propose_question with one question definition shaped like its example; the checker admits it or names the problem. An admitted question is not yet run: never state a number for it.
Business questions and their numbers stay with the recorded answers in Studio; do not invent figures.
Keep answers short and plain. Use the economist's words, not code, except when quoting a law's expression.`

const json = z.json()

export const tools = {
  world: tool({
    description: "List the world's artifacts: entities, laws (with their statement) and actions.",
    inputSchema: z.object({}),
    execute: async () => worldIndex(WORLD),
  }),
  artifact: tool({
    description: "Read one artifact's full definition.",
    inputSchema: z.object({ artifactId: z.string().max(200) }),
    execute: async ({ artifactId }) => WORLD.find((envelope) => envelope.artifactId === artifactId) ?? { error: `No artifact ${artifactId}.` },
  }),
  propose_question: tool({
    description: `Ask the checker whether a business question is well defined over the analytics world: object type ${ANALYTICS.artifacts[0]?.name} (properties input, recovered, loss in grams; occurred_at; metal) with metrics ${ANALYTICS.metrics.map(({ name, label }) => `${name} (${label})`).join(", ")}. Give one question definition shaped like this admitted example: ${JSON.stringify(ANALYTICS.example)}. Change only its name, aggregate, groupBy, filter and window. It is checked, not run.`,
    inputSchema: z.object({ question: json.describe("One question definition (QuestionIR).") }),
    execute: async ({ question }) => questionVerdict(question, admitQuestion),
  }),
  propose: tool({
    description: "Ask the checker whether one change would be admitted: set the value at a JSON pointer inside one artifact.",
    inputSchema: z.object({
      artifact: z.string().max(200).describe("The artifactId to change."),
      pointer: z.string().max(400).describe("RFC 6901 pointer inside the artifact envelope, e.g. /payload/response/expression."),
      value: json.describe("The new JSON value at that pointer."),
    }),
    execute: async (proposal) => {
      try {
        return verdictOf(WORLD, proposal, check(applyProposal(WORLD, proposal), CONTEXT))
      } catch (error) {
        if (error instanceof ProposalError) return { proposal, error: error.message }
        throw error
      }
    },
  }),
}

export interface AgentRequest {
  readonly messages: readonly UIMessage[]
}

/** One assistant turn, bounded in steps and output, reporting the tokens it used. */
export async function respond(model: ReturnType<typeof gatewayModel>, request: AgentRequest, onUsage: (tokens: number) => void, signal: AbortSignal) {
  const result = streamText({
    model,
    system: SYSTEM,
    messages: await convertToModelMessages(request.messages.slice(-20)),
    tools,
    stopWhen: stepCountIs(6),
    maxOutputTokens: 2_000,
    abortSignal: signal,
    onFinish: ({ totalUsage }) => onUsage((totalUsage.inputTokens ?? 0) + (totalUsage.outputTokens ?? 0)),
  })
  return result.toUIMessageStreamResponse()
}
