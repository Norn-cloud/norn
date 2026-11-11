import { useQuery } from "@tanstack/react-query"
import { Link, useParams } from "@tanstack/react-router"
import { Bot, CheckCircle2, CircleDashed, Clock, Database, GitCompareArrows, Hourglass, Scale, ShieldAlert, ShieldCheck, ShieldX, UserCheck, type LucideIcon } from "lucide-react"
import type { ReactNode } from "react"
import { CopyId } from "@/components/copy-id"
import { EmptyState } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { cn } from "@/lib/utils"
import { useView } from "@/lib/view"
import { NO_DATA_CELL } from "../lib/definitions.ts"
import { humanize } from "../lib/cell-read.ts"
import { EPISODES_UNAVAILABLE, readEpisode, type Agreement, type Episode, type OutcomeState, type RuleOutcome, type Verdict } from "../lib/learning/episode.ts"

/**
 * `/episodes/$id`: one decision episode as a timeline (#1220 L-1): what was known, what was
 * suggested, what the person decided, which rules applied, and what happened. A display island:
 * it reads the cell's projection and shows it; it decides nothing and offers no action yet.
 */
export function EpisodePage() {
  const { id = "" } = useParams({ strict: false })
  const read = useQuery({ queryKey: ["episode", id], queryFn: () => readEpisode(id), retry: false })
  const result = read.data
  if (read.isPending) return <div className="mx-auto grid max-w-5xl gap-3" aria-label="Reading the episode"><Skeleton className="h-8 w-80" />{[0, 1, 2, 3, 4].map((i) => <Skeleton key={i} className="h-20 rounded-node" />)}</div>
  if (read.isError) return (
    <div role="alert" className="mx-auto flex max-w-5xl flex-wrap items-center gap-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
      <span>{read.error instanceof Error ? read.error.message : "The episode could not be read."}</span><span className="flex-1" />
      <Button variant="outline" size="sm" onClick={() => void read.refetch()}>Retry</Button>
    </div>
  )
  switch (result?.kind) {
    case "no-cell": return <EmptyState icon={GitCompareArrows} title="No data cell">{NO_DATA_CELL}</EmptyState>
    case "unavailable": return <EmptyState icon={GitCompareArrows} title="Not served yet">{EPISODES_UNAVAILABLE}</EmptyState>
    case "not-found": return <EmptyState icon={GitCompareArrows} title="No such episode">{result.message}</EmptyState>
    case "refused": return <EmptyState icon={GitCompareArrows} title="Can't read this episode">{result.message}</EmptyState>
    case "malformed": return <EmptyState icon={GitCompareArrows} title="Can't show this episode">The cell sent an episode Studio can't read ({result.problem}). Nothing is guessed.</EmptyState>
    case "episode": return <Timeline episode={result.episode} />
    default: return null
  }
}

const AGREEMENT: Readonly<Record<Agreement, { readonly label: string; readonly hint: string; readonly tone: string }>> = {
  accepted: { label: "Accepted the suggestion", hint: "The person chose what was suggested.", tone: "outcome" },
  edited: { label: "Edited the suggestion", hint: "The person chose the suggestion's edited variant.", tone: "agent" },
  // An override is a normal, valuable event, not an error: amber, never the refusal red.
  overridden: { label: "Overrode the suggestion", hint: "The person chose differently from what was suggested. Overrides carry the most to learn from.", tone: "wait" },
  none: { label: "No suggestion", hint: "Nothing was suggested; the person decided alone.", tone: "state" },
}
const RULE: Readonly<Record<RuleOutcome, { readonly label: string; readonly icon: LucideIcon; readonly tone: string }>> = {
  allowed: { label: "Allowed", icon: ShieldCheck, tone: "outcome" },
  would_block: { label: "Would have blocked", icon: ShieldAlert, tone: "wait" },
  blocked: { label: "Blocked", icon: ShieldX, tone: "law" },
}
const WAIVED = { label: "Blocked · waived", icon: ShieldAlert, tone: "wait" } as const
const OUTCOME: Readonly<Record<OutcomeState, { readonly label: string; readonly icon: LucideIcon; readonly tone: string }>> = {
  recorded: { label: "Recorded", icon: CheckCircle2, tone: "outcome" },
  open: { label: "Not measured yet", icon: Hourglass, tone: "state" },
  overdue: { label: "Not measured in time", icon: Clock, tone: "law" },
}
const VERDICT: Readonly<Record<Verdict, string>> = { exception: "Exception", noise: "Noise or data issue", pattern: "Reusable pattern", gap: "System gap" }

