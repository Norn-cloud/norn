import type { LucideIcon } from "lucide-react"
import type { ComponentProps, ReactNode } from "react"
import { SourceTag } from "@/components/source-tag"
import { Dot as StatusMark, StatusDot, type DotTone } from "@/components/status-dot"
import { cn } from "@/lib/utils"

/** The detail colours of the design system: a dot or a series takes one; a surface or a label fill never does. */
export type Tone = "gray" | "green" | "amber" | "red" | "blue" | "violet" | "teal" | "pink" | "orange"

// A state reads in the saturated dot tokens (DESIGN.md 4.3); the other tones mark a kind or a category.
const DOT_TONE: Readonly<Record<Tone, DotTone>> = {
  gray: "quiet", green: "live", amber: "held", red: "bad", blue: "blue", violet: "violet", teal: "teal", pink: "pink", orange: "orange",
}

/** A 6px round mark in a tone, always beside its word; `pulse` for something live (off under reduced motion). */
export function Dot({ tone, pulse = false, className }: { tone: Tone; pulse?: boolean; className?: string }) {
  return <StatusMark tone={DOT_TONE[tone]} pulse={pulse} className={className} />
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
  return status
    ? <StatusDot tone={DOT_TONE[tone]} pulse={pulse} data-slot="chip" data-tone={tone} data-chip="status" className={cn(mono && "font-mono tracking-[-0.01em]", className)} {...props}>{children}</StatusDot>
    : <SourceTag icon={Icon} mono={mono} data-slot="chip" data-tone={tone} data-chip="tag" className={className} {...props}>{children}</SourceTag>
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
