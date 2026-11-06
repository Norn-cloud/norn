import { useQuery } from "@tanstack/react-query"
import { useState } from "react"
import { useRouterState } from "@tanstack/react-router"
import { ArrowRight, ChevronRight, CircleCheck, CircleHelp, CircleX, ShieldCheck } from "lucide-react"
import { KpiTile, StatStrip } from "@/components/kpi-tile"
import { Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Badge } from "@/components/ui/badge"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { cases, field, levelWords, openAfterHash, scalar, targetsGuarantee, verdictWords } from "./lib/guarantees.ts"
import { recordedTransport, type Guarantees } from "./lib/transport.ts"
import type { MenuEntry } from "./shell.tsx"

const TITLE: Readonly<Record<string, string>> = {
  original: "As declared",
  mutant: "With the loss rule removed (a deliberately broken copy)",
}

/** The colour a verdict reads in: refuted red, holding green, unknown muted. */
const tone = (verdict: Guarantees["verdict"]) => verdict.kind === "refuted" ? "text-destructive" : verdict.kind === "bounded-pass" ? "text-success" : "text-muted-foreground"
/** The icon for a verdict: a check, a cross, or a question mark. */
const iconOf = (verdict: Guarantees["verdict"]) => verdict.kind === "bounded-pass" ? CircleCheck : verdict.kind === "refuted" ? CircleX : CircleHelp

/** A verdict as its icon alone, named for screen readers. */
function VerdictIcon({ verdict }: { readonly verdict: Guarantees["verdict"] }) {
  const Icon = iconOf(verdict)
  return <Icon aria-label={verdictWords(verdict)} className={`size-4 shrink-0 ${tone(verdict)}`} />
}

/** A verdict as its icon and words. */
function Verdict({ verdict }: { readonly verdict: Guarantees["verdict"] }) {
  const Icon = iconOf(verdict)
  return <span className={`inline-flex items-center gap-1.5 font-medium ${tone(verdict)}`}><Icon className="size-4" />{verdictWords(verdict)}</span>
}

function Story({ scenario }: { readonly scenario: NonNullable<Guarantees["goals"][number]["scenario"]> }) {
  return (
    <div className="space-y-2 px-4 py-3 text-sm">
      <div className="font-medium">{scenario.kind === "counterexample" ? "The model checker found a case that breaks the law" : "A case the law allows, showing it is not vacuous"}</div>
      <ol className="space-y-1">
        {scenario.states.map((state, step) => (
          <li key={step} className="tabular-nums">
            <span className="text-muted-foreground">{step === 0 ? "Before" : `After step ${step}`}: </span>
            {state.exists
              ? Object.entries(state.values).map(([name, value]) => `${field(name)} ${scalar(value)}`).join(" · ")
              : `no ${scenario.identity} record yet`}
          </li>
        ))}
      </ol>
      <div className="text-meta text-muted-foreground">Written by {scenario.writerArtifactId}.</div>
    </div>
  )
}

function Bounds({ json }: { readonly json: string }) {
  const bounds = JSON.parse(json) as { declaredBoundsProfile?: { enabledCases?: number; inputs?: Record<string, readonly string[]>; exclusions?: Record<string, string> } }
  const profile = bounds.declaredBoundsProfile
  if (!profile) return null
  return (
    <div className="space-y-1 px-4 py-3 text-meta text-muted-foreground">
      <div>Checked {profile.enabledCases} cases{profile.inputs && `, with ${Object.entries(profile.inputs).map(([name, [low, high]]) => `${field(name)} from ${low} to ${high}`).join(" and ")}`}.</div>
      {Object.entries(profile.exclusions ?? {}).map(([name, reason]) => <div key={name}>Not covered, {field(name)}: {reason}</div>)}
    </div>
  )
}