/** A soft chip in one of the shared tints. */
function Chip({ tone, children, className }: { readonly tone: string; readonly children: ReactNode; readonly className?: string }) {
  return (
    <span className={cn("inline-flex items-center gap-1 rounded-md px-2 py-0.5 text-xs", className)}
      style={{ background: `color-mix(in oklab, var(--tint-${tone}) 13%, transparent)`, color: `color-mix(in oklab, var(--tint-${tone}) 78%, var(--foreground))` }}>
      {children}
    </span>
  )
}

const when = (ms: number) => new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })

function Timeline({ episode: e }: { readonly episode: Episode }) {
  const { view } = useView()
  const technical = view === "technical"
  const label = (option: string) => e.decision.options.find((o) => o.id === option)?.label ?? option
  const flow = e.run.flow.title ?? humanize(e.run.flow.ruleId.replace(/\.flow\.rule$/, ""))
  const step = e.run.step.label ?? humanize(e.run.step.id)
  const agreement = AGREEMENT[e.agreement]
  const outcome = OUTCOME[e.outcome.state]
  return (
    <div className="mx-auto grid max-w-5xl gap-5">
      <header className="grid gap-2">
        <p className="text-meta text-muted-foreground">{flow} · {step}</p>
        <div className="flex flex-wrap items-center gap-3">
          <h1 className="text-xl font-medium text-balance">{e.decision.question}</h1>
          <Chip tone={e.standing === "measured" ? "outcome" : "state"}>{e.standing === "measured" ? "Measured" : "Context only"}</Chip>
        </div>
        <p className="text-meta text-muted-foreground">
          {e.standing === "measured" ? "Its outcome is recorded, so it can become a curated case." : "Its outcome isn't recorded yet, so it stays context: it can't become a curated case or a score."}
        </p>
      </header>

      <div className="grid gap-5 lg:grid-cols-[minmax(0,1fr)_18rem]">
        <ol aria-label="Episode" className="relative grid gap-3 before:absolute before:top-4 before:bottom-4 before:left-[1.15rem] before:w-px before:bg-node-border">
          <Band icon={Database} tone="state" title="What was known" aside={technical ? `generation ${e.context.definitionGeneration} · as of commit ${e.context.asOfCommitSeq}` : `as of ${when(e.decision.recordedAtMs)}`}>
            {e.context.subjects.length === 0
              ? <p className="text-sm text-muted-foreground">No records were named as this decision's subjects.</p>
              : <ul className="flex flex-wrap gap-2" aria-label="Subjects">{e.context.subjects.map((s) => (
                <li key={`${s.objectType}:${s.id}`}>
                  <Link to="/sources/$concept/$" params={{ concept: s.objectType, _splat: s.id }} className="inline-flex items-center gap-1.5 rounded-lg border border-node-border bg-background px-2.5 py-1 text-sm shadow-node hover:bg-muted/60">
                    {s.label ?? s.id}{technical && <span className="font-mono text-xs text-muted-foreground">{s.objectType}</span>}
                  </Link>
                </li>
              ))}</ul>}
          </Band>

          <Band icon={Bot} tone="agent" title="What was suggested" aside={e.recommendation === null ? undefined : <Chip tone="agent">{humanize(e.recommendation.producer.lifecycle)}</Chip>}>
            {e.recommendation === null
              ? <p className="text-sm text-muted-foreground">Nothing was suggested for this decision.</p>
              : <div className="grid gap-1.5 text-sm">
                <p><span className="text-muted-foreground">Suggested </span><span className="font-medium">{label(e.recommendation.pick)}</span></p>
                {e.recommendation.rationale && <p className="text-muted-foreground">{e.recommendation.rationale}</p>}
                <p className="text-meta text-muted-foreground">by {humanize(e.recommendation.producer.functionId)}{technical && <> · <code className="text-xs">{e.recommendation.producer.manifestPin}</code></>}</p>
              </div>}
          </Band>

          <Band icon={UserCheck} tone="ask" title="What was decided" aside={<Chip tone={agreement.tone}>{agreement.label}</Chip>} emphasis={e.agreement === "overridden"}>
            <div className="grid gap-1.5 text-sm">
              <p><span className="font-medium">{label(e.decision.choice)}</span><span className="text-muted-foreground"> · {e.decision.decidedBy}, {when(e.decision.recordedAtMs)}</span></p>
              {e.decision.note && <blockquote className="border-l-2 border-node-border pl-3 text-muted-foreground">{e.decision.note}</blockquote>}
              <p className="text-meta text-muted-foreground">{agreement.hint}</p>
              <ul className="mt-1 flex flex-wrap gap-1.5" aria-label="Options">
                {e.decision.options.map((o) => (
                  <li key={o.id} className={cn("rounded-md border px-2 py-0.5 text-xs", o.id === e.decision.choice ? "border-foreground/40 bg-muted font-medium" : "border-node-border text-muted-foreground")}>
                    {o.label}{o.id === e.recommendation?.pick && <span className="ms-1 text-[var(--tint-agent)]" aria-label="suggested">•</span>}
                  </li>
                ))}
              </ul>
            </div>
          </Band>

          <Band icon={Scale} tone="law" title="Rules that applied" aside={e.ruleOutcomes.length === 0 ? undefined : `${e.ruleOutcomes.length}`}>
            {e.ruleOutcomes.length === 0
              ? <p className="text-sm text-muted-foreground">No rule was checked for this decision.</p>
              : <ul className="grid gap-2" aria-label="Rules">{e.ruleOutcomes.map((r) => {
                // A waived block shows what actually happened, in the waiver's tone, not a bare "Blocked".
                const s = r.waiver !== null && r.outcome === "blocked" ? WAIVED : RULE[r.outcome]
                return (
                  <li key={r.lawId} className="grid gap-1 rounded-lg border border-node-border bg-background p-2.5 text-sm">
                    <div className="flex flex-wrap items-center gap-2">
                      <s.icon className="size-4 shrink-0" style={{ color: `var(--tint-${s.tone})` }} aria-hidden />
                      <span className="font-medium">{r.title}</span><span className="flex-1" /><Chip tone={s.tone}>{s.label}</Chip>
                    </div>
                    {technical && <code className="text-xs text-muted-foreground">{r.lawId}</code>}
                    {r.waiver && <p className="text-meta text-muted-foreground">Waived by {r.waiver.by}: {r.waiver.reason}</p>}
                  </li>
                )
              })}</ul>}
          </Band>

          <Band icon={outcome.icon} tone={outcome.tone} title="What happened" aside={<Chip tone={outcome.tone}>{outcome.label}</Chip>}>
            <div className="grid gap-1.5 text-sm">
              <p className="text-muted-foreground">Measured by: {e.outcome.spec.title}{e.outcome.spec.within && <> · within {e.outcome.spec.within}</>}</p>
              {e.outcome.record === null
                ? <p className="text-muted-foreground">{e.outcome.state === "overdue" ? "No outcome was recorded in the time allowed." : "No outcome has been recorded yet."}</p>
                : <dl className="grid grid-cols-[minmax(7rem,auto)_1fr] gap-x-4 gap-y-1">
                  {e.outcome.record.values.map((v) => <div key={v.name} className="contents"><dt className="text-muted-foreground">{v.name}</dt><dd className="tabular-nums">{v.display}</dd></div>)}
                  <dt className="text-muted-foreground">Recorded</dt><dd>{when(e.outcome.record.recordedAtMs)}</dd>
                </dl>}
            </div>
          </Band>
        </ol>

        <aside aria-label="Receipts" className="grid content-start gap-3">
          <section className="grid gap-2 rounded-node border border-node-border bg-card p-4 shadow-[var(--node-highlight),var(--elevation-node)]">
            <h2 className="text-sm font-medium">Triage</h2>
            {e.triage === null
              ? <p className="flex items-center gap-1.5 text-meta text-muted-foreground"><CircleDashed className="size-3.5" aria-hidden />Not triaged yet</p>
              : <div className="grid gap-1 text-sm"><Chip tone="agent" className="justify-self-start">{VERDICT[e.triage.verdict]}</Chip><p className="text-muted-foreground">{e.triage.reason}</p><p className="text-meta text-muted-foreground">{e.triage.curator}, {when(e.triage.atMs)}</p></div>}
          </section>
          <section className="grid gap-2 rounded-node border border-node-border bg-card p-4 shadow-[var(--node-highlight),var(--elevation-node)]">
            <h2 className="text-sm font-medium">Receipts</h2>
            <dl className="grid gap-2 text-sm">
              <Receipt label="Decision" hash={e.decision.receiptHash} seq={e.decision.commitSeq} technical={technical} />
              {e.recommendation && <Receipt label="Suggestion" hash={e.recommendation.producer.receiptHash} seq={e.recommendation.producer.commitSeq} technical={technical} />}
              {e.outcome.record && <Receipt label="Outcome" hash={e.outcome.record.receiptHash} seq={e.outcome.record.commitSeq} technical={technical} />}
              {e.ruleOutcomes.filter((r) => r.waiver).map((r) => <Receipt key={r.lawId} label={`Waiver · ${r.title}`} hash={r.waiver!.receiptHash} technical={technical} />)}
            </dl>
            <Link to="/runs/$id" params={{ id: e.run.id }} className="text-meta underline-offset-4 hover:underline">Open the run</Link>
          </section>
        </aside>
      </div>
    </div>
  )
}

