import { useQuery } from "@tanstack/react-query"
import { BadgeCheck, ChartLine, ShieldCheck } from "lucide-react"
import { useEffect, useState, type ReactNode } from "react"
import { ChoiceColumn, PathSummary, pathNumbers, type ColumnOption } from "@/components/decision/choice-columns"
import { DirtyGuard, EditorToolbar, ValidationPanel, useEditor } from "@/components/editor"
import { EmptyState, Panel } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { questionVerdictOf, type QuestionVerdict } from "./agent/question.tsx"
import { OPERATOR_WORDS, buildQuestion, issuesOf, readCatalog, type Condition, type Draft, type QuestionCatalog } from "./builder.tsx"
import { RunQuestion } from "./run.tsx"

/** One option of a column, as GET /api/questions/recommendations serves it. */
interface Choice { readonly id: string; readonly label: string; readonly detail: string; readonly recommended: boolean; readonly value: Record<string, unknown> }
export interface Recommendations {
  readonly basis: string
  readonly measures: readonly Choice[]
  readonly groupBy: readonly Choice[]
  readonly grains: Readonly<Record<string, readonly Choice[]>>
  readonly conditions: readonly Choice[]
  readonly filterable: readonly { readonly name: string; readonly kind: string; readonly unit: string | null; readonly values: readonly string[] }[]
  readonly windows: readonly Choice[]
}

/** The server's verdict with the question in plain words and that text's hash. */
export interface CheckedQuestion { readonly verdict: QuestionVerdict; readonly restatement: { readonly text: string; readonly sha256: string } }

/** A sign-off: the person read exactly these words (by their hash) and said "This is what I asked for". */
export interface SignOff { readonly sha256: string; readonly atMs: number }

async function readRecommendations(): Promise<Recommendations> {
  const response = await fetch("/api/questions/recommendations", { headers: { accept: "application/json" } })
  if (!response.ok) throw new Error(response.status === 401 ? "Sign in to build questions." : "The question tree is not available on this deployment.")
  return await response.json() as Recommendations
}

async function checkQuestion(question: unknown): Promise<CheckedQuestion> {
  const response = await fetch("/api/questions/check", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ question }) })
  const body = await response.json().catch(() => null) as { restatement?: CheckedQuestion["restatement"] } | null
  const verdict = questionVerdictOf(body)
  if (!response.ok || verdict === undefined || !body?.restatement) throw new Error("The checker could not be reached.")
  return { verdict, restatement: body.restatement }
}

const words = (name: string) => name.replaceAll("_", " ")
const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b)
const options = (choices: readonly Choice[], extra: readonly ColumnOption[] = []): readonly ColumnOption[] =>
  [...choices.map(({ id, label, detail, recommended }) => ({ id, label, detail, marks: recommended ? ["recommended" as const] : [] })), ...extra]

/** The draft the tree opens on: the top pick of every column. */
export function recommendedDraft(tree: Recommendations): Draft {
  const pick = (choices: readonly Choice[]) => choices.find(({ recommended }) => recommended) ?? choices[0]
  const by = pick(tree.groupBy)?.id ?? ""
  const window = (pick(tree.windows)?.value.window ?? {}) as { start?: string; end?: string }
  return {
    title: "",
    measures: pick(tree.measures) ? [pick(tree.measures)?.id ?? ""] : [],
    by,
    grain: pick(tree.grains[by] ?? [])?.id ?? "",
    filters: ((pick(tree.conditions)?.value.filter ?? []) as { path: { property: string }; operator: string; value: unknown }[]).map(({ path, operator, value }) => ({ property: path.property, operator, value: String(value) })),
    start: window.start?.slice(0, 10) ?? "",
    end: window.end?.slice(0, 10) ?? "",
  }
}

const conditionsOf = (choice: Choice): readonly Condition[] =>
  ((choice.value.filter ?? []) as { path: { property: string }; operator: string; value: unknown }[]).map(({ path, operator, value }) => ({ property: path.property, operator, value: String(value) }))

/** Which option of each column the draft is on; "custom" when it was edited beyond the offered options. */
export function selection(tree: Recommendations, draft: Draft) {
  const condition = tree.conditions.find((choice) => same(conditionsOf(choice), draft.filters))?.id ?? (draft.filters.length > 0 ? "custom" : "every")
  const window = tree.windows.find(({ value }) => {
    const w = value.window as { start?: string; end?: string }
    return w.start?.slice(0, 10) === draft.start && w.end?.slice(0, 10) === draft.end
  })?.id ?? "custom"
  return { measure: draft.measures[0], by: draft.by, grain: draft.grain, condition, window }
}

