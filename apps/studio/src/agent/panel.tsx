import { Chat, useChat } from "@ai-sdk/react"
import { DefaultChatTransport, type UIMessage } from "ai"
import { Check, LoaderCircle, Sparkles } from "lucide-react"
import { cn } from "@/lib/utils"
import { useEffect, useRef, useState, type FormEvent, type MutableRefObject } from "react"
import { Streamdown } from "streamdown"
import { StickToBottom } from "use-stick-to-bottom"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle, SheetTrigger } from "@/components/ui/sheet"
import { Textarea } from "@/components/ui/textarea"
import { DecisionCard, VerdictCard } from "./cards.tsx"
import { QuestionCard, questionVerdictOf } from "./question.tsx"
import { EXAMPLES, STEPS, SUGGESTIONS, sessionId, settleExample, stepOf, tabStorage, timeline, verdictOf, type ExampleRun, type Verdict } from "./verdict.ts"

const ALTERNATIVE = "Suggest a different change that keeps the intent but that the checker would admit, and propose it."

/** Enter sends; Shift+Enter is a new line, and Enter that confirms an IME composition is the input method's, not a send. */
export const sendsOnEnter = (event: { readonly key: string; readonly shiftKey: boolean; readonly nativeEvent: { readonly isComposing?: boolean } }) =>
  event.key === "Enter" && !event.shiftKey && event.nativeEvent.isComposing !== true

/** The server's refusal text, when the Worker sent one (`{"error": "..."}`), else the transport's own message. */
function reason(error: Error): string {
  try {
    const body = JSON.parse(error.message) as { error?: unknown }
    if (typeof body.error === "string") return body.error
  } catch { /* not JSON: a network or gateway failure */ }
  return "The assistant could not be reached."
}

/** Whether the message's last proposal was refused. */
const refused = (message: UIMessage) => {
  const last = message.parts.filter((part) => part.type === "tool-propose").at(-1)
  const verdict = last && "output" in last ? verdictOf(last.output) : undefined
  return verdict !== undefined && ("error" in verdict || !verdict.admitted)
}

/** The wait made visible: each step ticks off as the stream reaches it, with the seconds so far. */
function Progress({ step }: { readonly step: number }) {
  const [seconds, setSeconds] = useState(0)
  useEffect(() => {
    const started = Date.now()
    const timer = setInterval(() => setSeconds(Math.floor((Date.now() - started) / 1000)), 1000)
    return () => clearInterval(timer)
  }, [])
  return (
    <div className="enter rounded-xl border bg-card p-3.5 text-sm">
      {/* Only the step is announced; the seconds tick visually. */}
      <p role="status" aria-live="polite" className="sr-only">{STEPS[step]}</p>
      <ol className="space-y-2.5">
        {STEPS.map((label, index) => (
          <li key={label} aria-current={index === step ? "step" : undefined} className={cn("flex items-center gap-2.5 transition-colors", index > step && "text-muted-foreground/60")}>
            <span className={cn("flex size-5 shrink-0 items-center justify-center rounded-full border", index < step && "border-transparent bg-success text-background", index === step && "border-ring")}>
              {index < step ? <Check className="size-3" strokeWidth={3} /> : index === step ? <LoaderCircle className="size-3 animate-spin text-ring" /> : null}
            </span>
            <span className={cn(index === step && "font-medium")}>{label}</span>
            {index === step && <span aria-hidden className="ms-auto text-meta text-muted-foreground tabular-nums">{seconds} s</span>}
          </li>
        ))}
      </ol>
      <p className="mt-3 text-meta text-muted-foreground">The checker runs on the server; a full check usually takes 10 to 20 seconds.</p>
    </div>
  )
}

function Parts({ message }: { readonly message: UIMessage }) {
  return message.parts.map((part, index) => {
    const key = `${message.id}:${index}`
    if (part.type === "text") return message.role === "user" ? <p key={key} className="whitespace-pre-wrap">{part.text}</p> : <Streamdown key={key} className="text-sm [&>*:first-child]:mt-0 [&>*:last-child]:mb-0">{part.text}</Streamdown>
    if (part.type === "tool-propose_question") {
      if (part.state === "output-available") {
        const verdict = questionVerdictOf(part.output)
        return verdict ? <QuestionCard key={key} verdict={verdict} /> : <p key={key} className="text-sm text-muted-foreground">The checker's answer could not be read.</p>
      }
      if (part.state === "output-error") return <p key={key} className="text-sm text-destructive">The checker could not run: {part.errorText}</p>
      return <p key={key} className="flex items-center gap-2 text-sm text-muted-foreground"><LoaderCircle className="size-4 animate-spin" />Asking the checker…</p>
    }
    if (part.type === "tool-propose") {
      if (part.state === "output-available") {
        const verdict = verdictOf(part.output)
        return verdict ? <VerdictCard key={key} verdict={verdict} /> : <p key={key} className="text-sm text-muted-foreground">The checker's answer could not be read.</p>
      }
      if (part.state === "output-error") return <p key={key} className="text-sm text-destructive">The checker could not run: {part.errorText}</p>
      return null
    }
    if (part.type === "tool-world" || part.type === "tool-artifact") return <p key={key} className="text-meta text-muted-foreground">Read the definitions</p>
    return null
  })
}