function Document({ name, document }: { readonly name: string; readonly document: Guarantees }) {
  const unknown = document.goals.filter(({ verdict }) => verdict.kind === "unknown").length
  return (
    <section id={`guarantee-${name}`} className="scroll-mt-4 space-y-3">
      <div className="space-y-1 px-1">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="text-section font-medium">{TITLE[name] ?? name}</h2>
          <Badge variant="outline" title="A document the model checker produced, replayed exactly. Not a live connection.">Recorded</Badge>
        </div>
        <p className="text-meta text-muted-foreground">Law {document.lawId} {document.lawVersion} · <Verdict verdict={document.verdict} /></p>
      </div>
      <StatStrip>
        <KpiTile icon={CircleCheck} label="Goals discharged" value={`${document.discharged} / ${document.eligible}`} sub="of the goals this checker supports" />
        <KpiTile icon={CircleHelp} label="Goals unknown" value={`${unknown} / ${document.total}`} sub="outside what this checker supports" />
        <KpiTile icon={ShieldCheck} label="Evidence level" value={document.level.kind === "unknown" ? "None" : "Model"} sub={levelWords(document.level)} />
      </StatStrip>
      <Panel icon={ShieldCheck} label="Each goal">
        <TableCard><Table>
          <TableHeader><TableRow><TableHead>Goal</TableHead><TableHead>Result</TableHead><TableHead className="max-sm:hidden">Evidence</TableHead></TableRow></TableHeader>
          <TableBody>
            {document.goals.map((goal) => (
              <TableRow key={goal.index}>
                <TableCell>{field(goal.goalKind)}</TableCell>
                <TableCell className="whitespace-normal"><Verdict verdict={goal.verdict} />{goal.verdict.kind === "unknown" && <div className="text-meta text-muted-foreground">{goal.verdict.reason}</div>}</TableCell>
                <TableCell className="whitespace-normal text-muted-foreground max-sm:hidden">{levelWords(goal.level)}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table></TableCard>
      </Panel>
      {/* Every recorded trace: a counterexample that breaks the law, or a witness that shows it is not vacuous. */}
      {document.goals.map((goal) => goal.scenario && (
        <Panel key={goal.index} icon={goal.scenario.kind === "counterexample" ? CircleX : CircleCheck}
          label={`${goal.scenario.kind === "counterexample" ? "Counterexample to" : "A case allowed by"} ${field(goal.goalKind)}`}>
          <Well><Story scenario={goal.scenario} /></Well>
        </Panel>
      ))}
      <Panel icon={ShieldCheck} label="How strongly it is checked">
        <Well className="divide-y">
          <div className="px-4 py-3 text-sm"><span className="font-medium">Model checking: </span>{levelWords(document.level)} · {document.tools.map(({ id, version }) => `${id} ${version}`).join(", ")}</div>
          <div className="px-4 py-3 text-sm"><span className="font-medium">Runtime enforcement: </span>{document.runtimeEnforcement.kind === "available" ? "Recorded" : `Not yet: ${document.runtimeEnforcement.reason}`}</div>
          <div className="px-4 py-3 text-sm"><span className="font-medium">Parity tests: </span>{document.parity.kind === "available" ? "Recorded" : `Not yet: ${document.parity.reason}`}</div>
          <Bounds json={document.boundsJson} />
        </Well>
      </Panel>
    </section>
  )
}

type Scenario = NonNullable<Guarantees["goals"][number]["scenario"]>

/** A scenario as a before → after picture: the record before the step, the writer that ran, and each value it wrote, verbatim. */
function BeforeAfter({ scenario }: { readonly scenario: Scenario }) {
  const before = scenario.states[0]
  const after = scenario.states.at(-1)
  return (
    <div className="grid items-stretch gap-2 sm:grid-cols-[minmax(0,1fr)_auto_minmax(0,1.4fr)]">
      <div className="flex min-h-24 flex-col justify-center rounded-xl border border-dashed px-4 py-3 text-center">
        <div className="text-meta text-muted-foreground">Before</div>
        <div className="pt-1 font-medium">{before?.exists ? Object.entries(before.values).map(([name, value]) => `${field(name)} ${scalar(value)}`).join(" · ") : `No ${scenario.identity} yet`}</div>
      </div>
      <ArrowRight aria-label={`then ${scenario.writerArtifactId} runs`} className="mx-auto size-5 self-center text-muted-foreground max-sm:rotate-90" />
      <div className="rounded-xl border border-destructive/40 bg-destructive/5 px-4 py-3">
        <div className="text-meta text-muted-foreground">After</div>
        <dl className="divide-y divide-destructive/15 pt-1">
          {Object.entries(after?.values ?? {}).map(([name, value]) => (
            <div key={name} className="flex items-baseline justify-between gap-3 py-1">
              <dt className="text-meta text-muted-foreground">{field(name)}</dt>
              <dd className="text-xl leading-tight font-light tracking-tight tabular-nums">{scalar(value)}</dd>
            </div>
          ))}
        </dl>
      </div>
    </div>
  )
}

/** The proof story at a glance: the declared law holds on every eligible check; the broken copy is caught by a concrete case. */
function Hero({ declared, broken }: { readonly declared: Guarantees; readonly broken: Guarantees | undefined }) {
  const counterexample = broken?.goals.find(({ scenario }) => scenario?.kind === "counterexample")?.scenario
  const unknown = declared.goals.filter(({ verdict }) => verdict.kind === "unknown").length
  return (
    <div className="grid gap-3 lg:grid-cols-[minmax(0,2fr)_minmax(0,3fr)]">
      <section className="enter flex flex-col rounded-2xl border bg-card p-1.5">
        <h2 className="flex h-9 items-center gap-2 px-2.5 text-sm font-medium"><ShieldCheck aria-hidden className="size-4 text-muted-foreground" />{TITLE.original}</h2>
        <div className="flex flex-1 flex-col justify-between gap-5 rounded-xl border bg-surface-subtle px-5 py-5">
          <div className="space-y-2">
            <div className="flex items-baseline gap-3">
              <CircleCheck aria-hidden className="size-8 shrink-0 self-center text-success" />
              <span className="text-[3.25rem] leading-none font-light tracking-[-0.03em] tabular-nums">{declared.discharged}<span className="text-muted-foreground"> of {declared.eligible}</span></span>
            </div>
            <div className="text-lg font-medium">supported checks proven</div>
          </div>
          <ul className="divide-y rounded-lg border bg-card">
            {declared.goals.filter(({ verdict }) => verdict.kind !== "unknown").map((goal) => (
              <li key={goal.index} className="flex items-center gap-2.5 px-3 py-2 text-sm" title={verdictWords(goal.verdict)}>
                <VerdictIcon verdict={goal.verdict} />
                <span className="first-letter:uppercase">{field(goal.goalKind)}</span>
              </li>
            ))}
            {unknown > 0 && (
              <li className="flex items-center gap-2.5 px-3 py-2 text-sm text-muted-foreground">
                <CircleHelp aria-hidden className="size-4 shrink-0" />
                {unknown} of {declared.total} goals unknown: outside what this checker supports
              </li>
            )}
          </ul>
          <p className="text-sm text-muted-foreground">
            <Verdict verdict={declared.verdict} />{cases(declared.boundsJson) !== undefined && <> · {cases(declared.boundsJson)} cases checked</>} · law <code className="text-meta text-foreground">{declared.lawId}</code>
          </p>
        </div>
      </section>
      {broken && counterexample && <section style={{ animationDelay: "90ms" }} className="enter flex flex-col rounded-2xl border bg-card p-1.5">
        <h2 className="flex h-9 items-center gap-2 px-2.5 text-sm font-medium"><CircleX aria-hidden className="size-4 text-destructive" />A deliberately broken copy is caught</h2>
        <div className="flex flex-1 flex-col gap-4 rounded-xl border bg-surface-subtle px-5 py-5">
          <p className="text-panel text-pretty">With the loss rule removed, the model checker finds a case that breaks the law:</p>
          <BeforeAfter scenario={counterexample} />
          <p className="text-meta text-muted-foreground"><Verdict verdict={broken.verdict} /> · {broken.discharged} of {broken.eligible} supported checks proven · written by <code>{counterexample.writerArtifactId}</code></p>
        </div>
      </section>}
    </div>
  )
}

/** The recorded guarantee documents, for the submenu island. */
export function useGuaranteesMenu(): readonly MenuEntry[] | undefined {
  const read = useQuery({ queryKey: ["guarantees"], queryFn: recordedTransport.guarantees })
  return read.data?.map(({ name }) => ({ href: `#guarantee-${name}`, label: TITLE[name] ?? name }))
}

export function GuaranteesPage() {
  const read = useQuery({ queryKey: ["guarantees"], queryFn: recordedTransport.guarantees })
  const document = (name: string) => read.data?.find((entry) => entry.name === name)?.read.document
  const declared = document("original")
  // A submenu link to one document opens the details it sits in; any other
  // hash change keeps the reader's own open or closed choice.
  const hash = useRouterState({ select: (state) => state.location.hash })
  const [open, setOpen] = useState(() => targetsGuarantee(hash))
  const [seenHash, setSeenHash] = useState(hash)
  if (hash !== seenHash) {
    setSeenHash(hash)
    setOpen(openAfterHash(open, hash))
  }
  return (
    <section className="space-y-8">
      <div className="space-y-2">
        <h1 className="text-title font-medium">Guarantees</h1>
        <p className="max-w-2xl text-muted-foreground">The recorded law, and how strongly it is checked. The model checker explores every case within stated bounds; a broken copy of the definition shows it catches a real mistake.</p>
      </div>
      {read.isPending && <p className="text-muted-foreground">Reading guarantees…</p>}
      {read.isError && <p className="text-destructive">{read.error.message}</p>}
      {declared && <Hero declared={declared} broken={document("mutant")} />}
      {read.data && (
        <details open={open} onToggle={(event) => setOpen(event.currentTarget.open)} className="group/details">
          <summary className="flex w-fit cursor-pointer list-none items-center gap-1.5 rounded-md text-sm font-medium text-muted-foreground outline-none hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/30 [&::-webkit-details-marker]:hidden">
            <ChevronRight aria-hidden className="size-4 transition-transform group-open/details:rotate-90" />Technical details: each goal, the bounds and the tools
          </summary>
          <div className="space-y-8 pt-6">
            {read.data.map(({ name, read: { document } }) => <Document key={name} name={name} document={document} />)}
          </div>
        </details>
      )}
    </section>
  )
}
