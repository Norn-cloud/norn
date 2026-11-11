import { useMutation, useQuery } from "@tanstack/react-query"
import { ChartLine, Plus } from "lucide-react"
import { useEffect, useState, type ReactNode } from "react"
import { DirtyGuard, EditorToolbar, RowActions, ValidationPanel, useEditor } from "@/components/editor"
import { EmptyState, Panel } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { list, modKey, sameValue, type Issue } from "@/lib/editor"
import { QuestionCard, questionVerdictOf, type QuestionVerdict } from "./agent/question.tsx"
import { RunQuestion } from "./run.tsx"

/** What a question can be built from, as the Worker serves it (`GET /api/questions/catalog`). */
export interface QuestionCatalog {
  readonly objectSet: string | null
  readonly properties: readonly { readonly name: string; readonly kind: string; readonly unit: string | null }[]
  readonly metrics: readonly { readonly name: string; readonly label: string; readonly type: string }[]
  readonly grains: readonly string[]
  readonly operators: readonly string[]
  /** The cell's admitted question: its IR version, calendar and freshness are reused as they are. */
  readonly example: { readonly irVersion: string; readonly calendar: string; readonly freshness: unknown }
  /** The declarations an answer to a built question is read with (units, ratios, labels). */
  readonly declarations: { readonly metrics: readonly unknown[]; readonly objectType: unknown }
}

export interface Condition { readonly property: string; readonly operator: string; readonly value: string }
export interface Draft {
  readonly title: string
  readonly measures: readonly string[]
  readonly by: string
  readonly grain: string
  readonly filters: readonly Condition[]
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
    name: /^studio\.[a-z0-9_]+$/.test(draft.title) ? draft.title : `studio.${slug(draft.title)}`,
    objectSet: catalog.objectSet,
    aggregate: draft.measures.map((metric) => ({ metric })),
    groupBy: draft.by === "" ? [] : [{ path: { property: draft.by }, ...kind(draft.by) === "timestamp" && draft.grain !== "" ? { grain: draft.grain } : {} }],
    filter: draft.filters.map(({ property, operator, value: text }) => ({ path: { property }, operator, value: value(property, text) })),
    window: { start: `${draft.start}T00:00:00Z`, end: `${draft.end}T00:00:00Z` },
    calendar: catalog.example.calendar,
    freshness: catalog.example.freshness,
  }
}

/**
 * Where a checker finding belongs in the form, from its JSON pointer into the
 * checked document (`/questions/0/...`). Both the wire names (aggregate,
 * groupBy, filter) and the checker's own (metrics, dimensions, filters) place.
 */
export function fieldOf(pointer: string | null): string | null {
  const parts = (pointer ?? "").split("/").slice(1)
  // The checker's pointers start at the checked document (/questions/0/...), the cell's evaluate route at /question.
  const [section, index] = parts[0] === "questions" ? parts.slice(2) : parts[0] === "question" ? parts.slice(1) : parts
  if (section === "aggregate" || section === "metrics") return index === undefined ? "measures" : `measure.${index}`
  if (section === "groupBy" || section === "dimensions") return "by"
  if (section === "filter" || section === "filters") return index === undefined ? "filters" : `filter.${index}`
  if (section === "window") return "window"
  if (section === "name") return "title"
  return null
}

export const issuesOf = (verdict: QuestionVerdict | undefined): readonly Issue[] =>
  verdict === undefined || verdict.admitted ? [] : verdict.findings.map(({ code, message, pointer }) => ({ code, message, field: fieldOf(pointer ?? null) }))

const isObject = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value)
const isStrings = (value: unknown): boolean => Array.isArray(value) && value.every((item) => typeof item === "string")

