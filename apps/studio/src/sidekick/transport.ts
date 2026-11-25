import type { RequestPermissionRequest, RequestPermissionResponse } from "@agentclientprotocol/sdk"
import type { ChatTransport } from "ai"
import type { ToolResult } from "@/widget/bridge"
import type { StudioScope } from "@/lib/definitions"
import type { SidekickEvents, SidekickSession } from "./client.ts"
import { promptText, Turn, type SidekickChunk, type SidekickMessage } from "./transcript.ts"

/**
 * Sidekick's chat transport for the AI SDK's `useChat` (DESIGN.md 19.3): each send is one ACP
 * prompt turn on the bridge's socket, streamed back as UI message chunks. The session opens with
 * the first send, pinned to the workspace scope the panel opened with, and lives until the panel
 * starts a new chat.
 *
 * Approvals: the bridge asks with `session/request_permission` while the turn is still running. The
 * transport shows it as the tool part's approval request and holds the bridge's question open until
 * the person answers (`respond`), stops the turn, or the turn ends. Only the two answers the bridge
 * offered ("allow once" and "reject") can go back; anything else is `cancelled`. The bridge and the
 * cell decide what runs; the page only relays the person's pick.
 */

interface Pending {
  readonly allow: string
  readonly reject: string
  readonly toolCallId: string
  readonly resolve: (response: RequestPermissionResponse) => void
}

const CANCELLED: RequestPermissionResponse = { outcome: { outcome: "cancelled" } }

type Open = (events: SidekickEvents, options: { readonly scope?: StudioScope | null }) => Promise<SidekickSession>

export class SidekickTransport implements ChatTransport<SidekickMessage> {
  private session: SidekickSession | null = null
  private opening: Promise<SidekickSession> | null = null
  private turn: Turn | null = null
  private readonly approvals = new Map<string, Pending>()
  private approvalCount = 0
  /** Bumped by `close`: a session that finishes opening for an older generation is closed, not used. */
  private generation = 0

  constructor(private readonly options: { readonly scope?: StudioScope | null; readonly open?: Open } = {}) {}

  private async connect(): Promise<SidekickSession> {
    if (this.session !== null && !this.session.closed) return this.session
    if (this.opening !== null) return this.opening
    const generation = this.generation
    const open: Open = this.options.open ?? (async (events, options) => (await import("./client.ts")).SidekickSession.open(events, options))
    this.opening = open({
      update: (notification) => this.turn?.update(notification),
      view: (delivery) => this.turn?.view(delivery),
      permission: (request) => this.ask(request),
      closed: () => { this.session = null; this.cancelApprovals() },
    }, this.options.scope === undefined ? {} : { scope: this.options.scope })
    try {
      const session = await this.opening
      if (generation !== this.generation) { session.close(); throw new Error("Chat closed.") }
      this.session = session
      return session
    } finally {
      this.opening = null
    }
  }

  /** The bridge asks before a tool call: hold its question open until the person answers. */
  private ask(request: RequestPermissionRequest): Promise<RequestPermissionResponse> {
    const allow = request.options.find((option) => option.kind === "allow_once")
    const reject = request.options.find((option) => option.kind === "reject_once")
    const turn = this.turn
    if (turn === null || allow === undefined || reject === undefined) return Promise.resolve(CANCELLED)
    return new Promise((resolve) => {
      const approvalId = `approval-${++this.approvalCount}`
      this.approvals.set(approvalId, { allow: allow.optionId, reject: reject.optionId, toolCallId: request.toolCall.toolCallId, resolve })
      turn.approval(approvalId, request.toolCall)
    })
  }

  /** The person's answer to one approval request; false when it is no longer open. */
  respond(approvalId: string, approved: boolean): boolean {
    const pending = this.approvals.get(approvalId)
    if (pending === undefined) return false
    this.approvals.delete(approvalId)
    pending.resolve({ outcome: { outcome: "selected", optionId: approved ? pending.allow : pending.reject } })
    if (!approved) this.turn?.denied(pending.toolCallId)
    return true
  }

  private cancelApprovals() {
    for (const pending of this.approvals.values()) pending.resolve(CANCELLED)
    this.approvals.clear()
  }

  async sendMessages({ messages, abortSignal }: Parameters<ChatTransport<SidekickMessage>["sendMessages"]>[0]): Promise<ReadableStream<SidekickChunk>> {
    const last = messages.findLast((message) => message.role === "user")
    const text = last === undefined ? "" : promptText(last).trim()
    let closed = false
    return new ReadableStream<SidekickChunk>({
      start: async (controller) => {
        const emit = (chunk: SidekickChunk) => { if (!closed) controller.enqueue(chunk) }
        const turn = new Turn(emit)
        this.turn = turn
        const stop = () => { this.session?.cancel(); this.cancelApprovals() }
        abortSignal?.addEventListener("abort", stop, { once: true })
        turn.start()
        try {
          if (text === "") throw new Error("Type a question first.")
          const session = await this.connect()
          if (abortSignal?.aborted) return
          await session.prompt(text)
          turn.finish()
        } catch (failure) {
          if (!abortSignal?.aborted) turn.error(failure instanceof Error && failure.message !== "" ? failure.message : "Sidekick could not answer.")
        } finally {
          abortSignal?.removeEventListener("abort", stop)
          if (this.turn === turn) this.turn = null
          this.cancelApprovals()
          if (!closed) { closed = true; controller.close() }
        }
      },
      cancel: () => { closed = true },
    })
  }

  reconnectToStream(): Promise<ReadableStream<SidekickChunk> | null> {
    return Promise.resolve(null)
  }

  /** A view's own follow-up read, through the same session. */
  callTool(name: string, args: Record<string, unknown>): Promise<ToolResult> {
    const session = this.session
    if (session === null) return Promise.reject(new Error("Start a chat first."))
    return session.callTool(name, args)
  }

  /** End the chat's session: it closes and nothing it sends later is drawn; a later send opens a new one. */
  close() {
    this.generation += 1
    this.cancelApprovals()
    this.turn = null
    this.session?.close()
    this.session = null
  }
}
