import { useChat } from "@ai-sdk/react"
import { DefaultChatTransport, type UIMessage } from "ai"
import { Check, LoaderCircle, Sparkles } from "lucide-react"
import { cn } from "@/lib/utils"
import { useEffect, useMemo, useState, type FormEvent } from "react"
import { Streamdown } from "streamdown"
import { StickToBottom } from "use-stick-to-bottom"
import { Button } from "@/components/ui/button"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle, SheetTrigger } from "@/components/ui/sheet"
import { Textarea } from "@/components/ui/textarea"
import { DecisionCard, VerdictCard } from "./cards.tsx"
import { STEPS, SUGGESTIONS, sessionId, stepOf, verdictOf } from "./verdict.ts"

const ALTERNATIVE = "Suggest a different change that keeps the intent but that the checker would admit, and propose it."

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

/** The assistant (AI SDK chat; scrolling and markdown from use-stick-to-bottom and streamdown): ask for a change in plain words; the server's checker admits or refuses what it proposes. */
export function AgentPanel() {
  const transport = useMemo(() => new DefaultChatTransport({ api: "/api/agent", headers: () => ({ "x-norn-session": sessionId(globalThis.sessionStorage) }) }), [])
  const { messages, sendMessage, status, error, stop, regenerate, clearError } = useChat({ transport })
  const [input, setInput] = useState("")
  const [stopped, setStopped] = useState<ReadonlySet<string>>(new Set())
  const busy = status === "submitted" || status === "streaming"
  const send = (text: string) => { if (text.trim() === "" || busy) return; clearError(); void sendMessage({ text }); setInput("") }
  const submit = (event: FormEvent) => { event.preventDefault(); send(input) }
  const last = messages.at(-1)
  return (
    <div className="flex h-full min-h-0 flex-col">
      <StickToBottom className="relative min-h-0 flex-1 overflow-y-hidden" initial="smooth" resize="smooth" role="log">
        <StickToBottom.Content className="flex flex-col gap-4 p-4">
          {messages.length === 0 && (
            <div className="enter flex flex-col items-center gap-2 py-10 text-center">
              <span className="mb-1 flex size-11 items-center justify-center rounded-2xl bg-muted"><Sparkles className="size-5 text-muted-foreground" /></span>
              <div className="text-base font-semibold">Propose a change</div>
              <p className="max-w-xs text-sm text-muted-foreground">Ask in plain words. The checker on the server admits or refuses every proposal, and names the law when it refuses. Nothing is installed.</p>
            </div>
          )}
          {messages.map((message) => (
            <div key={message.id} className={cn("flex flex-col gap-2 text-sm", message.role === "user" ? "ms-auto max-w-[85%] rounded-2xl bg-muted px-3 py-2" : "max-w-full")}>
              <Parts message={message} />
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
        {messages.length === 0 && <div className="text-meta text-muted-foreground">Try one of these</div>}
        {messages.length === 0 && (
          <div className="flex flex-wrap gap-2">{SUGGESTIONS.map((suggestion) => <Button key={suggestion} size="sm" variant="outline" className="h-auto max-w-full rounded-full py-1.5 text-start font-normal whitespace-normal transition-colors hover:border-foreground/25" onClick={() => send(suggestion)}>{suggestion}</Button>)}</div>
        )}
        <form onSubmit={submit} className="flex items-end gap-2">
          <Textarea value={input} onChange={(event) => setInput(event.target.value)} placeholder="Ask for a change, e.g. let Approve skip Submit" rows={2} className="min-h-0 resize-none"
            onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); send(input) } }} />
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
  return (
    <Sheet>
      <SheetTrigger asChild>
        <Button variant="ghost" size="icon" className="rounded-full text-muted-foreground"><Sparkles className="size-5" /><span className="sr-only">Assistant</span></Button>
      </SheetTrigger>
      <SheetContent className="flex w-full flex-col gap-0 p-0 data-[side=right]:sm:max-w-md">
        <SheetHeader className="border-b">
          <SheetTitle>Assistant</SheetTitle>
          <SheetDescription>Proposals to the purchasing world's definitions, checked on the server.</SheetDescription>
        </SheetHeader>
        <AgentPanel />
      </SheetContent>
    </Sheet>
  )
}
