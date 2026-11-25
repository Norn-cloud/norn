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
 *
 * Stop: the turn is detached at once (nothing the bridge sends afterwards is drawn into it, and a
 * question it asks afterwards is answered `cancelled`), and only that turn's questions are cancelled.
 * The next prompt goes out only once the stopped one has settled at the bridge, so a fast follow-up
 * never receives the stopped turn's traffic; if it does not settle in time the session is reopened.
 *
 * Sessions: only the current session's traffic counts. A session that was replaced or closed is
 * retired first; whatever it sends afterwards (an update, a view, its own `closed`) is dropped, and a
 * question it asks is answered `cancelled`, so a late message never lands in a later turn.
 */

interface Pending {
  readonly turn: Turn
  readonly allow: string
  readonly reject: string
  readonly toolCallId: string
  readonly resolve: (response: RequestPermissionResponse) => void
}

const CANCELLED: RequestPermissionResponse = { outcome: { outcome: "cancelled" } }

/** How long a new prompt waits for a stopped one to settle before the session is reopened instead. */
export const SETTLE_MS = 10_000

type Open = (events: SidekickEvents, options: { readonly scope?: StudioScope | null }) => Promise<SidekickSession>

export class SidekickTransport implements ChatTransport<SidekickMessage> {
  private session: SidekickSession | null = null
  /** The current session's link: retired (live false) before the session is replaced or closed. */
  private link: { live: boolean } | null = null
  private opening: Promise<SidekickSession> | null = null
  private turn: Turn | null = null
  private readonly approvals = new Map<string, Pending>()
  private approvalCount = 0
  /** Settles when the last prompt the bridge was sent has answered (or failed). */
  private inflight: Promise<void> = Promise.resolve()
  /** Bumped by `close`: a session that finishes opening for an older generation is closed, not used. */
  private generation = 0

  constructor(private readonly options: { readonly scope?: StudioScope | null; readonly open?: Open; readonly settleMs?: number } = {}) {}

  private async connect(): Promise<SidekickSession> {
    if (this.session !== null && !this.session.closed) return this.session
    if (this.opening !== null) return this.opening
    const generation = this.generation
    const open: Open = this.options.open ?? (async (events, options) => (await import("./client.ts")).SidekickSession.open(events, options))
    // This session's own link: its events count only while it is the current session.
    const link = { live: true }
    this.opening = open({
      update: (notification) => { if (link.live) this.turn?.update(notification) },
      view: (delivery) => { if (link.live) this.turn?.view(delivery) },
      permission: (request) => link.live ? this.ask(request) : Promise.resolve(CANCELLED),
      closed: () => { if (!link.live) return; this.retire(); this.session = null; this.cancelApprovals(null) },
    }, this.options.scope === undefined ? {} : { scope: this.options.scope })
    try {
      const session = await this.opening
      if (generation !== this.generation || !link.live) { link.live = false; session.close(); throw new Error("Chat closed.") }
      this.link = link
      this.session = session
      return session
    } finally {
      this.opening = null
    }
  }

  /** Retire the current session's link: nothing it sends from now on reaches a turn. */
  private retire() {
    if (this.link !== null) this.link.live = false
    this.link = null
  }

  /** The bridge asks before a tool call: hold its question open until the person answers. */
  private ask(request: RequestPermissionRequest): Promise<RequestPermissionResponse> {
    const allow = request.options.find((option) => option.kind === "allow_once")
    const reject = request.options.find((option) => option.kind === "reject_once")
    const turn = this.turn
    if (turn === null || allow === undefined || reject === undefined) return Promise.resolve(CANCELLED)
    return new Promise((resolve) => {
      const approvalId = `approval-${++this.approvalCount}`
      this.approvals.set(approvalId, { turn, allow: allow.optionId, reject: reject.optionId, toolCallId: request.toolCall.toolCallId, resolve })
      turn.approval(approvalId, request.toolCall)
    })
  }