/** The conversation, draft and stopped proposals, held above the sheet so closing it loses none of them. */
export interface AgentState {
  readonly chat: Chat<UIMessage>
  readonly input: string
  readonly setInput: (input: string) => void
  readonly stopped: ReadonlySet<string>
  readonly setStopped: (update: (set: ReadonlySet<string>) => ReadonlySet<string>) => void
  readonly examples: readonly ExampleRun[]
  readonly setExamples: (update: (list: readonly ExampleRun[]) => readonly ExampleRun[]) => void
  readonly nextExample: MutableRefObject<number>
  /** Whether the Worker serves the example checker: unknown until the one probe answers. */
  readonly examplesServed: boolean | undefined
  /** Whether this deployment serves the agent route: unknown until the one probe answers. */
  readonly agentServed?: boolean
}

/** The example route is missing (404) or has no agent service bound (503), or could not be reached: examples cannot run here. */
export const examplesServedBy = (status: number | "unreachable"): boolean => status !== "unreachable" && status !== 404 && status !== 503

/** The agent route is missing (404), has no agent service bound (503), or cannot be reached: the assistant cannot answer here. */
export const agentServedBy = (status: number | "unreachable"): boolean => status !== "unreachable" && status !== 404 && status !== 503

export const AGENT_UNSERVED = "The assistant needs the agent service, which this deployment does not serve yet."

/** How long the probe waits before the route counts as not served. */
export const PROBE_DEADLINE_MS = 5_000
/** An example proposal is checked server-side, so it gets a longer deadline than the route probes. */
export const EXAMPLE_DEADLINE_MS = 30_000

/**
 * The one probe: whether the agent route is served. It always settles, and
 * fails closed: a failed request, an abort (the panel unmounted) or no answer
 * within the deadline all mean not served.
 */
export function probeAgentRoute(
  fetcher: (input: string, init?: RequestInit) => Promise<Response> = fetch,
  { signal, timeoutMs = PROBE_DEADLINE_MS }: { readonly signal?: AbortSignal; readonly timeoutMs?: number } = {},
): Promise<boolean> {
  const controller = new AbortController()
  return new Promise<boolean>((resolve) => {
    let settled = false
    // The probe settles once; cancelling (deadline or the caller's abort) settles it false at once and stops the timer.
    const finish = (served: boolean) => {
      if (settled) return
      settled = true
      clearTimeout(deadline)
      signal?.removeEventListener("abort", cancel)
      resolve(served)
    }
    const cancel = () => { controller.abort(); finish(false) }
    // A timer, not AbortSignal.timeout, so the deadline also runs under fake timers.
    const deadline = setTimeout(cancel, timeoutMs)
    if (signal?.aborted) return cancel()
    signal?.addEventListener("abort", cancel, { once: true })
    fetcher("/api/agent", { method: "GET", headers: { accept: "application/json" }, signal: controller.signal })
      .then((response) => agentServedBy(response.status), () => agentServedBy("unreachable"))
      // A response that lands after a cancel never counts as served.
      .then((served) => finish(served && !controller.signal.aborted))
  })
}

/** Sending is open only once the probe has said the route is served: never while it is pending or after it failed. */
export const canSend = (agentServed: boolean | undefined, text: string, busy: boolean): boolean =>
  agentServed === true && text.trim() !== "" && !busy

