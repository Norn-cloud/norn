import { ArrowRight, Eye, PencilLine, Sparkles, Zap, type LucideIcon } from "lucide-react"
import { useEffect, type ReactNode } from "react"
import nornMark from "@/assets/norn-mark.webp"
import { Chip, Dot, type Tone } from "@/components/chip"
import { cn } from "@/lib/utils"
import { SYSTEMS, type Can, type SystemId } from "./lending.ts"

/** A detail tone as a pale wash: the visual well at the top of a step card. */
export const WASH: Readonly<Record<Tone, string>> = {
  gray: "bg-[color-mix(in_oklab,var(--tone-gray)_5%,var(--background))]",
  green: "bg-[color-mix(in_oklab,var(--tone-green)_6%,var(--background))]",
  amber: "bg-[color-mix(in_oklab,var(--tone-amber)_6%,var(--background))]",
  red: "bg-[color-mix(in_oklab,var(--tone-red)_5%,var(--background))]",
  blue: "bg-[color-mix(in_oklab,var(--tone-blue)_5%,var(--background))]",
  violet: "bg-[color-mix(in_oklab,var(--tone-violet)_6%,var(--background))]",
  teal: "bg-[color-mix(in_oklab,var(--tone-teal)_6%,var(--background))]",
  pink: "bg-[color-mix(in_oklab,var(--tone-pink)_5%,var(--background))]",
  orange: "bg-[color-mix(in_oklab,var(--tone-orange)_5%,var(--background))]",
}

/** The small tag every preview screen carries: what is drawn here is not built in Studio yet. */
export function PreviewTag({ children = "Preview" }: { readonly children?: ReactNode }) {
  return <Chip tone="violet" icon={Sparkles} className="h-6 px-2 text-[0.75rem]">{children}</Chip>
}

/**
 * One slide at 1920×1080: soft canvas, the eyebrow, a two-line headline (the second line in the
 * action colour), one muted sentence, the screen's body, and a quiet footer.
 */
export function Slide({ eyebrow, title, accent, lead, preview, children, footer }: {
  readonly eyebrow: string; readonly title: ReactNode; readonly accent?: ReactNode; readonly lead?: ReactNode
  /** What on this slide is a preview of something not built yet; omitted when every part is in Studio. */
  readonly preview?: ReactNode
  readonly children: ReactNode; readonly footer?: ReactNode
}) {
  // A slide reads from across a room: every rem-sized part one step larger than in the app.
  useEffect(() => {
    const root = document.documentElement
    const before = root.style.fontSize
    root.style.fontSize = "17px"
    return () => { root.style.fontSize = before }
  }, [])
  return (
    <main className="flex min-h-svh items-center justify-center bg-[var(--shell)] p-[24px] text-foreground">
      <div className="flex min-h-[calc(100svh-48px)] w-full max-w-[1760px] flex-col rounded-[28px] bg-background px-16 pt-9 pb-7 shadow-[var(--elevation-1)] ring-1 ring-foreground/5">
        <header className="flex flex-col items-center gap-3 text-center">
          <div className="flex items-center gap-2 text-meta font-medium text-muted-foreground">
            <img src={nornMark} alt="" className="size-5" />
            <span>{eyebrow}</span>
            {preview !== undefined && <PreviewTag />}
          </div>
          <h1 className="text-[3.125rem] leading-[1.06] font-semibold tracking-[-0.035em] text-balance">
            {title}
            {accent !== undefined && <><br /><span className="text-[var(--action)]">{accent}</span></>}
          </h1>
          {lead !== undefined && <p className="max-w-[80ch] text-[1.125rem] leading-relaxed text-pretty text-muted-foreground">{lead}</p>}
        </header>
        <div className="flex flex-1 flex-col justify-center pt-8">{children}</div>
        <footer className="flex items-center justify-between gap-4 pt-5 text-meta text-muted-foreground">
          <span className="flex items-center gap-2"><Dot tone="gray" /> A sample story with synthetic data. {preview !== undefined && <span>{preview}</span>}</span>
          {footer}
        </footer>
      </div>
    </main>
  )
}

