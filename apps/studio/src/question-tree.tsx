import { useQuery } from "@tanstack/react-query"
import { BadgeCheck, ChartLine, ShieldCheck } from "lucide-react"
import { useEffect, useRef, useState, type ReactNode } from "react"
import { ChoiceColumn, PathSummary, pathNumbers, type ColumnOption } from "@/components/decision/choice-columns"
import { DirtyGuard, EditorToolbar, ValidationPanel, useEditor } from "@/components/editor"
import { EmptyState, Panel } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { questionVerdictOf, type QuestionVerdict } from "./agent/question.tsx"
import { OPERATOR_WORDS, buildQuestion, draftOf, issuesOf, readCatalog, type Condition, type Draft, type QuestionCatalog } from "./builder.tsx"
import { RunQuestion } from "./run.tsx"
import { questionDefinitions, QuestionDefinitionError } from "./lib/question-definitions.ts"
import { NO_DATA_CELL, useStudioScope } from "./lib/definitions.ts"
import { z } from "zod"
import { useView } from "@/lib/view"

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
/** The server's verdict with the question in plain words (the checker's restatement) and that text's hash; none, with the reason, when it cannot be stated. */
export interface CheckedQuestion { readonly verdict: QuestionVerdict; readonly restatement: { readonly text: string; readonly sha256: string } | null; readonly restatementError?: string }

/** A sign-off: the person read exactly these words (by their hash) and said "This is what I asked for". */
export interface SignOff { readonly sha256: string; readonly atMs: number; readonly signer?: string; readonly displayName?: string }

async function readRecommendations(): Promise<Recommendations> {
  const response = await fetch("/api/questions/recommendations", { headers: { accept: "application/json" } })
  if (!response.ok) throw new Error(response.status === 401 ? "Sign in to build questions." : "The question tree is not available on this deployment.")
  return await response.json() as Recommendations
}

async function checkQuestion(question: unknown): Promise<CheckedQuestion> {
  const response = await fetch("/api/questions/check", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ question }) })
  const body = await response.json().catch(() => null) as { restatement?: CheckedQuestion["restatement"]; restatementError?: unknown } | null
  const verdict = questionVerdictOf(body)
  if (!response.ok || verdict === undefined || body?.restatement === undefined) throw new Error("The checker could not be reached.")
  return { verdict, restatement: body.restatement, ...typeof body.restatementError === "string" ? { restatementError: body.restatementError } : {} }
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
    title: pick(tree.measures)?.label ?? "Question",
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

/**
 * Which option of each column the draft is on; "custom" when it was edited
 * beyond the offered options, or when the reader chose "Another condition"
 * (`customCondition`), even if its filter happens to equal a recommendation.
 */
export function selection(tree: Recommendations, draft: Draft, customCondition = false) {
  const condition = customCondition ? "custom" : tree.conditions.find((choice) => same(conditionsOf(choice), draft.filters))?.id ?? (draft.filters.length > 0 ? "custom" : "every")
  const window = tree.windows.find(({ value }) => {
    const w = value.window as { start?: string; end?: string }
    return w.start?.slice(0, 10) === draft.start && w.end?.slice(0, 10) === draft.end
  })?.id ?? "custom"
  return { measure: draft.measures[0], by: draft.by, grain: draft.grain, condition, window }
}

const savedQuestion = z.object({
  name: z.string(), irVersion: z.string(), objectSet: z.string().nullable(),
  aggregate: z.array(z.object({ metric: z.string() }).strict()),
  groupBy: z.array(z.object({ path: z.object({ property: z.string() }).strict(), grain: z.string().optional() }).strict()).max(1),
  filter: z.array(z.object({ path: z.object({ property: z.string() }).strict(), operator: z.string(), value: z.union([z.string(), z.number()]) }).strict()),
  window: z.object({ start: z.string(), end: z.string() }).strict(), calendar: z.string(), freshness: z.unknown(),
}).strict()
const stable = (value: unknown): string => JSON.stringify(value, (_key, v: unknown) => v !== null && typeof v === "object" && !Array.isArray(v) ? Object.fromEntries(Object.entries(v).sort(([a], [b]) => a.localeCompare(b))) : v)
const definitions = questionDefinitions()

