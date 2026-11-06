import { Check, Copy, X } from "lucide-react"
import { useEffect, useRef, useState } from "react"
import { cn } from "@/lib/utils"

/** An identifier in mono, with a button that copies it exactly. `short` shows its first characters only. */
export function CopyId({ value, short, className }: { readonly value: string; readonly short?: number; readonly className?: string }) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle")
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined)
  useEffect(() => () => clearTimeout(timer.current), [])
  const show = (next: "copied" | "failed") => { setState(next); clearTimeout(timer.current); timer.current = setTimeout(() => setState("idle"), 1500) }
  // No clipboard (an insecure context) or a refused permission shows as a failure, never as nothing.
  const copy = () => {
    const clipboard = globalThis.navigator?.clipboard
    if (clipboard === undefined) { show("failed"); return }
    clipboard.writeText(value).then(() => show("copied"), () => show("failed"))
  }
  const Icon = state === "copied" ? Check : state === "failed" ? X : Copy
  return (
    <span className={cn("inline-flex min-w-0 items-center gap-1 font-mono text-xs", className)}>
      <span className="truncate" title={value}>{short !== undefined && value.length > short ? `${value.slice(0, short)}…` : value}</span>
      <button type="button" onClick={copy} className={cn("shrink-0 rounded p-0.5 text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/40", state === "failed" && "text-destructive")}>
        <Icon className="size-3" /><span className="sr-only">{state === "copied" ? "Copied" : state === "failed" ? `Could not copy ${value}` : `Copy ${value}`}</span>
      </button>
    </span>
  )
}
