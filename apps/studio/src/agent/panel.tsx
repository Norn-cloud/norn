import { useChat } from "@ai-sdk/react"
import { DefaultChatTransport, type UIMessage } from "ai"
import { Bot, LoaderCircle, Sparkles } from "lucide-react"
import { useMemo, useState, type FormEvent } from "react"
import { Conversation, ConversationContent, ConversationEmptyState, ConversationScrollButton } from "@/components/ai-elements/conversation"
import { Message, MessageContent, MessageResponse } from "@/components/ai-elements/message"
import { Suggestion, Suggestions } from "@/components/ai-elements/suggestion"
import { Button } from "@/components/ui/button"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle, SheetTrigger } from "@/components/ui/sheet"
import { Textarea } from "@/components/ui/textarea"
import { DecisionCard, VerdictCard } from "./cards.tsx"
import { SUGGESTIONS, sessionId, verdictOf } from "./verdict.ts"

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

function Parts({ message }: { readonly message: UIMessage }) {
  return message.parts.map((part, index) => {
    const key = `${message.id}:${index}`
    if (part.type === "text") return message.role === "user" ? <p key={key} className="whitespace-pre-wrap">{part.text}</p> : <MessageResponse key={key}>{part.text}</MessageResponse>
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

/** The assistant: ask for a change in plain words; the server's checker admits or refuses what it proposes. */
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
      <Conversation className="min-h-0 flex-1">
        <ConversationContent>
          {messages.length === 0 && (
            <ConversationEmptyState icon={<Bot className="size-8" />} title="Propose a change" description="Ask in plain words. The checker on the server admits or refuses every proposal, and names the law when it refuses. Nothing is installed." />
          )}
          {messages.map((message) => (
            <Message key={message.id} from={message.role}>
              <MessageContent><Parts message={message} /></MessageContent>
            </Message>
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
        </ConversationContent>
        <ConversationScrollButton />
      </Conversation>
      <div className="space-y-2 border-t p-3">
        {messages.length === 0 && (
          <Suggestions>{SUGGESTIONS.map((suggestion) => <Suggestion key={suggestion} suggestion={suggestion} onClick={send} />)}</Suggestions>
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
