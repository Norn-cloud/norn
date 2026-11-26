import { useChat } from "@ai-sdk/react"
import type { DynamicToolUIPart } from "ai"
import { ArrowDown, RotateCcw, SquarePen, Wrench } from "lucide-react"
import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState } from "react"
import { Streamdown } from "streamdown"
import { StickToBottom, useStickToBottomContext } from "use-stick-to-bottom"
import { useAssistant } from "@/assistant-slot"
import { ChatComposer } from "@/components/chat-composer"
import { Mascot, type MascotState } from "@/components/system/mascot"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Bubble, BubbleContent } from "@/components/ui/bubble"
import { Button } from "@/components/ui/button"
import { Message, MessageAvatar, MessageContent } from "@/components/ui/message"
import { Trace, TraceCollapsible, TraceCollapsibleContent, TraceCollapsibleTrigger, TraceIcon, TraceLine, TraceTitle } from "@/components/ui/trace"
import { getDisplayedScope, type StudioScope } from "@/lib/definitions"
import type { ToolHost } from "@/widget/bridge"
import type { View } from "@/widget/views"
import type { ViewDelivery } from "./client.ts"
import type { SidekickMessage } from "./transcript.ts"
import { SidekickTransport } from "./transport.ts"

export { useSidekickServed } from "./served.ts"

/**
 * Sidekick: the person's agent inside Studio. It answers by calling Norn's own tools as the
 * signed-in person (the cell's policy decides what it sees and may do), and each tool's result
 * is drawn here with the same views a chat assistant shows (src/widget/views.tsx).
 *
 * The chat is drawn with shadcn's own chat primitives (Message, Bubble and Trace from the base-luma
 * registry, DESIGN.md 19.3) over the AI SDK's `useChat`, whose transport (`transport.ts`) speaks ACP
 * to the bridge. Before a tool call that can change anything, the bridge asks the person; the
 * question is drawn under the call, and the answer goes back to the bridge, which runs the call only
 * on "Allow". The cell still decides the call.
 */

// The views load with the first one drawn, never with the shell.
const DeliveryView = lazy(() => import("@/widget/views").then((module) => ({ default: module.DeliveryView })))

const VIEWS: readonly View[] = ["answer", "lineage", "dashboard", "receipt", "records", "findings", "plan", "inbox"]
const viewOf = (name: string): View | null => VIEWS.find((view) => view === name) ?? null

export const SUGGESTIONS = ["What changed this week?", "Where does that number come from?", "What needs my attention?"] as const

/** Each tool part's state: its ACP-like `data-status`, its word, and its dot (DESIGN.md 4.3). */
const STATE: Readonly<Record<DynamicToolUIPart["state"], { readonly status: string; readonly word: string; readonly dot: "outline" | "info" | "warning" | "success" | "destructive" }>> = {
  "input-streaming": { status: "pending", word: "Pending", dot: "outline" },
  "input-available": { status: "in_progress", word: "Running", dot: "info" },
  "approval-requested": { status: "awaiting_approval", word: "Waiting for you", dot: "warning" },
  "approval-responded": { status: "in_progress", word: "Running", dot: "info" },
  "output-available": { status: "completed", word: "Done", dot: "success" },
  "output-error": { status: "failed", word: "Failed", dot: "destructive" },
  "output-denied": { status: "declined", word: "Declined", dot: "destructive" },
}

/** A call the turn left unfinished (the person stopped it, or it ended first): it shows as stopped, never as still running or still asking. */
const STOPPED = { status: "stopped", word: "Stopped", dot: "outline" } as const
const UNFINISHED: ReadonlySet<DynamicToolUIPart["state"]> = new Set(["input-streaming", "input-available", "approval-requested", "approval-responded"])

