import { Chat, useChat } from "@ai-sdk/react"
import { DefaultChatTransport, type UIMessage } from "ai"
import { Bot, LoaderCircle, Sparkles } from "lucide-react"
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
import { EXAMPLES, SUGGESTIONS, sessionId, settleExample, tabStorage, timeline, verdictOf, type ExampleRun, type Verdict } from "./verdict.ts"

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
      return <p key={key} className="flex items-center gap-2 text-sm text-muted-foreground"><LoaderCircle className="size-4 animate-spin" />Asking the checker…</p>
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
}

/** The example route is missing (404) or has no agent service bound (503), or could not be reached: examples cannot run here. */
export const examplesServedBy = (status: number | "unreachable"): boolean => status !== "unreachable" && status !== 404 && status !== 503

export function useAgentState(): AgentState {
  const [chat] = useState(() => new Chat<UIMessage>({ transport: new DefaultChatTransport({ api: "/api/agent", headers: () => ({ "x-norn-session": sessionId(tabStorage()) }) }) }))
  const [input, setInput] = useState("")
  const [stopped, setStopped] = useState<ReadonlySet<string>>(new Set())
  const [examples, setExamples] = useState<readonly ExampleRun[]>([])
  const nextExample = useRef(0)
  const [examplesServed, setExamplesServed] = useState<boolean>()
  // Probe the example route once per page; it accepts only POST, so any answer but 404 or 503 means it is served.
  useEffect(() => {
    let current = true
    fetch("/api/agent/example", { method: "GET", headers: { accept: "application/json" } })
      .then((response) => response.status, () => "unreachable" as const)
      .then((status) => { if (current) setExamplesServed(examplesServedBy(status)) })
    return () => { current = false }
  }, [])
  return { chat, input, setInput, stopped, setStopped, examples, setExamples, nextExample, examplesServed }
}

/** The assistant (AI SDK chat; scrolling and markdown from use-stick-to-bottom and streamdown): ask for a change in plain words; the server's checker admits or refuses what it proposes. */
export function AgentPanel({ state }: { readonly state: AgentState }) {
  const { chat, input, setInput, stopped, setStopped, examples, setExamples, nextExample, examplesServed } = state
  const { messages, sendMessage, status, error, stop, regenerate, clearError } = useChat({ chat })
  const busy = status === "submitted" || status === "streaming"
  const send = (text: string) => { if (text.trim() === "" || busy) return; clearError(); void sendMessage({ text }); setInput("") }
  const submit = (event: FormEvent) => { event.preventDefault(); send(input) }
  const last = messages.at(-1)
  // Example proposals are checked by the server as recorded, without the model.
  const runExample = async (chip: string) => {
    const id = nextExample.current++
    setExamples((list) => [...list, { id, chip, after: messages.length, verdict: "pending" }])
    const settle = (verdict: Verdict | string) => setExamples((list) => settleExample(list, id, verdict))
    try {
      const response = await fetch("/api/agent/example", { method: "POST", headers: { "content-type": "application/json", "x-norn-session": sessionId(tabStorage()) }, body: JSON.stringify({ chip }) })
      const body: unknown = await response.json().catch(() => null)
      settle((response.ok ? verdictOf(body) : undefined) ?? reason(new Error(JSON.stringify(body))))
    } catch {
      settle("The checker could not be reached.")
    }
  }
  return (
    <div className="flex h-full min-h-0 flex-col">
      <StickToBottom className="relative min-h-0 flex-1 overflow-y-hidden" initial="smooth" resize="smooth" role="log">
        <StickToBottom.Content className="flex flex-col gap-4 p-4">
          {messages.length === 0 && examples.length === 0 && (
            <div className="flex flex-col items-center gap-2 py-10 text-center">
              <Bot className="size-8 text-muted-foreground" />
              <div className="font-medium">Propose a change</div>
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
      <div className="space-y-2 border-t p-3">
        <div className="flex flex-wrap gap-2">
          {/* The model's suggestions open a conversation; the example proposals stay offered throughout. */}
          {messages.length === 0 && SUGGESTIONS.map((suggestion) => <Button key={suggestion} size="sm" variant="outline" className="h-auto max-w-full rounded-full py-1.5 text-start whitespace-normal" onClick={() => send(suggestion)}>{suggestion}</Button>)}
          {examplesServed === false && <p className="w-full text-meta text-muted-foreground">Examples need the agent service.</p>}
          {EXAMPLES.map((chip) => (
            <Button key={chip} size="sm" variant="outline" disabled={examplesServed !== true} className="h-auto max-w-full rounded-full py-1.5 text-start whitespace-normal" onClick={() => void runExample(chip)} title="Sends this recorded proposal straight to the checker, without the model.">
              <span className="text-muted-foreground">Example proposal:</span> {chip}
            </Button>
          ))}
        </div>
        <form onSubmit={submit} className="flex items-end gap-2">
          <Textarea value={input} onChange={(event) => setInput(event.target.value)} placeholder="Ask for a change, e.g. record each melt's purity" rows={2} className="min-h-0 resize-none"
            onKeyDown={(event) => { if (sendsOnEnter(event)) { event.preventDefault(); send(input) } }} />
          {busy
            ? <Button type="button" variant="outline" onClick={() => void stop()}>Stop</Button>
            : <Button type="submit" disabled={input.trim() === ""}>Send</Button>}
        </form>
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
