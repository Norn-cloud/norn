import { useChat } from "@ai-sdk/react"
import type { DynamicToolUIPart } from "ai"
import { RotateCcw, SquarePen } from "lucide-react"
import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState } from "react"
import { useAssistant } from "@/assistant-slot"
import { Confirmation, ConfirmationAccepted, ConfirmationAction, ConfirmationActions, ConfirmationRejected, ConfirmationRequest, ConfirmationTitle } from "@/components/ai-elements/confirmation"
import { Conversation, ConversationContent, ConversationEmptyState, ConversationScrollButton } from "@/components/ai-elements/conversation"
import { Message, MessageContent, MessageResponse } from "@/components/ai-elements/message"
import { PromptInput, PromptInputBody, PromptInputFooter, PromptInputSubmit, PromptInputTextarea, PromptInputTools } from "@/components/ai-elements/prompt-input"
import { Suggestion, Suggestions } from "@/components/ai-elements/suggestion"
import { Tool, ToolContent, ToolHeader } from "@/components/ai-elements/tool"
import { Mascot, type MascotState } from "@/components/system/mascot"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import { getDisplayedScope, type StudioScope } from "@/lib/definitions"
import type { ToolHost } from "@/widget/bridge"
import type { View } from "@/widget/views"
import type { ViewDelivery } from "./client.ts"
import { toolWords, type SidekickMessage } from "./transcript.ts"
import { SidekickTransport } from "./transport.ts"

export { useSidekickServed } from "./served.ts"

/**
 * Sidekick: the person's agent inside Studio. It answers by calling Norn's own tools as the
 * signed-in person (the cell's policy decides what it sees and may do), and each tool's result
 * is drawn here with the same views a chat assistant shows (src/widget/views.tsx).
 *
 * The chat is AI Elements (`components/ai-elements/`, DESIGN.md 19.3) over the AI SDK's `useChat`,
 * whose transport (`transport.ts`) speaks ACP to the bridge. Before a tool call that can change
 * anything, the bridge asks the person; the question is the tool's Confirmation, and the answer
 * goes back to the bridge, which runs the call only on "Allow". The cell still decides the call.
 */

// The views load with the first one drawn, never with the shell.
const DeliveryView = lazy(() => import("@/widget/views").then((module) => ({ default: module.DeliveryView })))

const VIEWS: readonly View[] = ["answer", "lineage", "dashboard", "receipt", "records", "findings", "plan", "inbox"]
const viewOf = (name: string): View | null => VIEWS.find((view) => view === name) ?? null

export const SUGGESTIONS = ["What's our stock worth?", "Where does that number come from?", "What needs my attention?"] as const

/** The tool parts' ACP-like status, kept as `data-status` for tests and agents. */
const STATUS: Readonly<Record<DynamicToolUIPart["state"], string>> = {
  "input-streaming": "pending",
  "input-available": "in_progress",
  "approval-requested": "awaiting_approval",
  "approval-responded": "in_progress",
  "output-available": "completed",
  "output-error": "failed",
  "output-denied": "declined",
}

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

function ToolCall({ part, onAnswer }: { readonly part: DynamicToolUIPart; readonly onAnswer: (approvalId: string, approved: boolean) => void }) {
  const asking = part.state === "approval-requested"
  // A call waiting on the person opens by itself; otherwise the reader opens it.
  const [open, setOpen] = useState(asking)
  useEffect(() => { if (asking) setOpen(true) }, [asking])
  return (
    <Tool data-tool={part.toolName} data-status={STATUS[part.state]} open={open} onOpenChange={setOpen}>
      <ToolHeader type="dynamic-tool" toolName={part.toolName} state={part.state} title={toolWords(part.toolName)} />
      <ToolContent className="space-y-3 px-3 pb-3">
        <CallInput input={part.input} />
        {part.state === "output-error" && <p className="text-body text-destructive-text">{part.errorText}</p>}
        <Confirmation approval={part.approval} state={part.state} className="border-(--panel-border) bg-well">
          <ConfirmationTitle>
            <ConfirmationRequest>Sidekick wants to run <span className="font-mono text-meta">{part.toolName}</span> as you. The workspace's laws and policy still decide it.</ConfirmationRequest>
            <ConfirmationAccepted>You allowed this call.</ConfirmationAccepted>
            <ConfirmationRejected>You declined; nothing ran.</ConfirmationRejected>
          </ConfirmationTitle>
          {part.approval !== undefined && (
            <ConfirmationActions>
              <ConfirmationAction variant="outline" onClick={() => onAnswer(part.approval!.id, false)}>Decline</ConfirmationAction>
              <ConfirmationAction onClick={() => onAnswer(part.approval!.id, true)}>Allow</ConfirmationAction>
            </ConfirmationActions>
          )}
        </Confirmation>
      </ToolContent>
    </Tool>
  )
}

/** The mascot's pose for an assistant message: what Sidekick is doing in it right now. */
function poseOf(message: SidekickMessage, live: boolean, failed: boolean): MascotState {
  if (message.parts.some((part) => part.type === "dynamic-tool" && part.state === "approval-requested")) return "asking"
  if (failed) return "unavailable"
  return live ? "thinking" : "answered"
}

