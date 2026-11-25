import { useState } from "react"
import { Clock, GitBranch, Sparkles, UserRound, Wrench, Zap } from "lucide-react"
import { KindChip } from "@/components/canvas/node-card"
import { SourceTag } from "@/components/system/source-tag"
import { StatusDot, type DotTone } from "@/components/system/status-dot"
import { cn } from "@/lib/utils"
import { Unavailable } from "./canvas-frame.tsx"
import type { DecisionView } from "./lib/canvas/decision.ts"

/** A time limit as the decider should read it: exact, never rounded to whole hours (30 min, 1 h 30 min, 2 d 4 h). */
export function formatDuration(ms: number): string {
  const minutes = Math.round(ms / 60_000)
  if (minutes < 1) return `${Math.round(ms / 1000)} s`
  const days = Math.floor(minutes / 1440)
  const hours = Math.floor((minutes % 1440) / 60)
  const rest = minutes % 60
  return [days ? `${days} d` : "", hours ? `${hours} h` : "", rest ? `${rest} min` : ""].filter(Boolean).join(" ")
}

/** Why the answer cannot be sent from here yet. */
export const DECIDE_BLOCKED = "Decisions are sent from the inbox once the cell's decide route is live; the cell checks the laws again and issues the receipt."

/** A risk is a state: dot first, its word muted (DESIGN.md 13). */
const RISK_TONE: Record<DecisionView["risk"], DotTone> = { high: "bad", medium: "held", low: "quiet" }

/** An option's number: a small round chip, in ink when it is on the chosen path (neutral, never the action colour). */
const OptionNumber = ({ n, on }: { readonly n: number; readonly on: boolean }) => (
  <span className={cn("inline-flex size-6 shrink-0 items-center justify-center rounded-full font-mono text-meta ring-1 ring-inset",
    on ? "bg-foreground text-background ring-transparent" : "bg-card text-muted-foreground ring-node-border")}>{n}</span>
)