export function useAgentState(): AgentState {
  const [chat] = useState(() => new Chat<UIMessage>({ transport: new DefaultChatTransport({ api: "/api/agent", headers: () => ({ "x-norn-session": sessionId(tabStorage()) }) }) }))
  const [input, setInput] = useState("")
  const [stopped, setStopped] = useState<ReadonlySet<string>>(new Set())
  const [examples, setExamples] = useState<readonly ExampleRun[]>([])
  const nextExample = useRef(0)
  const [examplesServed, setExamplesServed] = useState<boolean>()
  // Probe the example route once per page; it accepts only POST, so any answer but 404 or 503 means it is served.
  useEffect(() => {
    const controller = new AbortController()
    let timedOut = false
    // Fails closed: no answer within the deadline means not served. An abort from unmount settles nothing.
    const deadline = setTimeout(() => { timedOut = true; controller.abort() }, PROBE_DEADLINE_MS)
    fetch("/api/agent/example", { method: "GET", headers: { accept: "application/json" }, signal: controller.signal })
      .then((response) => response.status, () => "unreachable" as const)
      .then((status) => { if (timedOut || !controller.signal.aborted) setExamplesServed(examplesServedBy(status)) })
      .finally(() => clearTimeout(deadline))
    return () => { clearTimeout(deadline); controller.abort() }
  }, [])
  const [agentServed, setAgentServed] = useState<boolean>()
  // Probe the agent route once per page; it takes only POST, so any answer but 404 or 503 means it is served.
  useEffect(() => {
    const unmounted = new AbortController()
    void probeAgentRoute(fetch, { signal: unmounted.signal }).then((served) => { if (!unmounted.signal.aborted) setAgentServed(served) })
    return () => unmounted.abort()
  }, [])
  return { chat, input, setInput, stopped, setStopped, examples, setExamples, nextExample, examplesServed, agentServed }
}