  /** The person's answer to one approval request; false when it is no longer open. */
  respond(approvalId: string, approved: boolean): boolean {
    const pending = this.approvals.get(approvalId)
    if (pending === undefined) return false
    this.approvals.delete(approvalId)
    pending.turn.answered(approvalId, approved)
    pending.resolve({ outcome: { outcome: "selected", optionId: approved ? pending.allow : pending.reject } })
    if (!approved) pending.turn.denied(pending.toolCallId)
    return true
  }

  /** Cancel the open questions of one turn, or of every turn (`null`). */
  private cancelApprovals(turn: Turn | null) {
    for (const [approvalId, pending] of this.approvals) {
      if (turn !== null && pending.turn !== turn) continue
      this.approvals.delete(approvalId)
      pending.resolve(CANCELLED)
    }
  }

  /** Wait for the previous prompt to settle: "late" when it did not in time; stopping the turn ends the wait at once ("stopped"). */
  private async settled(previous: Promise<void>, abortSignal: AbortSignal | undefined): Promise<"settled" | "late" | "stopped"> {
    let timer: ReturnType<typeof setTimeout> | undefined
    let stopped: (() => void) | undefined
    const late = new Promise<"late">((resolve) => { timer = setTimeout(() => resolve("late"), this.options.settleMs ?? SETTLE_MS) })
    const aborted = new Promise<"stopped">((resolve) => {
      stopped = () => resolve("stopped")
      if (abortSignal?.aborted) stopped()
      else abortSignal?.addEventListener("abort", stopped, { once: true })
    })
    try {
      return await Promise.race([previous.then(() => "settled" as const), late, aborted])
    } finally {
      clearTimeout(timer)
      if (stopped !== undefined) abortSignal?.removeEventListener("abort", stopped)
    }
  }

  async sendMessages({ messages, abortSignal }: Parameters<ChatTransport<SidekickMessage>["sendMessages"]>[0]): Promise<ReadableStream<SidekickChunk>> {
    const last = messages.findLast((message) => message.role === "user")
    const text = last === undefined ? "" : promptText(last).trim()
    let closed = false
    // This turn's place in line: the next prompt waits for it, the way this one waits for the last.
    // The place is handed on only once the previous prompt has settled or its session was retired:
    // a turn that ends before then (stopped, empty, or failing to connect) keeps the line on `previous`.
    const previous = this.inflight
    let released = false
    let done: () => void = () => undefined
    this.inflight = new Promise<void>((resolve) => { done = resolve })
    return new ReadableStream<SidekickChunk>({
      start: async (controller) => {
        const emit = (chunk: SidekickChunk) => { if (!closed) controller.enqueue(chunk) }
        const turn = new Turn(emit)
        let prompted: SidekickSession | null = null
        const stop = () => {
          // Detach first: whatever the bridge still sends for this turn is not drawn, and a question it
          // asks now is answered `cancelled`. Only this turn's questions are cancelled.
          if (this.turn === turn) this.turn = null
          prompted?.cancel()
          this.cancelApprovals(turn)
        }
        abortSignal?.addEventListener("abort", stop, { once: true })
        turn.start()
        try {
          if (text === "") throw new Error("Type a question first.")
          let session = await this.connect()
          const waited = await this.settled(previous, abortSignal)
          if (waited === "stopped") return
          if (waited === "late" && this.session === session) {
            // The stopped turn never settled: start over on a fresh session rather than share its traffic.
            // The old session is retired first, so nothing it sends later reaches the new turn.
            this.retire(); this.session = null; session.close()
          }
          released = true
          // Prompt only the chat's current, open session: one closed or replaced while this turn waited is not it.
          if (this.session !== session || session.closed) session = await this.connect()
          if (abortSignal?.aborted) return
          this.turn = turn
          prompted = session
          await session.prompt(text)
          turn.finish()
        } catch (failure) {
          if (!abortSignal?.aborted) turn.error(failure instanceof Error && failure.message !== "" ? failure.message : "Sidekick could not answer.")
        } finally {
          abortSignal?.removeEventListener("abort", stop)
          if (this.turn === turn) this.turn = null
          this.cancelApprovals(turn)
          if (released) done()
          else void previous.then(done, done)
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
    this.retire()
    this.cancelApprovals(null)
    this.turn = null
    const session = this.session
    this.session = null
    session?.close()
  }
}
