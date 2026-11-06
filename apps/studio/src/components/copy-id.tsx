import { Check, Copy } from "lucide-react"
import { useState } from "react"
import { cn } from "@/lib/utils"

/** An identifier in mono, with a button that copies it exactly. `short` shows its first characters only. */
export function CopyId({ value, short, className }: { readonly value: string; readonly short?: number; readonly className?: string }) {
  const [copied, setCopied] = useState(false)
  const copy = () => void globalThis.navigator?.clipboard?.writeText(value).then(() => { setCopied(true); setTimeout(() => setCopied(false), 1200) })
  return (
    <span className={cn("inline-flex min-w-0 items-center gap-1 font-mono text-xs", className)}>
      <span className="truncate" title={value}>{short !== undefined && value.length > short ? `${value.slice(0, short)}…` : value}</span>
      <button type="button" onClick={copy} className="shrink-0 rounded p-0.5 text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/40">
        {copied ? <Check className="size-3" /> : <Copy className="size-3" />}<span className="sr-only">{copied ? "Copied" : `Copy ${value}`}</span>
      </button>
    </span>
  )
}