/** The response as a catalogue, if it has every field the builder reads; anything else is no catalogue. */
export function catalogOf(value: unknown): QuestionCatalog | undefined {
  if (!isObject(value) || !(value.objectSet === null || typeof value.objectSet === "string") || !isObject(value.example) || typeof value.example.irVersion !== "string" || typeof value.example.calendar !== "string") return undefined
  const { properties, metrics, grains, operators } = value
  if (!Array.isArray(properties) || !properties.every((p) => isObject(p) && typeof p.name === "string" && typeof p.kind === "string" && (p.unit === null || typeof p.unit === "string"))) return undefined
  if (!Array.isArray(metrics) || !metrics.every((m) => isObject(m) && typeof m.name === "string" && typeof m.label === "string" && typeof m.type === "string")) return undefined
  return isStrings(grains) && isStrings(operators) ? value as unknown as QuestionCatalog : undefined
}

const UNAVAILABLE = "The question catalogue is not available on this deployment."

export async function readCatalog(): Promise<QuestionCatalog> {
  const response = await fetch("/api/questions/catalog", { headers: { accept: "application/json" } })
  if (!response.ok) throw new Error(response.status === 401 ? "Sign in to build questions." : UNAVAILABLE)
  const catalog = catalogOf(await response.json().catch(() => undefined))
  if (catalog === undefined) throw new Error(UNAVAILABLE)
  return catalog
}

/**
 * A check's result belongs to the draft and editor revision it was run on:
 * after any edit, undo or redo it is shown no longer (undo back to the same
 * draft is a new revision), and a late response for an older draft is ignored.
 */
export const checkedFor = <D, R>(check: { readonly variables: D | undefined; readonly data: R | undefined; readonly error: Error | null }, draft: D, at?: { readonly ranAt: object | undefined; readonly revision: object }) =>
  check.variables === draft && (at === undefined || at.ranAt === at.revision) ? { data: check.data, error: check.error } : { data: undefined, error: null }

async function checkQuestion(question: unknown): Promise<QuestionVerdict> {
  const response = await fetch("/api/questions/check", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ question }) })
  const verdict = questionVerdictOf(await response.json().catch(() => null))
  if (!response.ok || verdict === undefined) throw new Error("The checker could not be reached.")
  return verdict
}

const fieldClass = "h-9 w-full rounded-3xl border border-transparent bg-input/50 px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30 aria-invalid:border-destructive"

function Field({ label, field, invalid, children }: { readonly label: string; readonly field?: string; readonly invalid?: boolean; readonly children: ReactNode }) {
  return <label className="grid gap-1.5 text-sm" data-field={field} data-invalid={invalid || undefined}><span className={invalid ? "text-destructive" : "text-muted-foreground"}>{label}</span>{children}</label>
}

function Choice({ label, field, invalid, value, onChange, options }: { readonly label: string; readonly field?: string; readonly invalid?: boolean; readonly value: string; readonly onChange: (value: string) => void; readonly options: readonly (readonly [string, string])[] }) {
  return (
    <Field label={label} field={field} invalid={invalid}>
      <select className={fieldClass} aria-invalid={invalid || undefined} value={value} onChange={(event) => onChange(event.target.value)}>
        {options.map(([option, text]) => <option key={option} value={option}>{text}</option>)}
      </select>
    </Field>
  )
}

type QuestionIR = ReturnType<typeof buildQuestion>

/** The draft a stored QuestionIR reopens as, so editing it loses nothing the builder can write. */
/** Whether a stored question's window is whole UTC days, the only windows a draft can hold without changing them. */
export const windowFitsDraft = (question: Partial<QuestionIR>): boolean =>
  [question.window?.start, question.window?.end].every((iso) => iso === undefined || (typeof iso === "string" && iso === `${iso.slice(0, 10)}T00:00:00Z`))

/**
 * Whether a stored question survives a trip through the form unchanged: its
 * window is whole days and the question the draft rebuilds is the same one,
 * grouping, freshness and all (its name aside, which follows the title).
 */
