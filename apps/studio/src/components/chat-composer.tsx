import { ArrowUp, Square } from "lucide-react"
import { useState, type KeyboardEvent, type ReactNode } from "react"
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupTextarea } from "@/components/ui/input-group"
import { cn } from "@/lib/utils"

/** The chat's state as the composer needs it (the AI SDK's `ChatStatus`). */
export type ComposerStatus = "ready" | "submitted" | "streaming" | "error"

/** Enter sends; Shift+Enter is a new line; Enter that confirms an IME composition is the input method's. */
export const sendsOnEnter = (event: { readonly key: string; readonly shiftKey: boolean; readonly nativeEvent: { readonly isComposing?: boolean } }) =>
  event.key === "Enter" && !event.shiftKey && event.nativeEvent.isComposing !== true

/**
 * The text composer for Sidekick and the Ask Norn bar (DESIGN.md 19.3): shadcn's input-group with a
 * textarea, the shadcn docs' chat-composer pattern, in Luma's geometry. Text only (Sidekick's bridge
 * takes text prompts). While a turn runs the send button turns into Stop; an empty field never sends.
 */
export function ChatComposer({ label, placeholder, status = "ready", onSubmit, onStop, submitLabel = "Send", submitText, lead, footer, rows = 2, className, groupClassName }: {
  readonly label: string
  readonly placeholder?: string
  readonly status?: ComposerStatus
  readonly onSubmit: (text: string) => void
  readonly onStop?: () => void
  readonly submitLabel?: string
  /** Words beside the send icon (the Ask bar's "Ask"); icon-only without. */
  readonly submitText?: string
  /** A mark before the field (the Ask bar's mascot). */
  readonly lead?: ReactNode
  /** A quiet line under the field (Sidekick's key hints). */
  readonly footer?: ReactNode
  readonly rows?: number
  readonly className?: string
  readonly groupClassName?: string
}) {
  const [text, setText] = useState("")
  const working = status === "submitted" || status === "streaming"
  const submit = () => {
    const request = text.trim()
    if (request === "" || working) return
    onSubmit(request)
    setText("")
  }
  const keyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (!sendsOnEnter(event)) return
    event.preventDefault()
    submit()
  }
  const stopping = working && onStop !== undefined
  const button = stopping
    ? <InputGroupButton type="button" size={submitText ? "sm" : "icon-sm"} variant="default" aria-label="Stop" className="rounded-full px-3" onClick={onStop}><Square aria-hidden className="fill-current" />{submitText && "Stop"}</InputGroupButton>
    : <InputGroupButton type="submit" size={submitText ? "sm" : "icon-sm"} variant="default" aria-label={submitLabel} className="rounded-full px-3" disabled={text.trim() === "" || working}><ArrowUp aria-hidden />{submitText}</InputGroupButton>
  return (
    <form className={cn("w-full", className)} onSubmit={(event) => { event.preventDefault(); submit() }}>
      <InputGroup className={cn("h-auto", groupClassName)}>
        {lead !== undefined && <InputGroupAddon>{lead}</InputGroupAddon>}
        <InputGroupTextarea aria-label={label} placeholder={placeholder} rows={rows} value={text} onChange={(event) => setText(event.target.value)} onKeyDown={keyDown}
          className={cn("field-sizing-content max-h-48 min-h-0 resize-none", rows === 1 && "py-3 max-sm:py-3.5")} />
        {/* With a footer the send button sits at its end (the shadcn composer); else at the field's end. */}
        {footer === undefined
          ? <InputGroupAddon align="inline-end">{button}</InputGroupAddon>
          : <InputGroupAddon align="block-end" className="justify-between gap-2 pt-0">{footer}{button}</InputGroupAddon>}
      </InputGroup>
    </form>
  )
}