export function QuestionTree({ tree, catalog, guard }: { readonly tree: Recommendations; readonly catalog: QuestionCatalog; readonly guard?: (dirty: boolean) => ReactNode }) {
  const editor = useEditor<Draft>({ initial: recommendedDraft(tree) })
  const { value: draft, set } = editor
  const patch = (next: Partial<Draft>) => set((current) => ({ ...current, ...next }))
  const [checked, setChecked] = useState<CheckedQuestion>()
  const [failure, setFailure] = useState<string>()
  const [signOff, setSignOff] = useState<SignOff>()
  const question = buildQuestion(draft, catalog)
  // The server checks every change (after a pause) and states the question back in plain words.
  useEffect(() => {
    const timer = setTimeout(() => {
      checkQuestion(question).then((result) => { setChecked(result); setFailure(undefined) }, (error: unknown) => setFailure(error instanceof Error ? error.message : String(error)))
    }, 300)
    return () => clearTimeout(timer)
  }, [JSON.stringify(question)])
  const restated = checked?.restatement
  const admitted = checked?.verdict.admitted === true
  // A sign-off holds only for the exact words signed; any edit changes them and clears it.
  const signed = signOff !== undefined && restated !== undefined && signOff.sha256 === restated.sha256 && admitted
  useEffect(() => { if (signOff && restated && signOff.sha256 !== restated.sha256) setSignOff(undefined) }, [restated?.sha256])
  const chosen = selection(tree, draft)
  const issues = issuesOf(checked?.verdict)
  const invalid = (field: string) => issues.some((issue) => issue.field === field || issue.field?.startsWith(`${field}.`))
  const grains = tree.grains[draft.by] ?? []
  const measureOptions = options(tree.measures)
  const groupOptions = options(tree.groupBy)
  const conditionOptions = options(tree.conditions, [{ id: "custom", label: "Another condition", detail: "on a property you choose" }])
  const windowOptions = options(tree.windows, [{ id: "custom", label: "Other dates", detail: "a window you choose" }])
  const numbers = pathNumbers([
    { options: measureOptions, selected: chosen.measure },
    { options: groupOptions, selected: chosen.by },
    ...grains.length > 0 ? [{ options: options(grains), selected: chosen.grain }] : [],
    { options: conditionOptions, selected: chosen.condition },
    { options: windowOptions, selected: chosen.window },
  ])
  const filterable = tree.filterable
  const custom = draft.filters[0] ?? { property: filterable[0]?.name ?? "", operator: "eq", value: "" }
  const property = filterable.find(({ name }) => name === custom.property)
  return (
    <div className="space-y-5">
      {guard?.(editor.dirty)}
      <EditorToolbar editor={editor} />
      <div className="flex items-start gap-6 overflow-x-auto pb-2" data-testid="question-tree">
        <ChoiceColumn title="Measure" field="measures" invalid={invalid("measure") || invalid("measures")} options={measureOptions} selected={chosen.measure} onSelect={(id) => patch({ measures: [id] })} />
        <div className="flex flex-col gap-4">
          <ChoiceColumn title="Group by" field="by" invalid={invalid("by")} options={groupOptions} selected={chosen.by}
            onSelect={(id) => patch({ by: id, grain: (tree.grains[id] ?? []).find(({ recommended }) => recommended)?.id ?? tree.grains[id]?.[0]?.id ?? "" })} />
          {grains.length > 0 && <ChoiceColumn title={`Every (grain of ${words(draft.by)})`} options={options(grains)} selected={chosen.grain} onSelect={(id) => patch({ grain: id })} />}
        </div>
        <ChoiceColumn title="Only where" field="filters" invalid={invalid("filter") || invalid("filters")} options={conditionOptions} selected={chosen.condition}
          onSelect={(id) => {
            if (id === "custom") return patch({ filters: [custom] })
            const choice = tree.conditions.find((entry) => entry.id === id)
            if (choice) patch({ filters: conditionsOf(choice) })
          }}>
          {chosen.condition === "custom" && (
            <div className="grid gap-2 rounded-lg border bg-card p-2 text-sm">
              <select aria-label="Condition property" className="h-8 rounded-3xl bg-input/50 px-3" value={custom.property} onChange={(event) => patch({ filters: [{ ...custom, property: event.target.value, value: "" }] })}>
                {filterable.map(({ name }) => <option key={name} value={name}>{words(name)}</option>)}
              </select>
              <select aria-label="Condition operator" className="h-8 rounded-3xl bg-input/50 px-3" value={custom.operator} onChange={(event) => patch({ filters: [{ ...custom, operator: event.target.value }] })}>
                {catalog.operators.map((operator) => <option key={operator} value={operator}>{OPERATOR_WORDS[operator] ?? operator}</option>)}
              </select>
              {property && property.values.length > 0
                ? <select aria-label="Condition value" className="h-8 rounded-3xl bg-input/50 px-3" value={custom.value} onChange={(event) => patch({ filters: [{ ...custom, value: event.target.value }] })}>
                    <option value="">Choose a value</option>
                    {property.values.map((value) => <option key={value} value={value}>{value}</option>)}
                  </select>
                : <Input aria-label="Condition value" value={custom.value} placeholder={property?.unit ? `in ${property.unit}` : "value"} onChange={(event) => patch({ filters: [{ ...custom, value: event.target.value }] })} />}
            </div>
          )}
        </ChoiceColumn>
        <ChoiceColumn title="Window" field="window" invalid={invalid("window")} options={windowOptions} selected={chosen.window}
          onSelect={(id) => {
            const choice = tree.windows.find((entry) => entry.id === id)
            const window = choice?.value.window as { start?: string; end?: string } | undefined
            if (window) patch({ start: window.start?.slice(0, 10) ?? "", end: window.end?.slice(0, 10) ?? "" })
            else patch({ start: "", end: "" })
          }}>
          {chosen.window === "custom" && (
            <div className="grid grid-cols-2 gap-2 rounded-lg border bg-card p-2">
              <Input aria-label="From" type="date" value={draft.start} onChange={(event) => patch({ start: event.target.value })} />
              <Input aria-label="Until" type="date" value={draft.end} onChange={(event) => patch({ end: event.target.value })} />
            </div>
          )}
        </ChoiceColumn>
      </div>
      <PathSummary numbers={numbers}><span className="text-meta text-muted-foreground">{tree.basis}</span></PathSummary>
      {failure && <p role="alert" className="text-destructive">{failure}</p>}
      {checked && !admitted && <ValidationPanel issues={issues} title="The checker refused this question" details={<div className="space-y-1 font-mono break-all">{checked.verdict.findings.map(({ code }, index) => <div key={`${code}:${index}`}>{code}</div>)}</div>} />}
      {restated && (
        <Panel icon={ShieldCheck} label="In plain words" action={<span className="font-mono text-xs text-muted-foreground" title={restated.sha256}>{restated.sha256.slice(0, 12)}</span>}>
          <div className="space-y-3 p-1">
            <p className="text-base" data-testid="restatement">{restated.text}</p>
            <p className="text-meta text-muted-foreground">Written from the question itself by the server, not by the assistant. Check that it says what you mean.</p>
            <div className="flex flex-wrap items-center gap-3">
              {signed
                ? <span className="flex items-center gap-2 text-sm font-medium"><BadgeCheck className="size-4 text-success" />Signed off: this is what you asked for</span>
                : <Button type="button" size="sm" disabled={!admitted} title={admitted ? undefined : "The checker must admit the question first"} onClick={() => restated && setSignOff({ sha256: restated.sha256, atMs: Date.now() })}>This is what I asked for</Button>}
              {!admitted && <span className="text-meta text-muted-foreground">Sign-off opens once the checker admits the question.</span>}
            </div>
          </div>
        </Panel>
      )}
      {signed ? <RunQuestion key={restated?.sha256} question={question} catalog={catalog} /> : <p className="text-meta text-muted-foreground">Run opens once you have signed off on the question in plain words.</p>}
    </div>
  )
}

export function QuestionTreePage() {
  const catalog = useQuery({ queryKey: ["question-catalog"], queryFn: readCatalog, retry: false, staleTime: Infinity })
  const tree = useQuery({ queryKey: ["question-tree"], queryFn: readRecommendations, retry: false, staleTime: 60_000 })
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-medium">Build a question</h1>
        <p className="max-w-2xl text-muted-foreground">Choose left to right: what to measure, how to group it, which records count and over which dates. The server recommends a pick in each column from what the cell declares, checks every change and says the question back in plain words.</p>
      </div>
      {(catalog.isPending || tree.isPending) && <p className="text-muted-foreground">Reading the catalogue…</p>}
      {catalog.isError && <EmptyState icon={ChartLine}>{catalog.error.message}</EmptyState>}
      {tree.isError && <EmptyState icon={ChartLine}>{tree.error.message}</EmptyState>}
      {catalog.data && tree.data && (tree.data.measures.length === 0
        ? <EmptyState icon={ChartLine} title="Nothing to measure yet">This world declares no measures.</EmptyState>
        : <QuestionTree tree={tree.data} catalog={catalog.data} guard={(dirty) => <DirtyGuard dirty={dirty} />} />)}
    </section>
  )
}
