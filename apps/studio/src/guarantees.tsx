import { useQuery } from "@tanstack/react-query"
import { CircleCheck, CircleHelp, CircleX, ShieldCheck } from "lucide-react"
import { KpiTile, StatStrip } from "@/components/kpi-tile"
import { Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Badge } from "@/components/ui/badge"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { field, levelWords, scalar, verdictWords } from "./lib/guarantees.ts"
import { recordedTransport, type Guarantees } from "./lib/transport.ts"
import type { MenuEntry } from "./shell.tsx"

const TITLE: Readonly<Record<string, string>> = {
  original: "As declared",
  mutant: "With the loss rule removed (a deliberately broken copy)",
}

function Verdict({ verdict }: { readonly verdict: Guarantees["verdict"] }) {
  const Icon = verdict.kind === "bounded-pass" ? CircleCheck : verdict.kind === "refuted" ? CircleX : CircleHelp
  return <span className={`inline-flex items-center gap-1.5 font-medium ${verdict.kind === "refuted" ? "text-destructive" : verdict.kind === "bounded-pass" ? "text-success" : "text-muted-foreground"}`}><Icon className="size-4" />{verdictWords(verdict)}</span>
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

/** The recorded guarantee documents, for the submenu island. */
export function useGuaranteesMenu(): readonly MenuEntry[] | undefined {
  const read = useQuery({ queryKey: ["guarantees"], queryFn: recordedTransport.guarantees })
  return read.data?.map(({ name }) => ({ href: `#guarantee-${name}`, label: TITLE[name] ?? name }))
}

export function GuaranteesPage() {
  const read = useQuery({ queryKey: ["guarantees"], queryFn: recordedTransport.guarantees })
  return (
    <section className="space-y-8">
      <div className="space-y-2">
        <h1 className="text-title font-medium">Guarantees</h1>
        <p className="max-w-2xl text-muted-foreground">The recorded law, and how strongly it is checked. The model checker explores every case within stated bounds; a broken copy of the definition shows it catches a real mistake.</p>
      </div>
      {read.isPending && <p className="text-muted-foreground">Reading guarantees…</p>}
      {read.isError && <p className="text-destructive">{read.error.message}</p>}
      {read.data?.map(({ name, read: { document } }) => <Document key={name} name={name} document={document} />)}
    </section>
  )
}
