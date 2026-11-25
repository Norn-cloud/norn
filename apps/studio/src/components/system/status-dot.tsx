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

/**
 * What a dot's state reads as in words, so the dot is never its only carrier (DESIGN.md 4.3). Either
 * the word follows it (`children`, rendered as the dot's next siblings with no wrapper, so the
 * caller's flex and gap still apply), or a dot that stands alone names its state (`aria-label`; it
 * is then announced as an image). A conditional word (`{x && "…"}`) does not type-check.
 */
export type Word = Exclude<ReactNode, null | undefined | boolean>
type DotWord =
  | { readonly children: Word; readonly "aria-label"?: never }
  | { readonly "aria-label": string; readonly children?: never }

/** A 6px round mark in a tone, always with its word; `pulse` for something live (off under reduced motion). */
export function Dot({ tone, pulse = false, className, children, ...props }: { readonly tone: DotTone; readonly pulse?: boolean } & DotWord & Omit<ComponentProps<"span">, "children" | "aria-label">) {
  const named = props["aria-label"] !== undefined
  return (
    <>
      <span aria-hidden={!named || undefined} role={named ? "img" : undefined} data-slot="dot" data-tone={tone} className={cn("relative inline-flex size-[6px] shrink-0", className)} {...props}>
        {pulse && <span className={cn("absolute inset-0 rounded-full opacity-60 motion-safe:animate-ping motion-safe:[animation-duration:2.4s]", DOT[tone])} />}
        <span className={cn("relative inline-flex size-[6px] rounded-full", DOT[tone])} />
      </span>
      {children}
    </>
  )
}

/**
 * A state, dot first (DESIGN.md 10, StatusDot): the 6px dot and its word in muted text, step 1 unless
 * the caller sets step 2. No frame, no fill, no coloured label: the word carries the state, so the
 * dot is never the only signal. `children` is required for that reason, and may not be empty.
 */
export function StatusDot({ tone, pulse = false, children, className, ...props }: {
  readonly tone: DotTone
  readonly pulse?: boolean
  readonly children: Word
} & Omit<ComponentProps<"span">, "children">) {
  return (
    <span data-slot="status-dot" data-tone={tone} className={cn("inline-flex max-w-full min-w-0 shrink-0 items-center gap-1.5 text-meta whitespace-nowrap text-muted-foreground", className)} {...props}>
      <Dot tone={tone} pulse={pulse}>{children}</Dot>
    </span>
  )
}
