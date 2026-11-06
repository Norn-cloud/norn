import { useMutation, useQuery } from "@tanstack/react-query"
import { ChartLine, Plus, X } from "lucide-react"
import { useState, type ReactNode } from "react"
import { EmptyState, Panel } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { QuestionCard, questionVerdictOf, type QuestionVerdict } from "./agent/question.tsx"

/** What a question can be built from, as the Worker serves it (`GET /api/questions/catalog`). */
export interface QuestionCatalog {
  readonly objectSet: string | null
  readonly properties: readonly { readonly name: string; readonly kind: string; readonly unit: string | null }[]
  readonly metrics: readonly { readonly name: string; readonly label: string; readonly type: string }[]
  readonly grains: readonly string[]
  readonly operators: readonly string[]
  /** The cell's admitted question: its IR version, calendar and freshness are reused as they are. */
  readonly example: { readonly irVersion: string; readonly calendar: string; readonly freshness: unknown }
}

export interface Draft {
  readonly title: string
  readonly metric: string
  readonly by: string
  readonly grain: string
  readonly filters: readonly { readonly property: string; readonly operator: string; readonly value: string }[]
  readonly start: string
  readonly end: string
}

export const OPERATOR_WORDS: Record<string, string> = { eq: "is", ne: "is not", lt: "is below", le: "is at most", gt: "is above", ge: "is at least" }
const words = (name: string) => name.replaceAll("_", " ")
const slug = (title: string) => title.trim().toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "") || "untitled"
const NUMERIC = new Set(["decimal", "integer"])

/**
 * The QuestionIR for a draft: exactly what was picked, nothing judged. Whether
 * it is a question is the server checker's call, so nothing here refuses.
 */
export function buildQuestion(draft: Draft, catalog: QuestionCatalog) {
  const kind = (property: string) => catalog.properties.find(({ name }) => name === property)?.kind
  const value = (property: string, text: string) => NUMERIC.has(kind(property) ?? "") && text.trim() !== "" && Number.isFinite(Number(text)) ? Number(text) : text
  return {
    irVersion: catalog.example.irVersion,
    name: `studio.${slug(draft.title)}`,
    objectSet: catalog.objectSet,
    aggregate: [{ metric: draft.metric }],
    groupBy: draft.by === "" ? [] : [{ path: { property: draft.by }, ...kind(draft.by) === "timestamp" && draft.grain !== "" ? { grain: draft.grain } : {} }],
    filter: draft.filters.map(({ property, operator, value: text }) => ({ path: { property }, operator, value: value(property, text) })),
    window: { start: `${draft.start}T00:00:00Z`, end: `${draft.end}T00:00:00Z` },
    calendar: catalog.example.calendar,
    freshness: catalog.example.freshness,
  }
}

async function readCatalog(): Promise<QuestionCatalog> {
  const response = await fetch("/api/questions/catalog", { headers: { accept: "application/json" } })
  if (!response.ok) throw new Error(response.status === 401 ? "Sign in to build questions." : "The question catalogue is not available on this deployment.")
  return await response.json() as QuestionCatalog
}

async function checkQuestion(question: unknown): Promise<QuestionVerdict> {
  const response = await fetch("/api/questions/check", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ question }) })
  const verdict = questionVerdictOf(await response.json().catch(() => null))
  if (!response.ok || verdict === undefined) throw new Error("The checker could not be reached.")
  return verdict
}

const fieldClass = "h-9 w-full rounded-3xl border border-transparent bg-input/50 px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30"

function Field({ label, children }: { readonly label: string; readonly children: ReactNode }) {
  return <label className="grid gap-1.5 text-sm"><span className="text-muted-foreground">{label}</span>{children}</label>
}

function Choice({ label, value, onChange, options }: { readonly label: string; readonly value: string; readonly onChange: (value: string) => void; readonly options: readonly (readonly [string, string])[] }) {
  return (
    <Field label={label}>
      <select className={fieldClass} value={value} onChange={(event) => onChange(event.target.value)}>
        {options.map(([option, text]) => <option key={option} value={option}>{text}</option>)}
      </select>
    </Field>
  )
}

export function initialDraft(catalog: QuestionCatalog): Draft {
  const time = catalog.properties.find(({ kind }) => kind === "timestamp")?.name ?? ""
  return { title: "", metric: catalog.metrics[0]?.name ?? "", by: time, grain: catalog.grains.includes("week") ? "week" : catalog.grains[0] ?? "", filters: [], start: "", end: "" }
}

