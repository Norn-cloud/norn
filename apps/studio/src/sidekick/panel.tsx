import { Check, LoaderCircle, SquarePen, Sparkles, X } from "lucide-react"
import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react"
import { Streamdown } from "streamdown"
import { StickToBottom } from "use-stick-to-bottom"
import { useAssistant } from "@/assistant-slot"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Textarea } from "@/components/ui/textarea"
import { cn } from "@/lib/utils"
import { getDisplayedScope, type StudioScope } from "@/lib/definitions"
import type { ToolHost } from "@/widget/bridge"
import type { View } from "@/widget/views"
import type { SidekickSession, ViewDelivery } from "./client.ts"
import { applyUpdate, promptItem, toolWords, viewItem, type Item, type Transcript } from "./transcript.ts"

export { useSidekickServed } from "./served.ts"

/**
 * Sidekick: the person's agent inside Studio. It answers by calling Norn's own tools as the
 * signed-in person (the cell's policy decides what it sees and may do), and each tool's result
 * is drawn here with the same views a chat assistant shows (src/widget/views.tsx).
 */

// The views load with the first one drawn, never with the shell.
const DeliveryView = lazy(() => import("@/widget/views").then((module) => ({ default: module.DeliveryView })))

const VIEWS: readonly View[] = ["answer", "lineage", "dashboard", "receipt", "records", "findings", "plan", "inbox"]
const viewOf = (name: string): View | null => VIEWS.find((view) => view === name) ?? null

export const SUGGESTIONS = ["What's our stock worth?", "Where does that number come from?", "What needs my attention?"] as const

/** Enter sends; Shift+Enter is a new line; Enter that confirms an IME composition is the input method's. */
export const sendsOnEnter = (event: { readonly key: string; readonly shiftKey: boolean; readonly nativeEvent: { readonly isComposing?: boolean } }) =>
  event.key === "Enter" && !event.shiftKey && event.nativeEvent.isComposing !== true

function ToolRow({ item }: { readonly item: Extract<Item, { kind: "tool" }> }) {
  const failed = item.status === "failed"
  const done = item.status === "completed"
  return (
    <div className="flex items-center gap-2 text-meta text-muted-foreground" data-tool={item.tool} data-status={item.status}>
      <span className={cn("flex size-4 shrink-0 items-center justify-center rounded-full", done && "bg-success text-background", failed && "bg-destructive text-background")}>
        {done ? <Check className="size-2.5" strokeWidth={3} /> : failed ? <X className="size-2.5" strokeWidth={3} /> : <LoaderCircle className="size-3.5 animate-spin" />}
      </span>
      <span className={cn(!done && !failed && "text-foreground")}>{toolWords(item.tool)}</span>
      <Badge variant="outline" className="ms-auto font-mono text-meta font-normal">{item.tool}</Badge>
    </div>
  )
}

