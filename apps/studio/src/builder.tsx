import { useMutation, useQuery } from "@tanstack/react-query"
import { ChartLine, CircleCheck, Play, Plus } from "lucide-react"
import { useEffect, useState, type ReactNode } from "react"
import { DirtyGuard, EditorToolbar, RowActions, ValidationPanel, useEditor } from "@/components/editor"
import { LoadState } from "@/components/load-state"
import { EmptyState, Panel } from "@/components/system/panel"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { CellUnavailable, defaultFetch, fetchWithTimeout, getDisplayedScope, parseUnavailableDetail, scopeHeaders, useStudioScope, type StudioScope } from "@/lib/definitions"
import { list, modKey, sameValue, type Issue } from "@/lib/editor"
import { questionVerdictOf, type QuestionVerdict } from "./agent/question.tsx"
import { usePinnedScope } from "./lib/session.ts"
import { readDocument } from "./lib/exact.ts"
import { readMetrics, recentWindow } from "./lib/metrics.ts"
import type { UnitGroup } from "./lib/answer-anatomy.ts"
import type { Question } from "./lib/transport.ts"
import { UnitGroups } from "./questions.tsx"
import { RunQuestion } from "./run.tsx"

/** What a question can be built from, as the Worker serves it (`GET /api/questions/catalog`). */
export interface QuestionCatalog {
  readonly objectSet: string | null
  readonly properties: readonly { readonly name: string; readonly kind: string; readonly unit: string | null }[]
  readonly metrics: readonly { readonly name: string; readonly label: string; readonly display?: string; readonly type: string }[]
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
const sentence = (text: string) => text.charAt(0).toUpperCase() + text.slice(1)
/** A measure as a person names it: the server's display words (the checker derives them), else its label. */
export const measureText = (metric: { readonly label: string; readonly display?: string }) => sentence(metric.display ?? metric.label)
/** A property in words: "observed_at" reads "Observed at". */
export const fieldText = (name: string) => sentence(words(name))
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

export async function readCatalog(boundScope?: StudioScope | null): Promise<QuestionCatalog> {
  const scope = boundScope !== undefined ? boundScope : getDisplayedScope()
  const response = await fetchWithTimeout(defaultFetch, "/api/questions/catalog", { headers: { accept: "application/json", ...scopeHeaders(scope) } })
  if (!response.ok) {
    if (response.status === 401) throw new Error("Sign in to build questions.")
    if (response.status >= 500) {
      const body = await response.json().catch(() => null)
      const message = typeof (body as { message?: unknown } | null)?.message === "string" ? (body as { message: string }).message : undefined
      throw new CellUnavailable(response.status, message, parseUnavailableDetail(body))
    }
    throw new Error(UNAVAILABLE)
  }
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

export async function postQuestionCheck(question: unknown, boundScope?: StudioScope | null): Promise<{ readonly verdict: QuestionVerdict; readonly body: unknown }> {
  const scope = boundScope !== undefined ? boundScope : getDisplayedScope()
  const response = await fetch("/api/questions/check", {
    method: "POST",
    headers: { "content-type": "application/json", ...scopeHeaders(scope) },
    body: JSON.stringify({ question }),
  })
  const body = await response.json().catch(() => null)
  const verdict = questionVerdictOf(body)
  if (!response.ok || verdict === undefined) throw new Error("The checker could not be reached.")
  return { verdict, body }
}

async function checkQuestion(question: unknown, boundScope?: StudioScope | null): Promise<QuestionVerdict> {
  return (await postQuestionCheck(question, boundScope)).verdict
}

const fieldClass = "h-9 w-full rounded-3xl border border-transparent bg-input/50 px-3 text-body outline-none focus-visible:border-ring focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring aria-invalid:border-destructive"

function Field({ label, field, invalid, children }: { readonly label: string; readonly field?: string; readonly invalid?: boolean; readonly children: ReactNode }) {
  return <label className="grid gap-1.5 text-body" data-field={field} data-invalid={invalid || undefined}><span className={invalid ? "text-destructive" : "text-muted-foreground"}>{label}</span>{children}</label>
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

/**
 * A new question: the first measure, by week of the first date, over the latest 52 whole weeks
 * (Monday to Monday), so the first Run answers without a date to fill in.
 */
export function initialDraft(catalog: QuestionCatalog, now = Date.now()): Draft {
  const time = catalog.properties.find(({ kind }) => kind === "timestamp")?.name ?? ""
  const window = recentWindow(now)
  return { title: "", measures: catalog.metrics[0] ? [catalog.metrics[0].name] : [], by: time, grain: catalog.grains.includes("week") ? "week" : catalog.grains[0] ?? "", filters: [], start: window.start.slice(0, 10), end: window.end.slice(0, 10) }
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

export function QuestionBuilder({ catalog, guard, initial, onUse, unsupported, checkerServed, originScope }: {
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
  /** Originating workspace scope so checking, evaluation, and trace stay pinned to this workspace. */
  readonly originScope?: StudioScope | null
}) {
  const probed = useCheckerServed()
  const served = checkerServed ?? probed
  const fallbackPinnedScope = usePinnedScope(catalog.objectSet)
  const pinnedScope = originScope !== undefined ? originScope : fallbackPinnedScope
  const check = useMutation({ mutationFn: (draft: Draft) => checkQuestion(buildQuestion(draft, catalog), pinnedScope) })
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
  const propertyOptions = catalog.properties.map(({ name }) => [name, fieldText(name)] as const)
  const metricOptions = catalog.metrics.map((metric) => [metric.name, measureText(metric)] as const)
  const record = words(catalog.objectSet?.split(".").pop() ?? "record").toLowerCase()
  // What the answer measures, in words: the one measure's display words, or the name given.
  const titleOf = (asked: Draft) => asked.title.trim() || (asked.measures.length === 1 ? metricOptions.find(([name]) => name === asked.measures[0])?.[1] : undefined) || "Answer"
  if (catalog.metrics.length === 0) return <EmptyState icon={ChartLine}>This world declares no measures yet, so there is nothing to build a question from.</EmptyState>
  return (
    <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
      {guard?.(editor.dirty)}
      <Panel icon={ChartLine} label="Question" action={<EditorToolbar editor={editor} />}>
        <form className="grid gap-4" onSubmit={(event) => { event.preventDefault(); run(draft) }}>
          {served === false && <p role="status" className="rounded-md border border-destructive/30 bg-destructive/5 p-3 text-body">{CHECKER_UNSERVED}</p>}
          <fieldset disabled={served === false} className="contents">
          <Field label="Name" field="title" invalid={invalid("title")}><Input value={draft.title} placeholder="e.g. Records by week" onChange={(event) => patch({ title: event.target.value })} /></Field>
          <div className="grid gap-2" data-field="measures">
            <span className={invalid("measures") ? "text-body text-destructive" : "text-body text-muted-foreground"}>Measures</span>
            {draft.measures.length === 0 && <p className="text-body text-muted-foreground">No measure yet.</p>}
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
            <span className={invalid("filters") ? "text-body text-destructive" : "text-body text-muted-foreground"}>Only where</span>
            {draft.filters.length === 0 && <p className="text-body text-muted-foreground">No conditions: every {record} counts.</p>}
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
          <Button type="submit" className="justify-self-start" title={`Run (${modKey()}Enter): the server checks the question, then answers it`} disabled={check.isPending || served === false}><Play />{check.isPending ? "Checking…" : "Run"}</Button>
          </fieldset>
        </form>
      </Panel>
      <div className="space-y-3">
        {unsupported && <p role="status" className="rounded-md border border-destructive/30 bg-destructive/5 p-3 text-body">{unsupported}</p>}
        {checked.data === undefined && checked.error === null && !(check.isPending && check.variables === draft) && <EmptyState icon={ChartLine} title="Pick a measure, then Run">The server checks the question, then answers it. Every number opens to the records behind it.</EmptyState>}
        {checked.error && <p role="alert" className="text-destructive">{checked.error.message}</p>}
        {/* One place for a refusal: the panel links each finding to its field; its codes are under Details. */}
        {checked.data && !checked.data.admitted && <ValidationPanel issues={issues} title="The checker refused this question" details={<div className="space-y-1 font-mono break-all">{checked.data.findings.map(({ code }, index) => <div key={`${code}:${index}`}>{code}</div>)}</div>} />}
        {checked.data?.admitted && <Checked verdict={checked.data} />}
        {/* The question exactly as the checker admitted it, answered at once, while the form still shows it. */}
        {checked.data?.admitted && check.variables && <BuiltAnswer key={JSON.stringify(check.variables)} question={buildQuestion(check.variables, catalog)} catalog={catalog} title={titleOf(check.variables)} scope={pinnedScope} />}
        {onUse && checked.data?.admitted && check.variables && <Button type="button" disabled={unsupported !== undefined} onClick={() => onUse(buildQuestion(check.variables as Draft, catalog), (check.variables as Draft).title)}>Use this question</Button>}
        {onUse && check.data?.admitted && !checked.data && <p className="text-body text-muted-foreground">Changed since it ran: Run again to use it.</p>}
      </div>
    </div>
  )
}

/** The checker's admission, quietly: its own restatement is one click away, verbatim. */
function Checked({ verdict }: { readonly verdict: QuestionVerdict }) {
  const canonical = verdict.restatement?.text
  return (
    <details className="group text-meta text-muted-foreground">
      <summary className="flex cursor-pointer list-none items-center gap-1.5"><CircleCheck aria-hidden className="size-3.5 text-success" />Checked by the server<span className="underline-offset-4 group-open:hidden hover:underline">· what it read</span></summary>
      <p className="mt-1.5 max-w-[72ch] text-pretty">{canonical ?? "The checker admitted this question."}</p>
    </details>
  )
}

/**
 * An admitted question, answered. A measure whose values come in several units (the cell names
 * them in the metric's unitPartition) is read one unit group at a time, as the Metrics board
 * reads it, so a chart never mixes grams with pieces; any other question is answered as asked.
 */
function BuiltAnswer({ question, catalog, title, scope }: { readonly question: QuestionIR; readonly catalog: QuestionCatalog; readonly title: string; readonly scope?: StudioScope | null }) {
  const scopeKey = scope ? `${scope.orgId}:${scope.environmentId}` : ""
  const metrics = useQuery({ queryKey: ["metrics", scopeKey], queryFn: () => readMetrics(scope), retry: false, staleTime: 60_000 })
  const [picked, setPicked] = useState<UnitGroup | null>(null)
  if (metrics.isPending) return <LoadState status="pending" skeleton="panel" loadingLabel="Reading metric definitions" onRetry={() => void metrics.refetch()}>{() => null}</LoadState>
  const partitions = question.aggregate.map(({ metric }) => metrics.data?.document.metrics.find(({ name }) => name === metric)?.unitPartition ?? [])
  const sources = partitions[0] ?? []
  const shared = partitions.every((partition) => partition.length === sources.length && partition.every((property, index) => property === sources[index]))
  if (sources.length === 0 || !shared) return <RunQuestion question={question} catalog={catalog} title={title} scope={scope} auto />
  const declared = { metrics: readDocument(JSON.stringify(catalog.declarations.metrics), "live"), objectType: readDocument(JSON.stringify(catalog.declarations.objectType), "live") } as Parameters<typeof UnitGroups>[0]["declared"]
  return <UnitGroups asked={question as unknown as Question} grain={question.groupBy[0]?.grain ?? "group"} sources={sources} title={title} declared={declared} picked={picked} onPick={setPicked} onSettled={settled} scope={scope} />
}
const settled = () => {}

export function QuestionBuilderPage() {
  const scope = useStudioScope()
  const scopeKey = scope.data ? `${scope.data.orgId}:${scope.data.environmentId}` : ""
  const catalog = useQuery({ queryKey: ["question-catalog", scope.data?.orgId, scope.data?.environmentId], queryFn: () => readCatalog(scope.data), enabled: !scope.isPending, retry: false, staleTime: Infinity })
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-medium tracking-tight">Explore a metric</h1>
        <p className="max-w-2xl text-muted-foreground">Pick what to measure, how to group it, which records count and over which dates, then Run. The server checks the question before it answers.</p>
      </div>
      <LoadState
        status={catalog.status}
        data={catalog.data}
        error={catalog.error}
        isFetching={catalog.isFetching}
        onRetry={() => void catalog.refetch()}
        skeleton="detail"
        loadingLabel="Reading metric catalogue"
      >
        {(data) => <QuestionBuilder key={scopeKey} catalog={data} originScope={scope.data} guard={(dirty) => <DirtyGuard dirty={dirty} />} />}
      </LoadState>
    </section>
  )
}