export function questionFitsDraft(question: Partial<QuestionIR> & { readonly name?: string }, catalog: QuestionCatalog): boolean {
  if (!windowFitsDraft(question)) return false
  const { name: _built, ...rebuilt } = buildQuestion(draftOf(question), catalog)
  const { name: _stored, ...stored } = question
  return sameValue(JSON.parse(JSON.stringify(rebuilt)), JSON.parse(JSON.stringify(stored)))
}

export const UNEDITABLE = "This question cannot be edited without changing it."

export function draftOf(question: Partial<QuestionIR> & { readonly name?: string }, title?: string): Draft {
  const day = (iso: unknown) => typeof iso === "string" ? iso.slice(0, 10) : ""
  const [by] = question.groupBy ?? []
  return {
    title: title ?? (question.name ?? "").replace(/^studio\./, "").replaceAll("_", " "),
    measures: (question.aggregate ?? []).map(({ metric }) => metric),
    by: by?.path.property ?? "",
    grain: by && "grain" in by && typeof by.grain === "string" ? by.grain : "week",
    filters: (question.filter ?? []).map(({ path, operator, value }) => ({ property: path.property, operator, value: String(value) })),
    start: day(question.window?.start),
    end: day(question.window?.end),
  }
}

export function initialDraft(catalog: QuestionCatalog): Draft {
  const time = catalog.properties.find(({ kind }) => kind === "timestamp")?.name ?? ""
  return { title: "", measures: catalog.metrics[0] ? [catalog.metrics[0].name] : [], by: time, grain: catalog.grains.includes("week") ? "week" : catalog.grains[0] ?? "", filters: [], start: "", end: "" }
}

/** The checker route is missing (404), has no host bound (503), or cannot be reached: nothing can be checked here. */
export const checkerServedBy = (status: number | "unreachable"): boolean => status !== "unreachable" && status !== 404 && status !== 503

export const CHECKER_UNSERVED = "The question checker isn't served here."

// One probe per page: every builder (the page, each dashboard tile sheet) shares its answer.
let checkerProbe: Promise<boolean> | undefined
const probeChecker = () => checkerProbe ??= fetch("/api/questions/check", { method: "GET", headers: { accept: "application/json" } })
  .then((response) => checkerServedBy(response.status), () => checkerServedBy("unreachable"))

/** Whether this deployment serves the question checker: unknown until the one probe answers. */
function useCheckerServed(): boolean | undefined {
  const [served, setServed] = useState<boolean>()
  useEffect(() => {
    let current = true
    void probeChecker().then((answer) => { if (current) setServed(answer) })
    return () => { current = false }
  }, [])
  return served
}