/** One decision level: numbered option cards on a spine, the agent's pick marked, the chosen one opening its follow-up. */
function Level({ view, path, depth, choose }: { readonly view: DecisionView; readonly path: readonly string[]; readonly depth: number; readonly choose: (depth: number, option: string) => void }) {
  const chosen = view.options.find(({ option }) => option.id === path[depth])
  return (
    <div className="flex items-start gap-8">
      <div className="space-y-3">
        {depth > 0 && <div className="canvas-node w-fit max-w-72 rounded-node border border-node-border bg-card px-3 py-2 text-body">{view.question}</div>}
        <ol className="relative flex w-72 flex-col gap-2.5 ps-4 before:absolute before:inset-y-5 before:start-0 before:w-px before:bg-node-border" aria-label={depth === 0 ? "Options" : `Options for: ${view.question}`}>
          {view.options.map(({ option, agentPick, tools }, index) => {
            const active = option.id === path[depth]
            const muted = chosen !== undefined && !active
            const extra = tools.filter((tool) => !(option.effect && "binding" in option.effect && option.effect.binding === tool))
            return (
              <li key={option.id} className="relative before:absolute before:top-1/2 before:-start-4 before:h-px before:w-4 before:bg-node-border">
                <button type="button" aria-pressed={active} onClick={() => choose(depth, option.id)}
                  className={cn("canvas-node w-full rounded-node border border-node-border bg-card px-3 py-2.5 text-start text-body outline-none focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring",
                    active && "canvas-node-selected", muted && "bg-surface-subtle/60")}>
                  <span className="flex items-start gap-2.5">
                    <OptionNumber n={index + 1} on={active} />
                    <span className="min-w-0 flex-1 space-y-1">
                      <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
                        <span className={cn("tracking-[-0.01em]", muted && "text-muted-foreground")}>{option.label}</span>
                        {agentPick && <span className="inline-flex items-center gap-1 text-meta leading-5 text-muted-foreground"><Sparkles aria-hidden className="size-3 text-tone-violet" />Agent's pick</span>}
                      </span>
                      <span className="block text-meta text-muted-foreground">{option.scope || "No scope stated"}</span>
                      {(option.effect || extra.length > 0) && (
                        <span className="flex flex-wrap items-center gap-1.5 pt-0.5">
                          {/* The effect's id as sent, a code: an icon says whether it is an action or a tool binding, never a prettified id. */}
                          {option.effect && ("action" in option.effect
                            ? <SourceTag icon={Zap} title="Action"><span className="sr-only">Action </span>{option.effect.action}</SourceTag>
                            : <SourceTag icon={Wrench} title="Tool binding"><span className="sr-only">Tool binding </span>{option.effect.binding}</SourceTag>)}
                          {/* The tools the option leads to, without repeating the binding its effect already names. */}
                          {extra.map((tool) => <SourceTag key={tool} icon={Wrench}>{tool}</SourceTag>)}
                        </span>
                      )}
                    </span>
                  </span>
                </button>
              </li>
            )
          })}
        </ol>
      </div>
      {chosen?.followUp && <Level view={chosen.followUp} path={path} depth={depth + 1} choose={choose} />}
    </div>
  )
}

/**
 * A decision as the person deciding sees it, left to right: the question
 * card, its options, and the decisions that follow the chosen one. Choosing
 * here only marks a path; answering is the cell's, on its decide route.
 */
export function DecisionTree({ view, agent, initialPath = [] }: { readonly view: DecisionView; readonly agent?: { readonly title: string; readonly model: string }; readonly initialPath?: readonly string[] }) {
  const [path, setPath] = useState<readonly string[]>(initialPath)
  const choose = (depth: number, option: string) => setPath((current) => [...current.slice(0, depth), option])
  const numbers: number[] = []
  for (let level: DecisionView | null | undefined = view, depth = 0; level && path[depth] !== undefined; depth += 1) {
    const index = level.options.findIndex(({ option }) => option.id === path[depth])
    if (index < 0) break
    numbers.push(index + 1)
    level = level.options[index]?.followUp
  }
  return (
    <div className="space-y-4 rounded-node border border-node-border bg-canvas-surface p-4" data-testid="decision-tree">
      <div className="flex items-baseline gap-2 text-body"><GitBranch aria-hidden className="size-4 self-center text-[var(--edge-active)]" /><span>Decision preview</span><span className="text-muted-foreground">as the decider will see it</span></div>
      <div className="flex items-center gap-8 overflow-x-auto p-1 pb-3">
        <div className="canvas-node w-72 shrink-0 rounded-node border border-node-border bg-card">
          <div className="space-y-3 p-4">
            <div className="flex items-center justify-between gap-2">
              <span className="flex min-w-0 items-center gap-2"><KindChip kind={agent ? "agent" : "decide"} /><span className="text-meta leading-4 text-muted-foreground">{agent ? `${agent.title} · ${agent.model}` : "Asked by this automation"}</span></span>
              <StatusDot tone={RISK_TONE[view.risk]}>{`${view.risk.charAt(0).toUpperCase()}${view.risk.slice(1)} risk`}</StatusDot>
            </div>
            <p className="text-body leading-6 tracking-[-0.01em] text-balance text-foreground">{view.question || "No question yet"}</p>
            <p className="text-meta text-muted-foreground">Context numbers and money at risk come from the cell's admitted questions when it asks.</p>
          </div>
          <div className="space-y-1 border-t border-node-border px-4 py-2.5 text-meta text-muted-foreground">
            <p className="flex items-start gap-1.5"><UserRound className="mt-0.5 size-3.5 shrink-0" /><span className="min-w-0 break-words">Decided by <span className="text-foreground">{view.deciderRoles.join(", ") || "no role named"}</span>{view.law ? <> under <span className="font-mono text-meta">{view.law}</span></> : null}</span></p>
            {view.expiresInMs !== undefined && <p className="flex items-start gap-1.5"><Clock className="mt-0.5 size-3.5 shrink-0" /><span>Expires {formatDuration(view.expiresInMs)} after it is asked; then it is withdrawn, not decided.</span></p>}
          </div>
        </div>
        {view.options.length > 0 ? <Level view={view} path={path} depth={0} choose={choose} /> : <p className="text-body text-muted-foreground">No options yet.</p>}
      </div>
      <div className="flex flex-wrap items-center gap-3 rounded-xl border border-node-border bg-card px-3 py-2 text-body shadow-node">
        <span className="text-muted-foreground">Then</span>
        {numbers.length === 0 ? <span className="text-muted-foreground">choose an option</span> : numbers.map((number, index) => (
          <span key={index} className="flex items-center gap-1.5">{index > 0 && <span className="text-muted-foreground">–</span>}<OptionNumber n={number} on /></span>
        ))}
        {numbers.length > 1 && <span className="text-meta text-muted-foreground" data-testid="pre-answered">Steps after the first are pre-answered: the cell checks them again when the run reaches them, and they have not run.</span>}
        <Unavailable className="ms-auto" reason={DECIDE_BLOCKED}>Approve this path</Unavailable>
      </div>
    </div>
  )
}
