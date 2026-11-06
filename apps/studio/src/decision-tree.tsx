import { useState } from "react"
import { GitBranch, Wrench } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { cn } from "@/lib/utils"
import { Unavailable } from "./canvas-frame.tsx"
import type { DecisionView } from "./lib/canvas/decision.ts"

/** Why the answer cannot be sent from here yet. */
export const DECIDE_BLOCKED = "Decisions are sent from the inbox once the cell's decide route is live; the cell checks the laws again and issues the receipt."

const RISK_TONE: Record<DecisionView["risk"], string> = {
  high: "bg-destructive/10 text-destructive",
  medium: "bg-amber-500/10 text-amber-700 dark:text-amber-400",
  low: "bg-muted text-muted-foreground",
}

/** One decision level: numbered options, the agent's pick marked, the chosen one opening its follow-up. */
function Level({ view, path, depth, choose }: { readonly view: DecisionView; readonly path: readonly string[]; readonly depth: number; readonly choose: (depth: number, option: string) => void }) {
  const chosen = view.options.find(({ option }) => option.id === path[depth])
  return (
    <div className="flex items-start gap-6">
      <ol className="flex min-w-60 flex-col gap-2" aria-label={depth === 0 ? "Options" : `Options for: ${view.question}`}>
        {depth > 0 && <li className="w-fit rounded-md border bg-card px-3 py-1.5 text-sm font-medium shadow-xs">{view.question}</li>}
        {view.options.map(({ option, agentPick, tools }, index) => {
          const active = option.id === path[depth]
          return (
            <li key={option.id}>
              <button type="button" aria-pressed={active} onClick={() => choose(depth, option.id)}
                className={cn("w-full rounded-lg border bg-card px-3 py-2 text-left text-sm transition-colors hover:bg-muted", active ? "border-primary ring-2 ring-primary/20" : chosen ? "opacity-60" : "")}>
                <span className="flex items-center gap-2">
                  <span className="rounded border px-1.5 font-mono text-xs text-muted-foreground">{index + 1}</span>
                  <span className="font-medium">{option.label}</span>
                  {agentPick && <span className="text-xs text-primary">Agent's pick</span>}
                </span>
                <span className="mt-1 flex flex-wrap items-center gap-2 text-meta text-muted-foreground">
                  <span>{option.scope || "No scope stated"}</span>
                  {option.effect && <span className="font-mono text-xs">{"action" in option.effect ? `action ${option.effect.action}` : `binding ${option.effect.binding}`}</span>}
                  {/* The tools the option leads to, without repeating the binding its effect already names. */}
                  {tools.filter((tool) => !(option.effect && "binding" in option.effect && option.effect.binding === tool)).map((tool) => <span key={tool} className="inline-flex items-center gap-1 font-mono text-xs"><Wrench className="size-3" />{tool}</span>)}
                </span>
              </button>
            </li>
          )
        })}
      </ol>
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
    <div className="space-y-4" data-testid="decision-tree">
      <div className="flex items-center gap-2 text-sm"><GitBranch className="size-4 text-primary" /><span className="font-medium">Decision preview</span><span className="text-muted-foreground">as the decider will see it</span></div>
      <div className="flex items-center gap-6 overflow-x-auto pb-2">
        <div className="w-64 shrink-0 space-y-2">
          <Badge className={cn("border-transparent", RISK_TONE[view.risk])}>{`${view.risk.charAt(0).toUpperCase()}${view.risk.slice(1)} risk`}</Badge>
          <div className="space-y-2 rounded-xl border bg-card p-4 shadow-xs">
            <div className="text-meta text-muted-foreground">{agent ? `${agent.title} · ${agent.model}` : "Asked by this automation"}</div>
            <p className="font-medium">{view.question || "No question yet"}</p>
            <p className="text-meta text-muted-foreground">Context numbers and money at risk come from the cell's admitted questions when it asks.</p>
            <p className="text-meta text-muted-foreground">Decided by: {view.deciderRoles.join(", ") || "no role named"}{view.law ? ` · under ${view.law}` : ""}</p>
            {view.expiresInMs !== undefined && <p className="text-meta text-muted-foreground">Expires {Math.round(view.expiresInMs / 3_600_000)} h after it is asked; then it is withdrawn, not decided.</p>}
          </div>
        </div>
        {view.options.length > 0 ? <Level view={view} path={path} depth={0} choose={choose} /> : <p className="text-sm text-muted-foreground">No options yet.</p>}
      </div>
      <div className="flex flex-wrap items-center gap-3 text-sm">
        <span className="text-muted-foreground">Then</span>
        {numbers.length === 0 ? <span className="text-muted-foreground">choose an option</span> : numbers.map((number, index) => (
          <span key={index} className="flex items-center gap-1">{index > 0 && <span className="text-muted-foreground">–</span>}<span className="rounded border px-1.5 font-mono text-xs">{number}</span></span>
        ))}
        {numbers.length > 1 && <span className="text-meta text-muted-foreground" data-testid="pre-answered">Steps after the first are pre-answered: the cell checks them again when the run reaches them, and they have not run.</span>}
        <Unavailable className="ms-auto" reason={DECIDE_BLOCKED}>Approve this path</Unavailable>
      </div>
    </div>
  )
}
