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

/**
 * The server's label for each Norn tool: the `title` its own tool manifest declares
 * (`x-norn-tool` on the route; an action's own title), which the bridge reads from the cell's
 * MCP `tools/list` and sends as `_norn/tools`. Studio adds no words of its own: a tool the
 * server gives no title is shown by its name.
 */
export type ToolLabels = ReadonlyMap<string, string>

/** The labels in a `_norn/tools` notice; anything malformed is left out. */
export function toolLabels(params: unknown): ToolLabels {
  const tools = (params as { readonly tools?: unknown } | null | undefined)?.tools
  const labels = new Map<string, string>()
  if (!Array.isArray(tools)) return labels
  for (const tool of tools as unknown[]) {
    const { name, title } = (tool ?? {}) as { readonly name?: unknown; readonly title?: unknown }
    if (typeof name === "string" && typeof title === "string" && title.trim() !== "") labels.set(name, title)
  }
  return labels
}

/** What a tool row says: the server's label, else the tool's own name. */
export const toolLabel = (labels: ToolLabels, tool: string): string => labels.get(tool) ?? tool

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
