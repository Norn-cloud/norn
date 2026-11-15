import type { SessionNotification } from "@agentclientprotocol/sdk"
import type { ViewDelivery } from "./client.ts"

/**
 * One Sidekick chat as the panel draws it, folded from ACP session updates: the person's
 * prompts, the agent's words, each Norn tool it called (compact), and the view each tool's
 * result carries. Pure, so the panel only renders.
 */
export type Item =
  | { readonly kind: "user"; readonly id: string; readonly text: string }
  | { readonly kind: "text"; readonly id: string; readonly text: string }
  | { readonly kind: "tool"; readonly id: string; readonly tool: string; readonly status: "pending" | "in_progress" | "completed" | "failed" }
  | { readonly kind: "view"; readonly id: string; readonly delivery: ViewDelivery }

export type Transcript = readonly Item[]

const TOOL_PREFIX = /^mcp__norn__/

/** The Norn tool a tool call names, without the agent's server prefix. */
export function toolName(update: { readonly title?: string | null; readonly _meta?: Record<string, unknown> | null }): string {
  const meta = update._meta?.claudeCode as { toolName?: unknown } | undefined
  const name = typeof meta?.toolName === "string" ? meta.toolName : update.title ?? "tool"
  return name.replace(TOOL_PREFIX, "")
}

/** What each Norn tool did, in plain words; another tool reads as its name. */
const TOOL_WORDS: Readonly<Record<string, string>> = {
  norn_answer: "Answered the question",
  norn_trace_answer: "Traced where the number comes from",
  norn_trace_evaluated: "Traced the evaluated answer",
  norn_evaluate: "Evaluated the question",
  norn_inbox: "Read what needs attention",
  norn_get_findings: "Read the held record's findings",
  norn_list_held: "Listed held-back records",
  norn_query_objects: "Read records",
  norn_get_object: "Read a record",
  norn_object_links: "Read a record's links",
  norn_trace_object: "Traced a record to its sources",
  norn_catalog: "Read the catalog",
  norn_question_catalog: "Read the questions it can ask",
  norn_list_metrics: "Listed the metrics",
  norn_list_definitions: "Read the definitions",
  norn_plan_definitions: "Planned a definitions change",
  norn_apply_definitions: "Applied a definitions change",
  norn_propose_definition: "Proposed a definition",
  norn_profile_type: "Profiled a type",
  norn_call_action: "Called an action",
}
export const toolWords = (tool: string) => TOOL_WORDS[tool] ?? tool.replace(/^norn_/, "").replaceAll("_", " ")

export function promptItem(transcript: Transcript, id: string, text: string): Transcript {
  return [...transcript, { kind: "user", id, text }]
}

export function viewItem(transcript: Transcript, delivery: ViewDelivery): Transcript {
  return [...transcript, { kind: "view", id: `view:${transcript.length}`, delivery }]
}

/** Fold one session update into the transcript. Updates it does not draw leave it as it is. */
export function applyUpdate(transcript: Transcript, notification: SessionNotification): Transcript {
  const update = notification.update
  switch (update.sessionUpdate) {
    case "agent_message_chunk": {
      if (update.content.type !== "text") return transcript
      const last = transcript.at(-1)
      if (last?.kind === "text") return [...transcript.slice(0, -1), { ...last, text: last.text + update.content.text }]
      return [...transcript, { kind: "text", id: `text:${transcript.length}`, text: update.content.text }]
    }
    case "tool_call":
      if (transcript.some((item) => item.kind === "tool" && item.id === update.toolCallId)) return transcript
      return [...transcript, { kind: "tool", id: update.toolCallId, tool: toolName(update), status: update.status ?? "pending" }]
    case "tool_call_update": {
      const status = update.status
      if (status === undefined || status === null) return transcript
      return transcript.map((item) => item.kind === "tool" && item.id === update.toolCallId ? { ...item, status } : item)
    }
    default:
      return transcript
  }
}
