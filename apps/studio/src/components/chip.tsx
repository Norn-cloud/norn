import type { LucideIcon } from "lucide-react"
import type { ComponentProps, ReactNode } from "react"
import { cn } from "@/lib/utils"

/** The detail colours of the design system: a dot or a series takes one; a surface or a label fill never does. */
export type Tone = "gray" | "green" | "amber" | "red" | "blue" | "violet" | "teal" | "pink" | "orange"

// Status dots use the saturated dot tokens (DESIGN.md 4.3); other tones mark a kind or a category.
const DOT: Readonly<Record<Tone, string>> = {
  gray: "bg-dot-quiet", green: "bg-dot-live", amber: "bg-dot-held", red: "bg-dot-bad", blue: "bg-tone-blue",
  violet: "bg-tone-violet", teal: "bg-tone-teal", pink: "bg-tone-pink", orange: "bg-tone-orange",
}

/** A 6px round mark in a tone, always beside its word; `pulse` for something live (off under reduced motion). */
export function Dot({ tone, pulse = false, className }: { tone: Tone; pulse?: boolean; className?: string }) {
  return (
    <span aria-hidden className={cn("relative inline-flex size-[6px] shrink-0", className)}>
      {pulse && <span className={cn("absolute inset-0 rounded-full opacity-60 motion-safe:animate-ping", DOT[tone])} />}
      <span className={cn("relative inline-flex size-[6px] rounded-full", DOT[tone])} />
    </span>
  )
}

/**
 * A short label for a state, kind or code (DESIGN.md 10 and 13). A state (any tone but gray, or `dot`)
 * reads dot-first: a 6px dot in its tone and the word in muted text, with no frame or fill (StatusDot).
 * A neutral chip is an outline tag (SourceTag): step 1, a hairline ring, no fill, muted text, and an
 * optional muted icon. Colour is never the only carrier: the word always says it.
 */
export function Chip({ tone = "gray", icon: Icon, dot = false, pulse = false, mono = false, className, children, ...props }: {
  tone?: Tone
  /** A muted icon before a neutral tag's label (a state shows its dot instead). */
  icon?: LucideIcon
  /** Show a gray chip as a state too: a quiet dot and its word. */
  dot?: boolean
  pulse?: boolean
  mono?: boolean
  children: ReactNode
} & Omit<ComponentProps<"span">, "children">) {
  const status = tone !== "gray" || dot
  return (
    <span data-slot="chip" data-tone={tone} data-chip={status ? "status" : "tag"}
      className={cn("inline-flex max-w-full shrink-0 items-center gap-1.5 text-meta whitespace-nowrap text-muted-foreground",
        !status && "h-[1.2rem] rounded-(--radius-chip) px-1.5 ring-1 ring-line ring-inset",
        mono && "font-mono tracking-[-0.01em]", className)} {...props}>
      {status ? <Dot tone={tone} pulse={pulse} /> : Icon && <Icon aria-hidden className="size-3 shrink-0" strokeWidth={1.7} />}
      {children}
    </span>
  )
}

/** A passive info pill for a fact: a muted label and a value, like "commit 2983" or "DuckDB" (DESIGN.md 11 meta row). */
export function FactChip({ icon: Icon, label, children, className, ...props }: { icon?: LucideIcon; label?: ReactNode; children: ReactNode } & Omit<ComponentProps<"span">, "children">) {
  return (
    <span data-slot="fact-chip" className={cn("inline-flex h-6 max-w-full items-center gap-1.5 rounded-full bg-(--well) px-2.5 text-meta whitespace-nowrap tabular-nums ring-1 ring-line ring-inset", className)} {...props}>
      {Icon && <Icon aria-hidden className="size-3.5 shrink-0 text-muted-foreground" strokeWidth={1.6} />}
      {label !== undefined && <span className="text-muted-foreground">{label}</span>}
      <span className="min-w-0 truncate text-foreground">{children}</span>
    </span>
  )
}