/** A numbered step: the visual well on top, then a two-word title and one muted sentence. */
export function StepCard({ number, icon: Icon, kicker, tone, title, sentence, children, className }: {
  readonly number: string; readonly icon: LucideIcon; readonly kicker: string; readonly tone: Tone
  readonly title: string; readonly sentence: string; readonly children: ReactNode; readonly className?: string
}) {
  return (
    <section className={cn("flex min-w-0 flex-col rounded-[22px] bg-background p-2.5 shadow-[var(--elevation-node)] ring-1 ring-foreground/5", className)}>
      <div className={cn("flex flex-1 flex-col rounded-[16px] px-6 pt-4 pb-5 ring-1 ring-foreground/[0.04]", WASH[tone])}>
        <div className="flex items-center justify-between pb-4 text-meta text-muted-foreground">
          <span className="flex items-center gap-2 font-medium"><Icon aria-hidden className="size-4" />{kicker}</span>
          <span className="text-xs font-medium tabular-nums">{number}</span>
        </div>
        <div className="flex flex-1 flex-col">{children}</div>
      </div>
      <div className="space-y-1 px-4 pt-4 pb-3">
        <h2 className="text-[1.375rem] font-semibold tracking-[-0.02em]">{title}</h2>
        <p className="text-[0.9375rem] leading-relaxed text-pretty text-muted-foreground">{sentence}</p>
      </div>
    </section>
  )
}

/** The small round arrow between two steps. */
export function StepArrow() {
  return (
    <span aria-hidden className="flex size-9 shrink-0 items-center justify-center self-center rounded-full bg-background text-muted-foreground shadow-[var(--elevation-float)] ring-1 ring-foreground/5">
      <ArrowRight className="size-4" />
    </span>
  )
}

/** A source system as a chip: its name, in its tone. */
export function SourceChip({ system, className }: { readonly system: SystemId; readonly className?: string }) {
  const { name, tone } = SYSTEMS[system]
  return <Chip tone={tone} dot className={className}>{name}</Chip>
}

const CAN: Readonly<Record<Can, { readonly label: string; readonly icon: LucideIcon }>> = {
  see: { label: "See", icon: Eye },
  edit: { label: "Edit", icon: PencilLine },
  act: { label: "Act", icon: Zap },
}

/** One role and what it may do here: allowed verbs dark, the rest faded. */
export function RoleRow({ role, can, note, compact = false }: { readonly role: string; readonly can: readonly Can[]; readonly note?: string; readonly compact?: boolean }) {
  return (
    <div className={cn("flex items-center gap-3", compact ? "text-xs" : "text-sm")}>
      <span className={cn("min-w-0 flex-1 truncate font-medium", compact && "text-[0.8125rem]")}>{role}{note && <span className="font-normal text-muted-foreground"> · {note}</span>}</span>
      <span className="flex items-center gap-1">
        {(Object.keys(CAN) as Can[]).map((verb) => {
          const { label, icon: Icon } = CAN[verb]
          const allowed = can.includes(verb)
          return (
            <span key={verb} title={`${label}: ${allowed ? "allowed" : "not allowed"}`}
              className={cn("inline-flex h-6 items-center gap-1 rounded-md px-1.5 text-xs font-medium", allowed ? "bg-foreground/[0.06] text-foreground" : "text-muted-foreground/45 line-through decoration-muted-foreground/30")}>
              <Icon aria-hidden className="size-3" />{label}
            </span>
          )
        })}
      </span>
    </div>
  )
}

const FACE: Readonly<Record<"violet" | "teal" | "blue" | "amber", string>> = {
  violet: "text-tone-violet bg-tone-violet/12",
  teal: "text-tone-teal bg-tone-teal/12",
  blue: "text-tone-blue bg-tone-blue/12",
  amber: "text-tone-amber bg-tone-amber/14",
}

/** A person's mark: initials on a soft tone. */
export function Person({ initials, tone = "violet", size = "md" }: { readonly initials: string; readonly tone?: keyof typeof FACE; readonly size?: "sm" | "md" | "lg" | "xl" }) {
  return (
    <span aria-hidden className={cn("flex shrink-0 items-center justify-center rounded-full font-semibold tracking-tight ring-4 ring-background",
      size === "xl" ? "size-20 text-2xl" : size === "lg" ? "size-14 text-lg" : size === "md" ? "size-10 text-sm" : "size-7 text-[0.6875rem]",
      FACE[tone])}>{initials}</span>
  )
}