function ViewCard({ delivery, host }: { readonly delivery: ViewDelivery; readonly host: ToolHost }) {
  const view = viewOf(delivery.view)
  if (view === null) return null
  return (
    <div className="enter overflow-hidden rounded-xl border bg-card" data-view={view}>
      <Suspense fallback={<p className="p-3 text-body text-muted-foreground">Drawing Norn's answer…</p>}>
        <DeliveryView view={view} delivery={{ input: delivery.input, result: delivery.result }} bridge={host} studio={window.location.origin} tool={delivery.tool} />
      </Suspense>
    </div>
  )
}

function Entry({ item, host }: { readonly item: Item; readonly host: ToolHost }) {
  if (item.kind === "user") return <p className="ms-auto max-w-[85%] whitespace-pre-wrap rounded-2xl bg-muted px-3 py-2 text-body">{item.text}</p>
  if (item.kind === "text") return <Streamdown className="text-body [&>*:first-child]:mt-0 [&>*:last-child]:mb-0">{item.text}</Streamdown>
  if (item.kind === "tool") return <ToolRow item={item} />
  return <ViewCard delivery={item.delivery} host={host} />
}

type Status = "idle" | "connecting" | "busy"

/** The chat, held by the shell's slot so closing the panel keeps it, scoped to one user/org/env tuple. */
export function Sidekick({ scope, scopeKey }: { readonly scope?: StudioScope | null; readonly scopeKey?: string } = {}) {
  const [transcript, setTranscript] = useState<Transcript>([])
  const [input, setInput] = useState("")
  const [status, setStatus] = useState<Status>("idle")
  const [error, setError] = useState<string | null>(null)
  const session = useRef<SidekickSession | null>(null)
  const generation = useRef(0)
  const lastScopeKey = useRef(scopeKey)
  const prompts = useRef(0)
  const assistant = useAssistant()

  // Immediately close any prior session and discard in-flight opens/responses when the workspace scope changes.
  useEffect(() => {
    if (lastScopeKey.current === scopeKey) return
    lastScopeKey.current = scopeKey
    generation.current += 1
    session.current?.close()
    session.current = null
    setTranscript([])
    setError(null)
    setStatus("idle")
  }, [scopeKey])

  const open = useCallback(async (expectedGeneration: number): Promise<SidekickSession | null> => {
    if (session.current !== null && !session.current.closed) return session.current
    setStatus("connecting")
    const pinnedScope = scope !== undefined ? scope : getDisplayedScope()
    const { SidekickSession: Session } = await import("./client.ts")
    if (generation.current !== expectedGeneration) return null
    const opened = await Session.open(
      {
        update: (notification) => {
          if (generation.current !== expectedGeneration) return
          setTranscript((current) => applyUpdate(current, notification))
        },
        view: (delivery) => {
          if (generation.current !== expectedGeneration) return
          setTranscript((current) => viewItem(current, delivery))
        },
        closed: () => {
          if (generation.current === expectedGeneration) session.current = null
        },
      },
      { scope: pinnedScope },
    )
    if (generation.current !== expectedGeneration) {
      opened.close()
      return null
    }
    session.current = opened
    return opened
  }, [scope])

  const send = useCallback(async (text: string) => {
    const request = text.trim()
    if (request === "" || status !== "idle") return
    const sendGen = generation.current
    setError(null)
    setInput("")
    setTranscript((current) => promptItem(current, `user:${prompts.current++}`, request))
    try {
      const active = await open(sendGen)
      if (active === null || generation.current !== sendGen) return
      setStatus("busy")
      await active.prompt(request)
    } catch (failure) {
      if (generation.current !== sendGen) return
      setError(failure instanceof Error && failure.message !== "" ? failure.message : "Sidekick could not answer.")
    } finally {
      if (generation.current === sendGen) setStatus("idle")
    }
  }, [open, status])

  // A prompt handed over from a page (Home's Ask bar) is taken once, only for its matching scope.
  useEffect(() => {
    if (assistant.prompt === null || status !== "idle") return
    if (assistant.promptScopeKey !== undefined && assistant.promptScopeKey !== scopeKey) return
    const text = assistant.prompt
    assistant.took()
    void send(text)
  }, [assistant, scopeKey, send, status])

  useEffect(() => () => {
    generation.current += 1
    session.current?.close()
    session.current = null
  }, [])

  const newChat = () => {
    generation.current += 1
    session.current?.close()
    session.current = null
    setTranscript([])
    setError(null)
    setStatus("idle")
  }

  const host = useMemo<ToolHost>(() => ({
    callTool: async (name, args) => {
      const active = session.current
      if (active === null) throw new Error("Start a chat first.")
      return active.callTool(name, args)
    },
    openLink: (url) => { window.open(url, "_blank", "noopener,noreferrer") },
  }), [])

  const submit = (event: FormEvent) => { event.preventDefault(); void send(input) }
  const working = status !== "idle"
  const waiting = working && transcript.at(-1)?.kind === "user"

  return (
    <div className="flex h-full min-h-0 flex-col" data-slot="sidekick">
      <div className="flex items-center gap-2 border-b px-4 py-2">
        <p className="text-meta text-muted-foreground">Acts as you, with Norn's tools only</p>
        <Button size="sm" variant="ghost" className="ms-auto" onClick={newChat} disabled={transcript.length === 0 && !working}><SquarePen aria-hidden />New chat</Button>
      </div>
      <StickToBottom className="relative min-h-0 flex-1 overflow-y-hidden" initial="smooth" resize="smooth" role="log" aria-live="polite">
        <StickToBottom.Content className="flex flex-col gap-3 p-4">
          {transcript.length === 0 && (
            <div className="enter flex flex-col items-center gap-2 py-10 text-center">
              <span className="mb-1 flex size-11 items-center justify-center rounded-2xl bg-muted"><Sparkles className="size-5 text-muted-foreground" /></span>
              <div className="text-body">Ask Sidekick</div>
              <p className="max-w-xs text-body text-muted-foreground">It answers from your workspace through Norn, as you. What you can't see, it can't see either.</p>
            </div>
          )}
          {transcript.map((item) => <Entry key={item.id} item={item} host={host} />)}
          {waiting && <p role="status" className="flex items-center gap-2 text-body text-muted-foreground"><LoaderCircle className="size-4 animate-spin" />{status === "connecting" ? "Connecting…" : "Thinking…"}</p>}
          {error !== null && (
            <div role="alert" className="rounded-xl border bg-card p-3 text-body">
              <p>Sidekick couldn't answer.</p>
              <p className="text-muted-foreground">{error}</p>
            </div>
          )}
        </StickToBottom.Content>
      </StickToBottom>
      <div className="space-y-2.5 border-t p-3">
        {transcript.length === 0 && (
          <div className="flex flex-wrap gap-2">
            {SUGGESTIONS.map((suggestion) => (
              <Button key={suggestion} size="sm" variant="outline" disabled={working} className="h-auto rounded-full py-1.5 font-normal" onClick={() => void send(suggestion)}>{suggestion}</Button>
            ))}
          </div>
        )}
        <form onSubmit={submit} className="flex items-end gap-2">
          <Textarea value={input} onChange={(event) => setInput(event.target.value)} placeholder="Ask about your numbers, records or what needs you" rows={2} className="min-h-0 resize-none" aria-label="Message Sidekick"
            onKeyDown={(event) => { if (sendsOnEnter(event)) { event.preventDefault(); void send(input) } }} />
          {working
            ? <Button type="button" variant="outline" onClick={() => session.current?.cancel()}>Stop</Button>
            : <Button type="submit" disabled={input.trim() === ""}>Send</Button>}
        </form>
      </div>
    </div>
  )
}