export function QuestionTree({ tree, catalog, guard }: { readonly tree: Recommendations; readonly catalog: QuestionCatalog; readonly guard?: (dirty: boolean) => ReactNode }) {
  const { view } = useView()
  const editor = useEditor<Draft>({ initial: recommendedDraft(tree) })
  const { value: draft, set } = editor
  const patch = (next: Partial<Draft>) => set((current) => ({ ...current, ...next }))
  const [checked, setChecked] = useState<CheckedQuestion>()
  const [failure, setFailure] = useState<string>()
  const [signOff, setSignOff] = useState<SignOff>()
  const [saved, setSaved] = useState<{ readonly id: string; readonly version: number }>()
  const [saving, setSaving] = useState(false)
  const [verified, setVerified] = useState(false)
  const [removed, setRemoved] = useState<{ readonly id: string; readonly version: number; readonly draft: Draft; readonly question: ReturnType<typeof buildQuestion>; readonly sha256: string }>()
  const [saveError, setSaveError] = useState<string>()
  const [restatementFailure, setRestatementFailure] = useState<string>()
  const [refresh, setRefresh] = useState(0)
  const [pageCursor, setPageCursor] = useState<string>()
  const scope = useStudioScope()
  const cellAvailable = scope.isSuccess && scope.data !== null
  const list = useQuery({ queryKey: ["saved-questions", scope.data?.orgId, scope.data?.environmentId, refresh, pageCursor], queryFn: () => definitions.list(pageCursor), enabled: cellAvailable, retry: false })
  const revision = useRef(editor.revision)
  revision.current = editor.revision
  const question = buildQuestion(draft, catalog)
  const questionKey = JSON.stringify(question)
  const { name: _name, ...restatedQuestion } = question
  const restatementKey = JSON.stringify(restatedQuestion)
  const [checkedKey, setCheckedKey] = useState<string>()
  const [checkedRevision, setCheckedRevision] = useState<object>()
  const [serverWords, setServerWords] = useState<{ readonly key: string; readonly value: { readonly text: string; readonly sha256: string } }>()
  // Only the newest check may authorize this exact editor revision.
  useEffect(() => {
    let current = true
    const timer = setTimeout(() => {
      checkQuestion(question).then((result) => { if (current) { setChecked(result); setCheckedKey(questionKey); setCheckedRevision(editor.revision); setFailure(undefined) } }, (error: unknown) => { if (current) { setChecked(undefined); setFailure(error instanceof Error ? error.message : String(error)) } })
    }, 300)
    return () => { current = false; clearTimeout(timer) }
  }, [questionKey, editor.revision])
  // The cell supplies the words to sign; local checker wording can differ from the cell.
  useEffect(() => {
    if (!cellAvailable) return
    let current = true
    const timer = setTimeout(() => {
      definitions.admit(question.name, question).then((value) => {
        if (current) { setServerWords(value ? { key: restatementKey, value } : undefined); setRestatementFailure(value ? undefined : "The cell did not admit this question.") }
      }, (error: unknown) => {
        if (current) { setServerWords(undefined); setRestatementFailure(error instanceof Error ? error.message : String(error)) }
      })
    }, 300)
    return () => { current = false; clearTimeout(timer) }
  }, [restatementKey, cellAvailable, scope.data?.orgId, scope.data?.environmentId])
  const currentCheck = checkedKey === questionKey && checkedRevision === editor.revision ? checked : undefined
  const fromCell = serverWords?.key === restatementKey ? serverWords.value : undefined
  const restated = fromCell
  const admitted = currentCheck?.verdict.admitted === true
  const signed = signOff !== undefined && restated != null && signOff.sha256 === restated.sha256 && admitted
  useEffect(() => { if (signOff && restated && signOff.sha256 !== restated.sha256) setSignOff(undefined) }, [signOff, restated?.sha256])
  // Only the newest Open may change the editor: a slower earlier read is dropped when it lands.
  const opening = useRef(0)
  const openSaved = async (id: string) => {
    if (editor.dirty && !globalThis.confirm("Discard your unsaved changes and open this question?")) return
    const request = ++opening.current
    try {
      const stored = await definitions.get(id)
      if (request !== opening.current) return
      const parsed = savedQuestion.safeParse(stored.payload)
      if (!parsed.success) throw new QuestionDefinitionError("definition.shape", "This question uses fields this builder cannot edit.")
      const next = draftOf(parsed.data, parsed.data.name)
      if (stable(buildQuestion(next, catalog)) !== stable(parsed.data)) throw new QuestionDefinitionError("definition.unsupported", "This question cannot be reopened without changing it.")
      const { name: _storedName, ...storedQuestion } = buildQuestion(next, catalog)
      const signoff = stored.signoff
      editor.open(next); setCustomCondition(false); setSaved({ id: stored.id, version: stored.version }); setVerified(Boolean(signoff)); setRemoved(undefined)
      setSignOff(signoff && signoff.restatementSha256 === stored.words.sha256 ? { sha256: signoff.restatementSha256, atMs: signoff.signedAtMs, signer: signoff.signer, displayName: signoff.displayName ?? "a removed user" } : undefined)
      setChecked(undefined); setCheckedKey(undefined); setServerWords({ key: JSON.stringify(storedQuestion), value: stored.words }); setRestatementFailure(undefined); setSaveError(undefined)
    } catch (error) { if (request === opening.current) setSaveError(error instanceof Error ? error.message : String(error)) }
  }
  const save = async () => {
    if (!cellAvailable || !signed || !restated || !draft.title.trim() || saving) return
    const at = editor.revision
    setSaving(true); setSaveError(undefined)
    try {
      const result = await definitions.install(saved?.id ?? question.name, question, restated.sha256, saved?.version)
      setSaved({ id: result.id, version: result.version }); setVerified(true); setRemoved(undefined); editor.markSaved(draft); setRefresh((n) => n + 1)
    } catch (error) {
      if (error instanceof QuestionDefinitionError && error.code === "definition.signoff_mismatch") {
        setSignOff(undefined)
        if (revision.current !== at) setSaveError("The earlier question was not saved. Review the current question and sign off again.")
        else if (error.restatement) {
          setServerWords({ key: restatementKey, value: error.restatement })
          setSaveError("The server reads this question differently. Review its words and sign off again.")
        } else {
          setChecked(undefined); setCheckedKey(undefined); setServerWords(undefined)
          setSaveError("The cell rejected these words, but its updated restatement is not available yet. Your question was not saved.")
        }
      } else if (error instanceof QuestionDefinitionError && error.code === "definition.signoff_unavailable") {
        setSaveError("The cell cannot verify sign-off yet. Your question has not been saved.")
      } else if (error instanceof QuestionDefinitionError && error.status === 403) {
        setSaveError(`You do not have permission to save this question. ${error.message}`)
      } else setSaveError(error instanceof Error ? error.message : String(error))
    } finally { setSaving(false) }
  }
  const remove = async () => {
    if (!cellAvailable || !saved || saving || !signed || !restated || !verified || editor.dirty || !globalThis.confirm("Remove this saved question?")) return
    setSaving(true)
    try {
      const result = await definitions.retire(saved.id, saved.version)
      setRemoved({ id: saved.id, version: result.version, draft, question, sha256: restated.sha256 })
      setSaved(undefined); setVerified(false); setRefresh((n) => n + 1); setSaveError(undefined)
    } catch (error) { setSaveError(error instanceof Error ? error.message : String(error)) }
    finally { setSaving(false) }
  }
  const undoRemove = async () => {
    if (!cellAvailable || !removed || saving) return
    setSaving(true)
    try {
      const result = await definitions.install(removed.id, removed.question, removed.sha256, removed.version)
      setSaved({ id: result.id, version: result.version }); setVerified(true); editor.markSaved(removed.draft); setRemoved(undefined); setRefresh((n) => n + 1); setSaveError(undefined)
    } catch (error) { setSaveError(error instanceof Error ? error.message : String(error)) }
    finally { setSaving(false) }
  }
  // Set by choosing "Another condition", cleared by choosing an offered one or opening a saved question.
  const [customCondition, setCustomCondition] = useState(false)
  const chosen = selection(tree, draft, customCondition)
  const issues = issuesOf(currentCheck?.verdict)
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
      <EditorToolbar editor={editor} version={saved?.version} showSaveState />
      <div className="flex items-start gap-6 overflow-x-auto pb-2" data-testid="question-tree">
        <ChoiceColumn title="Measure" field="measures" invalid={invalid("measure") || invalid("measures")} options={measureOptions} selected={chosen.measure} onSelect={(id) => patch({ measures: [id] })} />
        <div className="flex flex-col gap-4">
          <ChoiceColumn title="Group by" field="by" invalid={invalid("by")} options={groupOptions} selected={chosen.by}
            onSelect={(id) => patch({ by: id, grain: (tree.grains[id] ?? []).find(({ recommended }) => recommended)?.id ?? tree.grains[id]?.[0]?.id ?? "" })} />
          {grains.length > 0 && <ChoiceColumn title={`Every (grain of ${words(draft.by)})`} options={options(grains)} selected={chosen.grain} onSelect={(id) => patch({ grain: id })} />}
        </div>
        <ChoiceColumn title="Only where" field="filters" invalid={invalid("filter") || invalid("filters")} options={conditionOptions} selected={chosen.condition}
          onSelect={(id) => {
            if (id === "custom") { setCustomCondition(true); return patch({ filters: [custom] }) }
            const choice = tree.conditions.find((entry) => entry.id === id)
            if (choice) { setCustomCondition(false); patch({ filters: conditionsOf(choice) }) }
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
      {restatementFailure && <p role="alert" className="text-destructive">{restatementFailure}</p>}
      {currentCheck && !admitted && <ValidationPanel issues={issues} title="The checker refused this question" details={<div className="space-y-1 font-mono break-all">{currentCheck.verdict.findings.map(({ code }, index) => <div key={`${code}:${index}`}>{code}</div>)}</div>} />}
      {currentCheck && restated === null && <p role="status" className="text-sm text-muted-foreground">This question cannot be stated in plain words, so it cannot be signed off: {currentCheck.restatementError ?? "the checker gave no reason"}.</p>}
      {restated && (
        <Panel icon={ShieldCheck} label="In plain words" action={view === "technical" ? <span className="font-mono text-xs text-muted-foreground" title={restated.sha256}>{restated.sha256.slice(0, 12)}</span> : undefined}>
          <div className="space-y-3 p-1">
            {fromCell && <p className="font-medium">The server reads your question as:</p>}
            <p className="text-base" data-testid="restatement">{restated.text}</p>
            <p className="text-meta text-muted-foreground">Written from the question itself by the server, not by the assistant. Check that it says what you mean.</p>
            <div className="flex flex-wrap items-center gap-3">
              {signed
                ? <span className="flex items-center gap-2 text-sm font-medium"><BadgeCheck className="size-4 text-success" />{signOff.signer ? <>Signed off by {signOff.displayName} on {new Date(signOff.atMs).toLocaleDateString()}{view === "technical" && <span className="font-mono text-meta text-muted-foreground">({signOff.signer})</span>}</> : "Signed off: this is what you asked for"}</span>
                : <Button type="button" size="sm" disabled={!admitted} title={admitted ? undefined : "The checker must admit the question first"} onClick={() => restated && setSignOff({ sha256: restated.sha256, atMs: Date.now() })}>This is what I asked for</Button>}
              {!admitted && <span className="text-meta text-muted-foreground">Sign-off opens once the checker admits the question.</span>}
            </div>
          </div>
        </Panel>
      )}
      <div className="space-y-2" data-testid="question-save-controls">
        <div className="flex flex-wrap items-end gap-3">
          <label className="grid gap-1.5 text-sm">Question name<Input value={draft.title} onChange={(event) => patch({ title: event.target.value })} /></label>
          <Button type="button" onClick={save} disabled={!cellAvailable || !signed || !draft.title.trim() || saving} aria-describedby={!signed ? "question-save-reason" : undefined}>{saving ? "Saving…" : "Save question"}</Button>
          {saved && <Button type="button" variant="outline" disabled={!cellAvailable || saving || !signed || !verified || editor.dirty} onClick={remove}>Remove question</Button>}
          {removed && <Button type="button" variant="outline" disabled={!cellAvailable || saving} onClick={undoRemove}>Undo removal</Button>}
        </div>
        {scope.isSuccess && scope.data === null && <p role="status" className="text-sm text-muted-foreground">{NO_DATA_CELL}</p>}
        {scope.isError && <p role="alert" className="text-destructive">{scope.error.message}</p>}
        {!signed && <p id="question-save-reason" className="text-meta text-muted-foreground">Sign off on the plain words first.</p>}
        {saveError && <p role="alert" className="text-destructive">{saveError}</p>}
      </div>
      {signed ? <RunQuestion key={restated?.sha256} question={question} catalog={catalog} /> : <p className="text-meta text-muted-foreground">Run opens once you have signed off on the question in plain words.</p>}
      <Panel icon={ChartLine} label="Saved questions">
        {cellAvailable && list.isPending && <p>Loading saved questions…</p>}
        {list.isError && <p role="alert" className="text-destructive">{list.error.message}</p>}
        {list.data?.items.length === 0 && <p className="text-sm text-muted-foreground">No saved questions yet.</p>}
        {list.data?.items.map((item) => <div key={item.id} className="flex items-center justify-between gap-3 py-2"><span><span>{item.title || item.id} · version {item.version}</span><span className="block text-meta text-muted-foreground">{item.id}</span></span><Button type="button" variant="outline" size="sm" disabled={!cellAvailable || saving} onClick={() => openSaved(item.id)}>Open</Button></div>)}
        {pageCursor && <Button type="button" variant="ghost" onClick={() => setPageCursor(undefined)}>First page</Button>}
        {list.data?.cursor && <Button type="button" variant="ghost" onClick={() => setPageCursor(list.data.cursor ?? undefined)}>Next page</Button>}
      </Panel>
    </div>
  )
}

export function QuestionTreePage() {
  const catalog = useQuery({ queryKey: ["question-catalog"], queryFn: readCatalog, retry: false, staleTime: Infinity })
  const tree = useQuery({ queryKey: ["question-tree"], queryFn: readRecommendations, retry: false, staleTime: 60_000 })
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-semibold tracking-tight">Build a question</h1>
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