export function QuestionBuilder({ catalog, guard, initial, onUse, unsupported, checkerServed }: {
  readonly catalog: QuestionCatalog
  readonly guard?: (dirty: boolean) => ReactNode
  /** The draft to open; a new question otherwise. */
  readonly initial?: Draft
  /** Offered once the checker admits the question: hands the admitted QuestionIR to the caller (e.g. a dashboard tile). */
  readonly onUse?: (question: QuestionIR, title: string) => void
  /** Why the opened question cannot be used again from this form (its window is finer than days): "Use this question" stays off. */
  readonly unsupported?: string
  /** Overrides the probe (tests, or a caller that already knows). */
  readonly checkerServed?: boolean
}) {
  const probed = useCheckerServed()
  const served = checkerServed ?? probed
  const check = useMutation({ mutationFn: (draft: Draft) => checkQuestion(buildQuestion(draft, catalog)) })
  // The editor revision each check ran at: a check shows only while the editor is still at that revision.
  const [ranAt, setRanAt] = useState<object>()
  const editor = useEditor<Draft>({ initial: initial ?? initialDraft(catalog), onRun: (draft) => run(draft) })
  const run = (next: Draft) => { if (served === false) return; setRanAt(editor.revision); check.mutate(next) }
  const { value: draft, set } = editor
  const patch = (next: Partial<Draft>) => set((current) => ({ ...current, ...next }))
  const setFilter = (index: number, next: Partial<Condition>) => patch({ filters: draft.filters.map((filter, at) => at === index ? { ...filter, ...next } : filter) })
  const checked = checkedFor(check, draft, { ranAt, revision: editor.revision })
  const issues = issuesOf(checked.data)
  const invalid = (field: string) => issues.some((issue) => issue.field === field)
  const property = catalog.properties.find(({ name }) => name === draft.by)
  const propertyOptions = catalog.properties.map(({ name }) => [name, words(name)] as const)
  const metricOptions = catalog.metrics.map(({ name, label }) => [name, label] as const)
  const record = words(catalog.objectSet?.split(".").pop() ?? "record")
  if (catalog.metrics.length === 0) return <EmptyState icon={ChartLine}>This world declares no measures yet, so there is nothing to build a question from.</EmptyState>
  return (
    <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
      {guard?.(editor.dirty)}
      <Panel icon={ChartLine} label="Question" action={<EditorToolbar editor={editor} />}>
        <form className="grid gap-4" onSubmit={(event) => { event.preventDefault(); run(draft) }}>
          {served === false && <p role="status" className="rounded-md border border-destructive/30 bg-destructive/5 p-3 text-sm">{CHECKER_UNSERVED}</p>}
          <fieldset disabled={served === false} className="contents">
          <Field label="Name" field="title" invalid={invalid("title")}><Input value={draft.title} placeholder="e.g. Gold yield by week" onChange={(event) => patch({ title: event.target.value })} /></Field>
          <div className="grid gap-2" data-field="measures">
            <span className={invalid("measures") ? "text-sm text-destructive" : "text-sm text-muted-foreground"}>Measures</span>
            {draft.measures.length === 0 && <p className="text-sm text-muted-foreground">No measure yet.</p>}
            {draft.measures.map((metric, index) => (
              <div key={index} className="grid grid-cols-[1fr_auto] items-end gap-2">
                <Choice label={`Measure ${index + 1}`} field={`measure.${index}`} invalid={invalid(`measure.${index}`)} value={metric} onChange={(next) => patch({ measures: draft.measures.map((m, at) => at === index ? next : m) })} options={metricOptions} />
                <RowActions index={index} count={draft.measures.length} label={`measure ${index + 1}`} onDuplicate={() => patch({ measures: list.duplicate(draft.measures, index) })} onMove={(to) => patch({ measures: list.move(draft.measures, index, to) })} onRemove={() => patch({ measures: list.remove(draft.measures, index) })} />
              </div>
            ))}
            <Button type="button" variant="outline" size="sm" className="justify-self-start" onClick={() => patch({ measures: [...draft.measures, catalog.metrics[0]?.name ?? ""] })}><Plus />Add measure</Button>
          </div>
          <div className="grid grid-cols-2 gap-3">
            <Choice label="By" field="by" invalid={invalid("by")} value={draft.by} onChange={(by) => patch({ by })} options={[["", "Nothing (one total)"], ...propertyOptions]} />
            {property?.kind === "timestamp" && <Choice label="Every" field="grain" invalid={invalid("by")} value={draft.grain} onChange={(grain) => patch({ grain })} options={catalog.grains.map((grain) => [grain, grain[0]?.toUpperCase() + grain.slice(1)] as const)} />}
          </div>
          <div className="grid gap-2" data-field="filters">
            <span className={invalid("filters") ? "text-sm text-destructive" : "text-sm text-muted-foreground"}>Only where</span>
            {draft.filters.length === 0 && <p className="text-sm text-muted-foreground">Every {record} record.</p>}
            {draft.filters.map((filter, index) => (
              <div key={index} className="grid grid-cols-[1fr_1fr_1fr_auto] items-end gap-2" data-field={`filter.${index}`}>
                <Choice label={`Field ${index + 1}`} invalid={invalid(`filter.${index}`)} value={filter.property} onChange={(next) => setFilter(index, { property: next })} options={propertyOptions} />
                <Choice label="Condition" invalid={invalid(`filter.${index}`)} value={filter.operator} onChange={(next) => setFilter(index, { operator: next })} options={catalog.operators.map((operator) => [operator, OPERATOR_WORDS[operator] ?? operator] as const)} />
                <Field label="Value" invalid={invalid(`filter.${index}`)}><Input value={filter.value} aria-invalid={invalid(`filter.${index}`) || undefined} onChange={(event) => setFilter(index, { value: event.target.value })} /></Field>
                <RowActions index={index} count={draft.filters.length} label={`condition ${index + 1}`} onDuplicate={() => patch({ filters: list.duplicate(draft.filters, index) })} onMove={(to) => patch({ filters: list.move(draft.filters, index, to) })} onRemove={() => patch({ filters: list.remove(draft.filters, index) })} />
              </div>
            ))}
            <Button type="button" variant="outline" size="sm" className="justify-self-start" onClick={() => patch({ filters: [...draft.filters, { property: catalog.properties.find(({ kind }) => kind === "text")?.name ?? catalog.properties[0]?.name ?? "", operator: catalog.operators[0] ?? "eq", value: "" }] })}><Plus />Add condition</Button>
          </div>
          <div className="grid grid-cols-2 gap-3" data-field="window">
            <Field label="From" invalid={invalid("window")}><Input type="date" value={draft.start} aria-invalid={invalid("window") || undefined} onChange={(event) => patch({ start: event.target.value })} /></Field>
            <Field label="Until" invalid={invalid("window")}><Input type="date" value={draft.end} aria-invalid={invalid("window") || undefined} onChange={(event) => patch({ end: event.target.value })} /></Field>
          </div>
          <Button type="submit" className="justify-self-start" title={`Check (${modKey()}Enter)`} disabled={check.isPending || served === false}>{check.isPending ? "Checking…" : "Check with the server"}</Button>
          </fieldset>
        </form>
      </Panel>
      <div className="space-y-3">
        {unsupported && <p role="status" className="rounded-md border border-destructive/30 bg-destructive/5 p-3 text-sm">{unsupported}</p>}
        {checked.data === undefined && checked.error === null && !(check.isPending && check.variables === draft) && <EmptyState icon={ChartLine}>The server's checker admits the question or names what is wrong. Nothing is run yet.</EmptyState>}
        {checked.error && <p role="alert" className="text-destructive">{checked.error.message}</p>}
        {/* One place for a refusal: the panel links each finding to its field; its codes are under Details. */}
        {checked.data && !checked.data.admitted && <ValidationPanel issues={issues} title="The checker refused this question" details={<div className="space-y-1 font-mono break-all">{checked.data.findings.map(({ code }, index) => <div key={`${code}:${index}`}>{code}</div>)}</div>} />}
        {checked.data?.admitted && <QuestionCard verdict={checked.data} />}
        {/* Only the question the checker admitted, and only while the form still shows it. */}
        {/* Run: the question exactly as the checker admitted it, while the form still shows it. */}
        {checked.data?.admitted && check.variables && <RunQuestion key={JSON.stringify(check.variables)} question={buildQuestion(check.variables, catalog)} catalog={catalog} />}
        {onUse && checked.data?.admitted && check.variables && <Button type="button" disabled={unsupported !== undefined} onClick={() => onUse(buildQuestion(check.variables as Draft, catalog), (check.variables as Draft).title)}>Use this question</Button>}
        {onUse && check.data?.admitted && !checked.data && <p className="text-sm text-muted-foreground">Changed since the check: check again to use it.</p>}
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
      {catalog.data && <QuestionBuilder catalog={catalog.data} guard={(dirty) => <DirtyGuard dirty={dirty} />} />}
    </section>
  )
}