function AssistantMessage({ message, pose, host, onAnswer }: { readonly message: SidekickMessage; readonly pose: MascotState; readonly host: ToolHost; readonly onAnswer: (approvalId: string, approved: boolean) => void }) {
  return (
    <div className="flex gap-2.5" data-role="assistant">
      <Mascot size={28} state={pose} className="mt-0.5" label={pose === "asking" ? "Sidekick is waiting for you" : pose === "thinking" ? "Sidekick is working" : undefined} />
      <Message from="assistant" className="min-w-0 max-w-full">
        <MessageContent className="w-full gap-3">
          {pose === "thinking" && !message.parts.some((part) => part.type === "text" || part.type === "dynamic-tool" || part.type === "data-view") && <p role="status" className="text-body text-muted-foreground">Thinking…</p>}
          {message.parts.map((part, index) => {
            if (part.type === "text") return <MessageResponse key={index} className="text-body">{part.text}</MessageResponse>
            if (part.type === "dynamic-tool") return <ToolCall key={part.toolCallId} part={part} onAnswer={onAnswer} />
            if (part.type === "data-view") return <ViewCard key={part.id ?? index} delivery={part.data} host={host} />
            return null
          })}
        </MessageContent>
      </Message>
    </div>
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
  const chat = useChat<SidekickMessage>({ id: `sidekick:${scopeKey ?? ""}:${epoch}`, transport })
  const { messages, status, error, stop, regenerate, sendMessage, addToolApprovalResponse } = chat
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

  const answer = useCallback((approvalId: string, approved: boolean) => {
    void addToolApprovalResponse({ id: approvalId, approved })
    transport.respond(approvalId, approved)
  }, [addToolApprovalResponse, transport])

  const host = useMemo<ToolHost>(() => ({
    callTool: (name, args) => transport.callTool(name, args),
    openLink: (url) => { window.open(url, "_blank", "noopener,noreferrer") },
  }), [transport])

  const last = messages.at(-1)
  const waiting = working && last?.role === "user"

  return (
    <div className="flex h-full min-h-0 flex-col" data-slot="sidekick">
      <div className="flex items-center gap-2 border-b border-(--panel-border) px-4 py-2">
        <p className="text-meta text-muted-foreground">Acts as you, with Norn's tools only</p>
        <Button size="sm" variant="ghost" className="ms-auto" onClick={() => { void stop(); setEpoch((value) => value + 1) }} disabled={messages.length === 0 && !working}><SquarePen aria-hidden />New chat</Button>
      </div>
      <Conversation className="min-h-0" aria-live="polite" aria-label="Sidekick conversation">
        <ConversationContent className="gap-5 p-4">
          {messages.length === 0 && (
            <ConversationEmptyState className="enter py-10" title="Ask Sidekick" description="It answers from your workspace through Norn, as you. What you can't see, it can't see either."
              icon={<Mascot size={56} state={error ? "unavailable" : "idle"} />} />
          )}
          {messages.map((message, index) => message.role === "user"
            ? <Message key={message.id} from="user" data-role="user"><MessageContent>{message.parts.map((part, at) => part.type === "text" ? <p key={at} className="whitespace-pre-wrap text-body">{part.text}</p> : null)}</MessageContent></Message>
            : <AssistantMessage key={message.id} message={message} host={host} onAnswer={answer} pose={poseOf(message, working && index === messages.length - 1, error !== undefined && index === messages.length - 1)} />)}
          {waiting && (
            <p role="status" className="flex items-center gap-2.5 text-body text-muted-foreground">
              <Mascot size={28} state="thinking" />{status === "submitted" ? "Connecting…" : "Thinking…"}
            </p>
          )}
          {error !== undefined && (
            <Alert variant="destructive" data-slot="sidekick-error">
              <AlertTitle>Sidekick couldn't answer.</AlertTitle>
              <AlertDescription>{error.message}</AlertDescription>
              <div className="col-start-1 mt-2 flex">
                <Button size="sm" variant="outline" onClick={() => void regenerate()}><RotateCcw aria-hidden />Retry</Button>
              </div>
            </Alert>
          )}
        </ConversationContent>
        <ConversationScrollButton />
      </Conversation>
      <div className="space-y-2.5 border-t border-(--panel-border) p-3">
        {messages.length === 0 && (
          <Suggestions aria-label="Suggested questions">
            {SUGGESTIONS.map((suggestion) => <Suggestion key={suggestion} suggestion={suggestion} disabled={working} onClick={send} />)}
          </Suggestions>
        )}
        <PromptInput onSubmit={({ text }) => send(text)} groupClassName="rounded-(--radius-composer)">
          <PromptInputBody>
            <PromptInputTextarea aria-label="Message Sidekick" placeholder="Ask about your numbers, records or what needs you" />
          </PromptInputBody>
          <PromptInputFooter>
            <PromptInputTools><span className="px-1.5 text-meta text-muted-foreground">Enter to send · Shift+Enter for a new line</span></PromptInputTools>
            <PromptInputSubmit status={status} onStop={() => void stop()} aria-label={working ? "Stop" : "Send"} />
          </PromptInputFooter>
        </PromptInput>
      </div>
    </div>
  )
}
