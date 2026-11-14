import { X } from "lucide-react"
import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react"
import { Sheet, SheetContent, SheetDescription, SheetTitle } from "@/components/ui/sheet"
import { useIsMobile } from "@/hooks/use-mobile"

/**
 * The assistant's place in the shell: a full-height panel docked beside the page, which stays
 * visible. The shell renders whatever `content` its caller mounts (the in-app agent); this module
 * only owns whether it is open and a prompt handed to it from a page.
 */
export interface AssistantContent {
  readonly title: string
  readonly content: ReactNode
}

interface AssistantState {
  readonly open: boolean
  setOpen(next: boolean): void
  toggle(): void
  /** Open the assistant with `text` as a prompt for it to take. */
  ask(text: string): void
  /** The prompt a page handed over, until the assistant takes it. */
  readonly prompt: string | null
  /** The assistant took the prompt. */
  took(): void
}

const STORED = "norn.studio.assistant-open"
const Context = createContext<AssistantState | null>(null)

const stored = () => {
  try {
    return globalThis.localStorage?.getItem(STORED) === "1"
  } catch {
    return false
  }
}

export function AssistantProvider({ children }: { readonly children: ReactNode }) {
  const [open, setOpenState] = useState(stored)
  const [prompt, setPrompt] = useState<string | null>(null)
  const setOpen = useCallback((next: boolean) => {
    setOpenState(next)
    try {
      globalThis.localStorage?.setItem(STORED, next ? "1" : "0")
    } catch { /* storage blocked: the panel still opens */ }
  }, [])
  useEffect(() => {
    const toggle = (event: KeyboardEvent) => {
      if (event.key.toLowerCase() === "j" && (event.metaKey || event.ctrlKey) && !event.altKey) {
        event.preventDefault()
        setOpenState((current) => {
          try {
            globalThis.localStorage?.setItem(STORED, current ? "0" : "1")
          } catch { /* best effort */ }
          return !current
        })
      }
    }
    document.addEventListener("keydown", toggle)
    return () => document.removeEventListener("keydown", toggle)
  }, [])
  const value = useMemo<AssistantState>(() => ({
    open,
    setOpen,
    toggle: () => setOpen(!open),
    ask: (text) => { setPrompt(text); setOpen(true) },
    prompt,
    took: () => setPrompt(null),
  }), [open, prompt, setOpen])
  return <Context.Provider value={value}>{children}</Context.Provider>
}

/** The assistant's state; outside a shell it is closed and asks go nowhere. */
export function useAssistant(): AssistantState {
  return useContext(Context) ?? { open: false, setOpen: () => {}, toggle: () => {}, ask: () => {}, prompt: null, took: () => {} }
}

/**
 * The slot. On a desktop it docks on the inline end beside the page card and the page narrows;
 * on a phone it is a sheet. Its content stays mounted while closed, so a conversation survives.
 */
export function AssistantPanel({ assistant }: { readonly assistant?: AssistantContent }) {
  const { open, setOpen } = useAssistant()
  const mobile = useIsMobile()
  if (assistant === undefined) return null
  const header = (
    <div className="flex h-14 shrink-0 items-center gap-2 border-b px-4">
      <h2 className="text-sm font-medium">{assistant.title}</h2>
      <button type="button" aria-label={`Close ${assistant.title}`} onClick={() => setOpen(false)} className="ms-auto flex size-8 items-center justify-center rounded-full text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"><X aria-hidden className="size-4" /></button>
    </div>
  )
  if (mobile) {
    return (
      <Sheet open={open} onOpenChange={setOpen}>
        <SheetContent className="flex w-full flex-col gap-0 p-0" aria-describedby={undefined}>
          <SheetTitle className="sr-only">{assistant.title}</SheetTitle>
          <SheetDescription className="sr-only">{assistant.title}</SheetDescription>
          {header}
          <div className="min-h-0 flex-1 overflow-y-auto">{assistant.content}</div>
        </SheetContent>
      </Sheet>
    )
  }
  return (
    <aside data-slot="assistant-panel" data-open={open || undefined} aria-label={assistant.title} hidden={!open}
      onKeyDown={(event) => { if (event.key === "Escape") setOpen(false) }}
      className="flex h-svh w-[min(28rem,38vw)] shrink-0 flex-col overflow-hidden border-s bg-background md:sticky md:top-0 md:h-[calc(100svh-1rem)]">
      {header}
      <div className="min-h-0 flex-1 overflow-y-auto">{assistant.content}</div>
    </aside>
  )
}
