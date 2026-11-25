import type { SessionNotification, ToolCallUpdate } from "@agentclientprotocol/sdk"
import type { UIMessage, UIMessageChunk } from "ai"
import type { ViewDelivery } from "./client.ts"

/**
 * One Sidekick turn as the chat draws it. The bridge speaks ACP; the panel's chat state is the AI
 * SDK's `useChat` (DESIGN.md 19.3), so each ACP session update becomes the UI message chunk that
 * draws it: the agent's words as text, each Norn tool call as a tool part (with the person's
 * approval when the bridge asks for it), and the view a tool's result carries as a `view` data
 * part. Pure, so the transport only routes and the panel only renders.
 */

export type SidekickData = { readonly view: ViewDelivery }
export type SidekickMessage = UIMessage<unknown, SidekickData>
export type SidekickChunk = UIMessageChunk<unknown, SidekickData>

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

/** The text an ACP tool call's content carries, if any. */
function contentText(update: Pick<ToolCallUpdate, "content">): string {
  return (update.content ?? []).flatMap((block) => block.type === "content" && block.content.type === "text" ? [block.content.text] : []).join("\n").trim()
}

/**
 * Folds one prompt turn into chunks, in order. Every tool chunk names a tool call this turn opened,
 * so the chat never meets a tool it does not know.
 */
export class Turn {
  private text: string | null = null
  private texts = 0
  private views = 0
  /** Each tool call this turn opened: settled once it has an outcome, its input as last drawn, and whether the person was asked. */
  private readonly tools = new Map<string, { name: string; settled: boolean; input: string; asked: boolean }>()
  private finished = false

  constructor(private readonly emit: (chunk: SidekickChunk) => void) {}

  start() {
    this.emit({ type: "start" })
  }

  private closeText() {
    if (this.text === null) return
    this.emit({ type: "text-end", id: this.text })
    this.text = null
  }

  /**
   * Open a tool part for this call, or draw its input again when the call now carries a different
   * one. The agent announces a call before its input has finished streaming, then sends the whole
   * input in a later update and in the approval question; the part always shows the input the
   * person is asked about. Once the person was asked, only the question redraws it: the part keeps
   * their question and the input that came with it.
   */
  private openTool(call: Pick<ToolCallUpdate, "toolCallId" | "title" | "rawInput" | "_meta">, question = false) {
    const known = this.tools.get(call.toolCallId)
    const input = call.rawInput ?? {}
    const drawn = JSON.stringify(input)
    if (known !== undefined && (known.settled || (known.asked && !question) || call.rawInput === undefined || known.input === drawn)) return
    if (known === undefined) this.closeText()
    // The name is the one the call opened with: a later update may not repeat it.
    const name = known?.name ?? toolName(call)
    this.tools.set(call.toolCallId, { name, settled: false, input: drawn, asked: known?.asked ?? false })
    this.emit({ type: "tool-input-available", toolCallId: call.toolCallId, toolName: name, title: toolWords(name), input, dynamic: true })
  }

  private settle(toolCallId: string, chunk: SidekickChunk) {
    const tool = this.tools.get(toolCallId)
    if (tool === undefined || tool.settled) return
    tool.settled = true
    this.emit(chunk)
  }

  update(notification: SessionNotification) {
    if (this.finished) return
    const update = notification.update
    switch (update.sessionUpdate) {
      case "agent_message_chunk": {
        if (update.content.type !== "text" || update.content.text === "") return
        if (this.text === null) {
          this.text = `text-${this.texts++}`
          this.emit({ type: "text-start", id: this.text })
        }
        this.emit({ type: "text-delta", id: this.text, delta: update.content.text })
        return
      }
      case "tool_call":
        this.openTool(update)
        if (update.status === "completed") this.settle(update.toolCallId, { type: "tool-output-available", toolCallId: update.toolCallId, output: contentText(update), dynamic: true })
        if (update.status === "failed") this.settle(update.toolCallId, { type: "tool-output-error", toolCallId: update.toolCallId, errorText: contentText(update) || "The tool call failed.", dynamic: true })
        return
      case "tool_call_update": {
        if (!this.tools.has(update.toolCallId)) return
        this.openTool(update)
        if (update.status === "completed") this.settle(update.toolCallId, { type: "tool-output-available", toolCallId: update.toolCallId, output: contentText(update), dynamic: true })
        if (update.status === "failed") this.settle(update.toolCallId, { type: "tool-output-error", toolCallId: update.toolCallId, errorText: contentText(update) || "The tool call failed.", dynamic: true })
        return
      }
      default:
        return
    }
  }

  /** A tool result's MCP Apps view, drawn where it arrived in the turn. */
  view(delivery: ViewDelivery) {
    if (this.finished) return
    this.closeText()
    this.emit({ type: "data-view", id: `view-${this.views++}`, data: delivery })
  }

  /** The bridge asks the person before this call runs. */
  approval(approvalId: string, call: Pick<ToolCallUpdate, "toolCallId" | "title" | "rawInput" | "_meta">) {
    if (this.finished) return
    // The question's own input is drawn first, so the person sees what they are allowing.
    this.openTool(call, true)
    const tool = this.tools.get(call.toolCallId)
    if (tool !== undefined) tool.asked = true
    this.emit({ type: "tool-approval-request", approvalId, toolCallId: call.toolCallId })
  }

  /** The person's answer, recorded in the turn itself so the tool part shows it whatever arrives next. */
  answered(approvalId: string, approved: boolean) {
    if (this.finished) return
    this.emit({ type: "tool-approval-response", approvalId, approved })
  }

  /** The person declined: the call never ran. */
  denied(toolCallId: string) {
    if (this.finished) return
    this.settle(toolCallId, { type: "tool-output-denied", toolCallId })
  }

  /** The turn ended: its stop reason, in the message's metadata-free finish. */
  finish() {
    if (this.finished) return
    this.closeText()
    this.finished = true
    this.emit({ type: "finish" })
  }

  /** The turn failed: Sidekick says so in words, and the chat offers Retry. */
  error(errorText: string) {
    if (this.finished) return
    this.closeText()
    this.finished = true
    this.emit({ type: "error", errorText })
  }
}

/** The person's words in a user message (Sidekick sends text only). */
export const promptText = (message: SidekickMessage): string =>
  message.parts.flatMap((part) => part.type === "text" ? [part.text] : []).join("")