function Band({ icon: Icon, tone, title, aside, emphasis = false, children }: {
  readonly icon: LucideIcon; readonly tone: string; readonly title: string; readonly aside?: ReactNode; readonly emphasis?: boolean; readonly children: ReactNode
}) {
  return (
    <li className="relative grid grid-cols-[2.3rem_minmax(0,1fr)] gap-3">
      <span className="relative z-10 mt-3 grid size-9 place-items-center rounded-full border border-node-border bg-card shadow-node" style={{ color: `var(--tint-${tone})` }}>
        <Icon className="size-4" aria-hidden />
      </span>
      <section aria-label={title} className={cn("grid gap-2 rounded-node border bg-card p-4 shadow-[var(--node-highlight),var(--elevation-node)]", emphasis ? "border-[color-mix(in_oklab,var(--tint-wait)_45%,var(--node-border))]" : "border-node-border")}>
        <div className="flex flex-wrap items-center gap-2"><h2 className="text-sm font-medium">{title}</h2><span className="flex-1" />{typeof aside === "string" ? <span className="text-meta text-muted-foreground tabular-nums">{aside}</span> : aside}</div>
        {children}
      </section>
    </li>
  )
}

function Receipt({ label, hash, seq, technical }: { readonly label: string; readonly hash: string; readonly seq?: number; readonly technical: boolean }) {
  return (
    <div className="grid gap-0.5">
      <dt className="text-meta text-muted-foreground">{label}{technical && seq !== undefined && <span className="tabular-nums"> · commit {seq}</span>}</dt>
      <dd className="min-w-0"><CopyId value={hash} short={14} /></dd>
    </div>
  )
}