function ViewCard({ delivery, host }: { readonly delivery: ViewDelivery; readonly host: ToolHost }) {
  const view = viewOf(delivery.view)
  if (view === null) return null
  return (
    <div className="enter overflow-hidden rounded-(--radius-well) border border-(--panel-border) bg-card" data-view={view}>
      <Suspense fallback={<p className="p-3 text-body text-muted-foreground">Drawing Norn's answer…</p>}>
        <DeliveryView view={view} delivery={{ input: delivery.input, result: delivery.result }} bridge={host} studio={window.location.origin} tool={delivery.tool} />
      </Suspense>
    </div>
  )
}

/** What the call will do, in the agent's own input: the action and its fields, never re-worded. */
function CallInput({ input }: { readonly input: unknown }) {
  if (input === null || typeof input !== "object" || Object.keys(input).length === 0) return null
  return <pre dir="ltr" className="max-h-40 overflow-auto rounded-(--radius) bg-well px-3 py-2 font-mono text-meta text-foreground">{JSON.stringify(input, null, 2)}</pre>
}

/** The bridge's question, under the call it is about: Allow or Decline, and afterwards what was answered. */
function Approval({ part, live, onAnswer }: { readonly part: DynamicToolUIPart; readonly live: boolean; readonly onAnswer: (approvalId: string, approved: boolean) => void }) {
  const approval = part.approval
  if (approval === undefined) return null
  // The turn ended before the person answered: the question is closed, and the call never ran.
  if (part.state === "approval-requested" && !live) return <p className="text-meta text-muted-foreground">Stopped before you answered; nothing ran.</p>
  if (part.state === "approval-requested") {
    return (
      <div role="group" aria-label="Approve this call" className="space-y-3 rounded-(--radius-well) border border-(--panel-border) bg-well p-3" data-slot="approval">
        <p className="text-body text-foreground">Sidekick wants to run <span className="font-mono text-meta">{part.toolName}</span> as you. The workspace's laws and policy still decide it.</p>
        <div className="flex justify-end gap-2">
          <Button size="sm" variant="outline" onClick={() => onAnswer(approval.id, false)}>Decline</Button>
          <Button size="sm" onClick={() => onAnswer(approval.id, true)}>Allow</Button>
        </div>
      </div>
    )
  }
  if (approval.approved === undefined) return null
  return <p className="text-meta text-muted-foreground">{approval.approved ? "You allowed this call." : "You declined; nothing ran."}</p>
}

function ToolCall({ part, live, onAnswer }: { readonly part: DynamicToolUIPart; readonly live: boolean; readonly onAnswer: (approvalId: string, approved: boolean) => void }) {
  const stopped = !live && UNFINISHED.has(part.state)
  const asking = part.state === "approval-requested" && !stopped
  const state = stopped ? STOPPED : STATE[part.state]
  // A call waiting on the person opens by itself; otherwise the reader opens it.
  const [open, setOpen] = useState(asking)
  useEffect(() => { if (asking) setOpen(true) }, [asking])
  return (
    <Trace data-tool={part.toolName} data-status={state.status}>
      <TraceCollapsible open={open} onOpenChange={setOpen}>
        <TraceCollapsibleTrigger className="gap-2">
          <TraceLine status={part.state === "output-error" ? "error" : !stopped && (part.state === "input-available" || part.state === "approval-responded") ? "running" : undefined}>
            <TraceIcon><Wrench /></TraceIcon>
            <TraceTitle>{part.title ?? part.toolName}</TraceTitle>
          </TraceLine>
          <Badge variant={state.dot} data-tool-state={part.state}>{state.word}</Badge>
        </TraceCollapsibleTrigger>
        <TraceCollapsibleContent className="space-y-3">
          <CallInput input={part.input} />
          {part.state === "output-error" && <p className="text-body text-destructive-text">{part.errorText}</p>}
          <Approval part={part} live={live} onAnswer={onAnswer} />
        </TraceCollapsibleContent>
      </TraceCollapsible>
    </Trace>
  )
}

/** The mascot's pose for an assistant message: what Sidekick is doing in it right now. */
function poseOf(message: SidekickMessage, live: boolean, failed: boolean): MascotState {
  if (live && message.parts.some((part) => part.type === "dynamic-tool" && part.state === "approval-requested")) return "asking"
  if (failed) return "unavailable"
  return live ? "thinking" : "answered"
}

function AssistantMessage({ message, pose, live, host, onAnswer }: { readonly message: SidekickMessage; readonly pose: MascotState; readonly live: boolean; readonly host: ToolHost; readonly onAnswer: (approvalId: string, approved: boolean) => void }) {
  const drawn = message.parts.some((part) => part.type === "text" || part.type === "dynamic-tool" || part.type === "data-view")
  return (
    <Message data-role="assistant" className="items-start text-body">
      <MessageAvatar className="mt-0.5 min-w-0 self-start bg-transparent">
        <Mascot size={28} state={pose} label={pose === "asking" ? "Sidekick is waiting for you" : pose === "thinking" ? "Sidekick is working" : undefined} />
      </MessageAvatar>
      <MessageContent className="gap-3">
        {pose === "thinking" && !drawn && <p role="status" className="text-body text-muted-foreground">Thinking…</p>}
        {message.parts.map((part, index) => {
          if (part.type === "text") return <Streamdown key={index} className="text-body [&>*:first-child]:mt-0 [&>*:last-child]:mb-0">{part.text}</Streamdown>
          if (part.type === "dynamic-tool") return <ToolCall key={part.toolCallId} part={part} live={live} onAnswer={onAnswer} />
          if (part.type === "data-view") return <ViewCard key={part.id ?? index} delivery={part.data} host={host} />
          return null
        })}
      </MessageContent>
    </Message>
  )
}

function ScrollToLatest() {
  const { isAtBottom, scrollToBottom } = useStickToBottomContext()
  if (isAtBottom) return null
  return (
    <Button type="button" size="icon-sm" variant="outline" className="absolute bottom-3 start-1/2 -translate-x-1/2 rounded-full rtl:translate-x-1/2" onClick={() => void scrollToBottom()}>
      <ArrowDown aria-hidden /><span className="sr-only">Scroll to the latest message</span>
    </Button>
  )
}

/** The chat, held by the shell's slot so closing the panel keeps it, scoped to one user/org/env tuple. */
export function Sidekick({ scope, scopeKey }: { readonly scope?: StudioScope | null; readonly scopeKey?: string } = {}) {
  // A new chat, or another workspace, is a new transport (a new session) and a new chat id.
  const [epoch, setEpoch] = useState(0)
  // The shell hands a new scope object on every render; the chat is pinned to the scope it opened with.
  const pinned = useRef(scope)
  pinned.current = scope
  const transport = useMemo(() => new SidekickTransport({ scope: pinned.current !== undefined ? pinned.current : getDisplayedScope() }), [scopeKey, epoch])
  useEffect(() => () => transport.close(), [transport])
  const { messages, status, error, stop, regenerate, sendMessage } = useChat<SidekickMessage>({ id: `sidekick:${scopeKey ?? ""}:${epoch}`, transport })
  const assistant = useAssistant()
  const working = status === "submitted" || status === "streaming"

  const send = useCallback((text: string) => {
    const request = text.trim()
    if (request === "" || working) return
    void sendMessage({ text: request })
  }, [sendMessage, working])

  // A prompt handed over from a page (Home's Ask bar) is taken once, only for its matching scope.
  useEffect(() => {
    if (assistant.prompt === null || working) return
    if (assistant.promptScopeKey !== undefined && assistant.promptScopeKey !== scopeKey) return
    const text = assistant.prompt
    assistant.took()
    send(text)
  }, [assistant, scopeKey, send, working])

  // The answer goes to the bridge and into the running turn as a tool-approval-response chunk, so the
  // part records it in stream order (no race with what the bridge sends next).
  const answer = useCallback((approvalId: string, approved: boolean) => {
    transport.respond(approvalId, approved)
  }, [transport])

  const host = useMemo<ToolHost>(() => ({
    callTool: (name, args) => transport.callTool(name, args),
    openLink: (url) => {
      try {
        const parsed = new URL(url, window.location.origin)
        if (parsed.protocol === "https:" || parsed.protocol === "http:") {
          window.open(parsed.href, "_blank", "noopener,noreferrer")
        }
      } catch { /* ignore invalid URLs */ }
    },
  }), [transport])

  const last = messages.at(-1)
  const waiting = working && last?.role === "user"

  return (
    <div className="flex h-full min-h-0 flex-col" data-slot="sidekick">
      <div className="flex items-center gap-2 border-b border-(--panel-border) px-4 py-2">
        <p className="text-meta text-muted-foreground">Acts as you, with Norn's tools only</p>
        <Button size="sm" variant="ghost" className="ms-auto" onClick={() => { void stop(); setEpoch((value) => value + 1) }} disabled={messages.length === 0 && !working}><SquarePen aria-hidden />New chat</Button>
      </div>
      <StickToBottom className="relative min-h-0 flex-1 overflow-y-hidden" initial="smooth" resize="smooth" role="log" aria-live="polite" aria-label="Sidekick conversation">
        <StickToBottom.Content className="flex flex-col gap-5 p-4">
          {messages.length === 0 && (
            <div className="enter flex flex-col items-center gap-2 py-10 text-center">
              <Mascot size={56} state={error ? "unavailable" : "idle"} className="mb-1" />
              <h3 className="text-body">Ask Sidekick</h3>
              <p className="max-w-xs text-body text-muted-foreground">It answers from your workspace through Norn, as you. What you can't see, it can't see either.</p>
            </div>
          )}
          {messages.map((message, index) => message.role === "user"
            ? (
              <Message key={message.id} align="end" data-role="user">
                <MessageContent>
                  <Bubble variant="muted" align="end"><BubbleContent className="text-body whitespace-pre-wrap">{message.parts.map((part) => part.type === "text" ? part.text : "").join("")}</BubbleContent></Bubble>
                </MessageContent>
              </Message>
            )
            : <AssistantMessage key={message.id} message={message} host={host} onAnswer={answer} live={working && index === messages.length - 1} pose={poseOf(message, working && index === messages.length - 1, error !== undefined && index === messages.length - 1)} />)}
          {waiting && (
            <p role="status" className="flex items-center gap-2.5 text-body text-muted-foreground">
              <Mascot size={28} state="thinking" />{status === "submitted" ? "Connecting…" : "Thinking…"}
            </p>
          )}
          {error !== undefined && (
            <Alert variant="destructive" data-slot="sidekick-error">
              <Mascot size={28} state="unavailable" className="row-span-2" />
              <AlertTitle>Sidekick couldn't answer.</AlertTitle>
              <AlertDescription>{error.message}</AlertDescription>
              <div className="col-start-2 mt-2 flex">
                <Button size="sm" variant="outline" onClick={() => void regenerate()}><RotateCcw aria-hidden />Retry</Button>
              </div>
            </Alert>
          )}
        </StickToBottom.Content>
        <ScrollToLatest />
      </StickToBottom>
      <div className="space-y-2.5 border-t border-(--panel-border) p-3">
        {messages.length === 0 && (
          <div className="flex flex-wrap gap-2" aria-label="Suggested questions" role="group">
            {SUGGESTIONS.map((suggestion) => <Button key={suggestion} size="sm" variant="outline" disabled={working} className="h-auto rounded-full py-1.5 font-normal" onClick={() => send(suggestion)}>{suggestion}</Button>)}
          </div>
        )}
        <ChatComposer label="Message Sidekick" placeholder="Ask about your numbers, records or what needs you" status={status} onSubmit={send} onStop={() => void stop()}
          groupClassName="rounded-(--radius-composer)" footer={<span className="px-1.5 text-meta text-muted-foreground">Enter to send · Shift+Enter for a new line</span>} />
      </div>
    </div>
  )
}
