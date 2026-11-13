import type { LucideIcon } from "lucide-react"
import type { ComponentProps, ReactNode } from "react"
import { cn } from "@/lib/utils"

/** The detail colours of design system v1: a chip, a dot or a series takes one; a surface never does. */
export type Tone = "gray" | "green" | "amber" | "red" | "blue" | "violet" | "teal" | "pink" | "orange"

const TONE: Readonly<Record<Tone, string>> = {
  gray: "text-tone-gray bg-tone-gray/10 dark:bg-tone-gray/15",
  green: "text-tone-green bg-tone-green/10 dark:bg-tone-green/15",
  amber: "text-tone-amber bg-tone-amber/12 dark:bg-tone-amber/15",
  red: "text-tone-red bg-tone-red/10 dark:bg-tone-red/15",
  blue: "text-tone-blue bg-tone-blue/10 dark:bg-tone-blue/15",
  violet: "text-tone-violet bg-tone-violet/10 dark:bg-tone-violet/15",
  teal: "text-tone-teal bg-tone-teal/10 dark:bg-tone-teal/15",
  pink: "text-tone-pink bg-tone-pink/10 dark:bg-tone-pink/15",
  orange: "text-tone-orange bg-tone-orange/10 dark:bg-tone-orange/15",
}

const DOT: Readonly<Record<Tone, string>> = {
  gray: "bg-tone-gray", green: "bg-tone-green", amber: "bg-tone-amber", red: "bg-tone-red", blue: "bg-tone-blue",
  violet: "bg-tone-violet", teal: "bg-tone-teal", pink: "bg-tone-pink", orange: "bg-tone-orange",
}

/** A small round mark in a tone; `pulse` for something live. */
export function Dot({ tone, pulse = false, className }: { tone: Tone; pulse?: boolean; className?: string }) {
  return (
    <span aria-hidden className={cn("relative inline-flex size-1.5 shrink-0", className)}>
      {pulse && <span className={cn("absolute inset-0 rounded-full opacity-60 motion-safe:animate-ping", DOT[tone])} />}
      <span className={cn("relative inline-flex size-1.5 rounded-full", DOT[tone])} />
    </span>
  )
}

/**
 * A chip: a short label in a tone, with a dot or an icon in front. Status, kind, freshness, source
 * system and data type all read as chips, so the eye finds them before the prose around them.
 */
export function Chip({ tone = "gray", icon: Icon, dot = false, pulse = false, mono = false, className, children, ...props }: {
  tone?: Tone
  icon?: LucideIcon
  /** A dot in the tone before the label (ignored with an icon). */
  dot?: boolean
  pulse?: boolean
  mono?: boolean
  children: ReactNode
} & Omit<ComponentProps<"span">, "children">) {
  return (
    <span data-slot="chip" data-tone={tone} className={cn("inline-flex h-5 max-w-full shrink-0 items-center gap-1 rounded-md px-1.5 text-xs font-medium whitespace-nowrap", TONE[tone], mono && "font-mono text-[0.6875rem]", className)} {...props}>
      {Icon ? <Icon aria-hidden className="size-3 shrink-0" /> : dot && <Dot tone={tone} pulse={pulse} />}
      {children}
    </span>
  )
}

/** A neutral chip for a fact: a muted label and a value, like "commit 2983" or "DuckDB". */
export function FactChip({ icon: Icon, label, children, className, ...props }: { icon?: LucideIcon; label?: ReactNode; children: ReactNode } & Omit<ComponentProps<"span">, "children">) {
  return (
    <span data-slot="fact-chip" className={cn("inline-flex h-6 max-w-full items-center gap-1.5 rounded-md border border-border/80 bg-card px-2 text-meta whitespace-nowrap tabular-nums", className)} {...props}>
      {Icon && <Icon aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />}
      {label !== undefined && <span className="text-muted-foreground">{label}</span>}
      <span className="min-w-0 truncate text-foreground">{children}</span>
    </span>
  )
}