export function QuestionBuilder({ catalog }: { readonly catalog: QuestionCatalog }) {
  const [draft, setDraft] = useState<Draft>(() => initialDraft(catalog))
  const check = useMutation({ mutationFn: () => checkQuestion(buildQuestion(draft, catalog)) })
  const set = (patch: Partial<Draft>) => setDraft((current) => ({ ...current, ...patch }))
  const setFilter = (index: number, patch: Partial<Draft["filters"][number]>) => set({ filters: draft.filters.map((filter, at) => at === index ? { ...filter, ...patch } : filter) })
  const property = catalog.properties.find(({ name }) => name === draft.by)
  const propertyOptions = catalog.properties.map(({ name }) => [name, words(name)] as const)
  return (
    <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
      <Panel icon={ChartLine} label="Question">
        <form className="grid gap-4" onSubmit={(event) => { event.preventDefault(); check.mutate() }}>
          <Field label="Name"><Input value={draft.title} placeholder="e.g. Gold yield by week" onChange={(event) => set({ title: event.target.value })} /></Field>
          <Choice label="Measure" value={draft.metric} onChange={(metric) => set({ metric })} options={catalog.metrics.map(({ name, label }) => [name, label] as const)} />
          <div className="grid grid-cols-2 gap-3">
            <Choice label="By" value={draft.by} onChange={(by) => set({ by })} options={[["", "Nothing (one total)"], ...propertyOptions]} />
            {property?.kind === "timestamp" && <Choice label="Every" value={draft.grain} onChange={(grain) => set({ grain })} options={catalog.grains.map((grain) => [grain, grain[0]?.toUpperCase() + grain.slice(1)] as const)} />}
          </div>
          <div className="grid gap-2">
            <span className="text-sm text-muted-foreground">Only where</span>
            {draft.filters.length === 0 && <p className="text-sm text-muted-foreground">Every {words(catalog.objectSet?.split(".").pop() ?? "record")} record.</p>}
            {draft.filters.map((filter, index) => (
              <div key={index} className="grid grid-cols-[1fr_1fr_1fr_auto] items-end gap-2">
                <Choice label="Field" value={filter.property} onChange={(value) => setFilter(index, { property: value })} options={propertyOptions} />
                <Choice label="Condition" value={filter.operator} onChange={(value) => setFilter(index, { operator: value })} options={catalog.operators.map((operator) => [operator, OPERATOR_WORDS[operator] ?? operator] as const)} />
                <Field label="Value"><Input value={filter.value} onChange={(event) => setFilter(index, { value: event.target.value })} /></Field>
                <Button type="button" variant="ghost" size="icon" aria-label="Remove condition" onClick={() => set({ filters: draft.filters.filter((_, at) => at !== index) })}><X /></Button>
              </div>
            ))}
            <Button type="button" variant="outline" size="sm" className="justify-self-start" onClick={() => set({ filters: [...draft.filters, { property: catalog.properties.find(({ kind }) => kind === "text")?.name ?? catalog.properties[0]?.name ?? "", operator: catalog.operators[0] ?? "eq", value: "" }] })}><Plus />Add condition</Button>
          </div>
          <div className="grid grid-cols-2 gap-3">
            <Field label="From"><Input type="date" value={draft.start} onChange={(event) => set({ start: event.target.value })} /></Field>
            <Field label="Until"><Input type="date" value={draft.end} onChange={(event) => set({ end: event.target.value })} /></Field>
          </div>
          <Button type="submit" className="justify-self-start" disabled={check.isPending}>{check.isPending ? "Checking…" : "Check with the server"}</Button>
        </form>
      </Panel>
      <div className="space-y-3">
        {check.isIdle && <EmptyState icon={ChartLine}>The server's checker admits the question or names what is wrong. Nothing is run yet.</EmptyState>}
        {check.isError && <p className="text-destructive">{check.error.message}</p>}
        {check.data && <QuestionCard verdict={check.data} />}
      </div>
    </div>
  )
}

export function QuestionBuilderPage() {
  const catalog = useQuery({ queryKey: ["question-catalog"], queryFn: readCatalog, retry: false, staleTime: Infinity })
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-medium">New question</h1>
        <p className="max-w-2xl text-muted-foreground">Pick what to measure, how to group it, which records count and over which dates. The server checks it before anything is run.</p>
      </div>
      {catalog.isPending && <p className="text-muted-foreground">Reading the catalogue…</p>}
      {catalog.isError && <EmptyState icon={ChartLine}>{catalog.error.message}</EmptyState>}
      {catalog.data && <QuestionBuilder catalog={catalog.data} />}
    </section>
  )
}
