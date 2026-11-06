import type { CSSProperties, ReactNode } from "react"
import { Bot, ChartLine, Circle, CirclePlay, CircleStop, Flag, GitFork, Hourglass, Play, Scale, Table2, UserRound, Wrench, type LucideIcon } from "lucide-react"
import { cn } from "@/lib/utils"
import { CARD } from "@/lib/canvas/viewport"

/** Each kind of thing on a canvas: its icon, its name and its tint token (`--tint-*` in the theme). */
export const NODE_KINDS: Readonly<Record<string, { readonly icon: LucideIcon; readonly label: string; readonly tint: string }>> = {
  state: { icon: Circle, label: "State", tint: "--tint-tool" },
  start: { icon: Play, label: "Start", tint: "--tint-start" },
  perform: { icon: Wrench, label: "Tool", tint: "--tint-tool" },
  decide: { icon: UserRound, label: "Ask a person", tint: "--tint-ask" },
  route: { icon: GitFork, label: "Route", tint: "--tint-route" },
  sleep: { icon: Hourglass, label: "Wait", tint: "--tint-wait" },
  succeed: { icon: Flag, label: "Outcome", tint: "--tint-outcome" },
  agent: { icon: Bot, label: "Agent", tint: "--tint-agent" },
  tool: { icon: Wrench, label: "Tool binding", tint: "--tint-tool" },
  law: { icon: Scale, label: "Law", tint: "--tint-law" },
  initial: { icon: CirclePlay, label: "Initial", tint: "--tint-outcome" },
  terminal: { icon: CircleStop, label: "Terminal", tint: "--tint-state" },
  chart: { icon: ChartLine, label: "Chart", tint: "--tint-tool" },
  table: { icon: Table2, label: "Table", tint: "--tint-state" },
}

/** Card geometry, so a canvas knows each node's size before it renders: fixed widths, and a height from its lines. */
export { CARD }
export const cardHeight = ({ kindLine = true, subtitle = false }: { readonly kindLine?: boolean; readonly subtitle?: boolean }) =>
  22 + (kindLine ? 16 : 0) + 20 + (subtitle ? 16 : 0)

/** An edge's label: a small pill on the canvas surface, the same on every canvas (exits, lifecycle steps). */
export const EDGE_PILL = "nodrag nopan pointer-events-auto absolute truncate rounded-full border border-node-border bg-card px-2 py-0 text-[11px] leading-4 font-medium text-muted-foreground shadow-node"

/** A small tinted icon chip for a kind; the tint is mixed into a pale fill so it reads as a hint. */
export function KindChip({ kind, className }: { readonly kind: string; readonly className?: string }) {
  const entry = NODE_KINDS[kind] ?? NODE_KINDS.state!
  const Icon = entry.icon
  return (
    <span aria-hidden style={{ "--tint": `var(${entry.tint})` } as CSSProperties}
      className={cn("inline-flex size-7 shrink-0 items-center justify-center rounded-lg bg-[color-mix(in_oklab,var(--tint)_14%,transparent)] text-[var(--tint)] ring-1 ring-[color-mix(in_oklab,var(--tint)_22%,transparent)] ring-inset", className)}>
      <Icon className="size-3.5" strokeWidth={2} />
    </span>
  )
}

/**
 * A node that floats on the canvas: a light card with the depth model's contact and ambient shadows, a hairline
 * border and a top highlight; hover lifts it a little, selection is an accent ring, a finding a destructive one.
 * The handles are its children, so each canvas keeps its own ports.
 */
export function NodeCard({ kind, kindLabel, title, subtitle, problems = 0, selected = false, className, minHeight, children, testId }: {
  readonly kind: string
  /** Overrides the kind's name (a lifecycle state's flags read better than "State"). */
  readonly kindLabel?: string
  readonly title: ReactNode
  readonly subtitle?: ReactNode
  readonly problems?: number
  readonly selected?: boolean
  readonly className?: string
  /** Taller than its content when its exits need the room (they sit a fixed pitch apart). */
  readonly minHeight?: number
  readonly children?: ReactNode
  readonly testId?: string
}) {
  // The kind line names what the card is; it is left out when the title already says it (a start called Start).
  const name = kindLabel ?? NODE_KINDS[kind]?.label ?? kind
  return (
    <div data-testid={testId} style={minHeight ? { minHeight } : undefined} className={cn("canvas-node group relative w-[208px] rounded-node border border-node-border bg-card px-3 py-2.5 text-left",
      selected && "canvas-node-selected", problems > 0 && "canvas-node-failing", className)}>
      {children}
      <div className="flex items-start gap-2.5">
        <KindChip kind={kind} />
        <div className="min-w-0 flex-1">
          {name !== title && <div className="truncate text-xs leading-4 font-medium text-muted-foreground">{name}</div>}
          <div data-node-title className="text-[15px] leading-5 font-semibold tracking-[-0.01em] break-words text-foreground">{title}</div>
          {subtitle && <div className="truncate text-xs leading-4 text-muted-foreground">{subtitle}</div>}
          {problems > 0 && (
            <div className="mt-1 inline-flex items-center gap-1 rounded-full bg-destructive/10 px-1.5 text-[11px] leading-4 font-medium text-destructive">
              <span className="size-1.5 rounded-full bg-destructive" />{problems} {problems === 1 ? "problem" : "problems"}
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

/** The inspector's head for one selected thing: its kind chip, kind and id, so the panel names what it edits. */
export function InspectorHead({ kind, kindLabel, id }: { readonly kind: string; readonly kindLabel?: string; readonly id: string }) {
  return (
    <div className="flex items-center gap-2.5 border-b border-node-border pb-3">
      <KindChip kind={kind} />
      {/* One heading names what the inspector edits, written once: the separator is read, not drawn. */}
      <h3 className="min-w-0">
        <span className="block text-[13px] leading-5 font-semibold">{kindLabel ?? NODE_KINDS[kind]?.label ?? kind}</span>
        <span className="sr-only"> · </span>
        <span className="block truncate font-mono text-[11px] leading-4 font-normal text-muted-foreground">{id}</span>
      </h3>
    </div>
  )
}

/** A group of inspector fields under a quiet header. */
export function InspectorSection({ title, children }: { readonly title: string; readonly children: ReactNode }) {
  return (
    <section className="space-y-3">
      <h3 className="text-xs font-semibold text-muted-foreground">{title}</h3>
      {children}
    </section>
  )
}
