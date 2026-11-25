import type { ComponentProps, ReactNode } from "react"
import { cn } from "@/lib/utils"

/**
 * The tones a 6px mark takes (DESIGN.md 4.3 and 4.4). The states use the dot tokens; the kind tones
 * mark a category (a source system, a kind of artefact) and never a state. Chrome is monochrome
 * (DESIGN.md 19.6): a category is told by its word, so every kind tone draws the same neutral mark.
 */
export type DotTone = "live" | "held" | "bad" | "quiet" | "accent" | "blue" | "violet" | "teal" | "pink" | "orange"

/** The fill class for each tone: the one place a dot token becomes a fill (the tick bar reuses it). */
export const DOT: Readonly<Record<DotTone, string>> = {
  live: "bg-dot-live", held: "bg-dot-held", bad: "bg-dot-bad", quiet: "bg-dot-quiet", accent: "bg-dot-accent",
  blue: "bg-muted-foreground", violet: "bg-muted-foreground", teal: "bg-muted-foreground", pink: "bg-muted-foreground", orange: "bg-muted-foreground",
}

/** A 6px round mark in a tone, always beside its word; `pulse` for something live (off under reduced motion). */
export function Dot({ tone, pulse = false, className, ...props }: { readonly tone: DotTone; readonly pulse?: boolean } & Omit<ComponentProps<"span">, "children">) {
  return (
    <span aria-hidden data-slot="dot" data-tone={tone} className={cn("relative inline-flex size-[6px] shrink-0", className)} {...props}>
      {pulse && <span className={cn("absolute inset-0 rounded-full opacity-60 motion-safe:animate-ping motion-safe:[animation-duration:2.4s]", DOT[tone])} />}
      <span className={cn("relative inline-flex size-[6px] rounded-full", DOT[tone])} />
    </span>
  )
}

/**
 * A state, dot first (DESIGN.md 10, StatusDot): the 6px dot and its word in muted text, step 1 unless
 * the caller sets step 2. No frame, no fill, no coloured label: the word carries the state, so the
 * dot is never the only signal. `children` is required for that reason.
 */
export function StatusDot({ tone, pulse = false, children, className, ...props }: {
  readonly tone: DotTone
  readonly pulse?: boolean
  readonly children: ReactNode
} & Omit<ComponentProps<"span">, "children">) {
  return (
    <span data-slot="status-dot" data-tone={tone} className={cn("inline-flex max-w-full min-w-0 shrink-0 items-center gap-1.5 text-meta whitespace-nowrap text-muted-foreground", className)} {...props}>
      <Dot tone={tone} pulse={pulse} />
      {children}
    </span>
  )
}