/** The assistant (AI SDK chat; scrolling and markdown from use-stick-to-bottom and streamdown): ask for a change in plain words; the server's checker admits or refuses what it proposes. */
export function AgentPanel({ state }: { readonly state: AgentState }) {
  const { chat, input, setInput, stopped, setStopped, examples, setExamples, nextExample, examplesServed } = state
  const { messages, sendMessage, status, error, stop, regenerate, clearError } = useChat({ chat })
  const busy = status === "submitted" || status === "streaming"
  const send = (text: string) => { if (!canSend(state.agentServed, text, busy)) return; clearError(); void sendMessage({ text }); setInput("") }
  const submit = (event: FormEvent) => { event.preventDefault(); send(input) }
  const last = messages.at(-1)
  // Example proposals are checked by the server as recorded, without the model.
  const runExample = async (chip: string) => {
    const id = nextExample.current++
    setExamples((list) => [...list, { id, chip, after: messages.length, verdict: "pending" }])
    const settle = (verdict: Verdict | string) => setExamples((list) => settleExample(list, id, verdict))
    // Aborting also bounds the body read, so a stalled response settles the entry instead of leaving it pending.
    const controller = new AbortController()
    const deadline = setTimeout(() => controller.abort(), EXAMPLE_DEADLINE_MS)
    try {
      const response = await fetch("/api/agent/example", { method: "POST", headers: { "content-type": "application/json", "x-norn-session": sessionId(tabStorage()) }, body: JSON.stringify({ chip }), signal: controller.signal })
      const body: unknown = await response.json().catch(() => null)
      const verdict = response.ok ? verdictOf(body) : undefined
      settle(verdict ?? (response.ok ? "The checker's answer could not be read." : reason(new Error(JSON.stringify(body)))))
    } catch {
      settle("The checker could not be reached.")
    } finally {
      clearTimeout(deadline)
    }
  }
  return (
    <div className="flex h-full min-h-0 flex-col">
      <StickToBottom className="relative min-h-0 flex-1 overflow-y-hidden" initial="smooth" resize="smooth" role="log">
        <StickToBottom.Content className="flex flex-col gap-4 p-4">
          {messages.length === 0 && examples.length === 0 && (
            <div className="enter flex flex-col items-center gap-2 py-10 text-center">
              <span className="mb-1 flex size-11 items-center justify-center rounded-2xl bg-muted"><Sparkles className="size-5 text-muted-foreground" /></span>
              <div className="text-base font-semibold">Propose a change</div>
              <p className="max-w-xs text-sm text-muted-foreground">Ask in plain words. The checker on the server admits or refuses every proposal, and names the law when it refuses. Nothing is installed.</p>
            </div>
          )}
          {/* Examples and chat messages in the order they were sent, so a new result lands at the bottom. */}
          {timeline(messages, examples).map((entry) => entry.kind === "message"
            ? (
              <div key={entry.message.id} className={cn("flex flex-col gap-2 text-sm", entry.message.role === "user" ? "ms-auto max-w-[85%] rounded-2xl bg-muted px-3 py-2" : "max-w-full")}>
                <Parts message={entry.message} />
              </div>
            )
            : (
              <div key={`example:${entry.example.id}`} className="flex flex-col gap-2 text-sm">
                <div className="ms-auto max-w-[85%] rounded-2xl bg-muted px-3 py-2"><Badge variant="outline" className="me-2">Example proposal</Badge>{entry.example.chip}</div>
                {entry.example.verdict === "pending"
                  ? <p className="flex items-center gap-2 text-sm text-muted-foreground"><LoaderCircle className="size-4 animate-spin" />Asking the checker…</p>
                  : typeof entry.example.verdict === "string" ? <p className="text-sm text-destructive">{entry.example.verdict}</p> : <VerdictCard verdict={entry.example.verdict} />}
              </div>
            ))}
          {busy && <Progress key={messages.filter(({ role }) => role === "user").length} step={stepOf(last)} />}
          {last?.role === "assistant" && !busy && refused(last) && !stopped.has(last.id) && (
            <DecisionCard disabled={busy} onStop={() => setStopped((set) => new Set(set).add(last.id))} onAlternative={() => send(ALTERNATIVE)} />
          )}
          {error && (
            <div role="alert" className="rounded-xl border bg-card p-3 text-sm">
              <p className="font-medium">The assistant is unavailable right now.</p>
              <p className="text-muted-foreground">{reason(error)} Studio's recorded pages still work.</p>
              <Button size="sm" variant="outline" className="mt-2" onClick={() => { clearError(); void regenerate() }}>Try again</Button>
            </div>
          )}
        </StickToBottom.Content>
      </StickToBottom>
      <div className="space-y-2.5 border-t p-3">
        {state.agentServed === false && <p role="status" className="text-meta text-muted-foreground">{AGENT_UNSERVED}</p>}
        {state.agentServed === undefined && <p role="status" className="text-meta text-muted-foreground">Checking the agent service…</p>}
        {messages.length === 0 && <div className="text-meta text-muted-foreground">Try one of these</div>}
        <div className="flex flex-wrap gap-2">
          {/* The model's suggestions open a conversation; the example proposals stay offered throughout. */}
          {messages.length === 0 && SUGGESTIONS.map((suggestion) => <Button key={suggestion} size="sm" variant="outline" disabled={state.agentServed !== true} className="h-auto max-w-full rounded-full py-1.5 text-start font-normal whitespace-normal transition-colors hover:border-foreground/25" onClick={() => send(suggestion)}>{suggestion}</Button>)}
          {examplesServed === false && <p className="w-full text-meta text-muted-foreground">Examples need the agent service.</p>}
          {EXAMPLES.map((chip) => (
            <Button key={chip} size="sm" variant="outline" disabled={examplesServed !== true} className="h-auto max-w-full rounded-full py-1.5 text-start font-normal whitespace-normal transition-colors hover:border-foreground/25" onClick={() => void runExample(chip)} title="Sends this recorded proposal straight to the checker, without the model.">
              <span className="text-muted-foreground">Example proposal:</span> {chip}
            </Button>
          ))}
        </div>
        <fieldset disabled={state.agentServed !== true} className="contents">
        <form onSubmit={submit} className="flex items-end gap-2">
          <Textarea value={input} onChange={(event) => setInput(event.target.value)} placeholder="Ask for a change, e.g. record each melt's purity" rows={2} className="min-h-0 resize-none"
            onKeyDown={(event) => { if (sendsOnEnter(event)) { event.preventDefault(); send(input) } }} />
          {busy
            ? <Button type="button" variant="outline" onClick={() => void stop()}>Stop</Button>
            : <Button type="submit" disabled={input.trim() === ""}>Send</Button>}
        </form>
        </fieldset>
      </div>
    </div>
  )
}

/** The page bar's assistant button and its side sheet. */
export function AgentButton() {
  // Radix unmounts the sheet's content when it closes; the state lives here, where it stays mounted.
  const state = useAgentState()
  return (
    <Sheet>
      <SheetTrigger asChild>
        <Button variant="ghost" size="icon" className="rounded-full text-muted-foreground"><Sparkles className="size-5" /><span className="sr-only">Assistant</span></Button>
      </SheetTrigger>
      <SheetContent className="flex w-full flex-col gap-0 p-0 data-[side=right]:sm:max-w-md">
        <SheetHeader className="border-b">
          <SheetTitle>Assistant</SheetTitle>
          <SheetDescription>Proposes changes to the Jewelry world's definitions: melts of gold scrap and the law that fine mass is conserved. The server's checker admits or refuses each one; nothing is installed.</SheetDescription>
          {/* The world the server's checker loads (worker/world.ts). */}
          <Badge variant="outline" className="w-fit">World · Jewelry (synthetic)</Badge>
        </SheetHeader>
        <AgentPanel state={state} />
      </SheetContent>
    </Sheet>
  )
}
