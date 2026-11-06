import { Plus, X } from "lucide-react"
import { useId } from "react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import type { Field } from "../lib/catalog.ts"
import { LIMITS, opsFor, type Clause, type Op, type Predicate } from "../lib/records-query.ts"
import { diagnosticsAt, type QueryDiagnostic } from "../lib/records-source.ts"

/** A clause being written: the value is kept as typed until it is applied. */
export interface DraftClause { readonly property: string; readonly op: Op; readonly value: string; readonly high: string; readonly values: readonly string[] }

export const OP_LABEL: Readonly<Record<Op, string>> = {
  eq: "is", ne: "is not", in: "is one of", contains: "contains", prefix: "starts with",
  lt: "is less than", lte: "is at most", gt: "is more than", gte: "is at least", between: "is between",
  empty: "has no value", present: "has a value",
}

export const filterable = (fields: readonly Field[]) => fields.filter((field) => opsFor(field.scalar).length > 0)

export function blankClause(field: Field): DraftClause {
  return { property: field.id, op: opsFor(field.scalar)[0] ?? "eq", value: field.scalar.kind === "boolean" ? "true" : "", high: "", values: [] }
}

/** The draft as the wire predicate. Values go as typed: the server validates them and points at any it refuses. */
export function toPredicate(draft: readonly DraftClause[], fields: readonly Field[]): Predicate | null {
  if (draft.length === 0) return null
  const all = draft.map((clause): Clause => {
    const kind = fields.find(({ id }) => id === clause.property)?.scalar.kind
    switch (clause.op) {
      case "empty":
      case "present": return { property: clause.property, op: clause.op }
      case "between": return { property: clause.property, op: "between", value: [clause.value.trim(), clause.high.trim()] }
      case "in": return { property: clause.property, op: "in", value: clause.values }
      default: return { property: clause.property, op: clause.op, value: kind === "boolean" ? clause.value === "true" : clause.value.trim() }
    }
  })
  return { all }
}

export function fromPredicate(predicate: Predicate | null): readonly DraftClause[] {
  return (predicate?.all ?? []).map((clause) => {
    const value = "value" in clause ? clause.value : undefined
    return {
      property: clause.property, op: clause.op,
      value: Array.isArray(value) ? (clause.op === "between" ? String(value[0] ?? "") : "") : value === undefined ? "" : String(value),
      high: clause.op === "between" && Array.isArray(value) ? String(value[1] ?? "") : "",
      values: clause.op === "in" && Array.isArray(value) ? value.map(String) : [],
    }
  })
}

export const sameDraft = (a: readonly DraftClause[], b: readonly DraftClause[]) => JSON.stringify(a) === JSON.stringify(b)

const select = "h-8 rounded-md border bg-background px-2 text-sm"

function ValueInput({ field, clause, onChange, label }: { readonly field: Field; readonly clause: DraftClause; readonly onChange: (next: DraftClause) => void; readonly label: string }) {
  const kind = field.scalar.kind
  if (clause.op === "empty" || clause.op === "present") return null
  if (clause.op === "in") {
    if (field.scalar.kind === "enum") {
      return (
        <select multiple aria-label={label} className={`${select} h-auto min-h-16`} value={[...clause.values]}
          onChange={(event) => onChange({ ...clause, values: [...event.target.selectedOptions].map(({ value }) => value) })}>
          {field.scalar.values.map((value) => <option key={value} value={value}>{value}</option>)}
        </select>
      )
    }
    return (
      <textarea aria-label={label} placeholder="One value per line" className="min-h-16 rounded-md border bg-background px-2 py-1 text-sm"
        value={clause.values.join("\n")} onChange={(event) => onChange({ ...clause, values: event.target.value.split("\n").map((v) => v.trim()).filter(Boolean).slice(0, LIMITS.inValues) })} />
    )
  }
  if (kind === "enum" && field.scalar.kind === "enum") {
    return (
      <select aria-label={label} className={select} value={clause.value} onChange={(event) => onChange({ ...clause, value: event.target.value })}>
        <option value="" disabled>Choose…</option>
        {field.scalar.values.map((value) => <option key={value} value={value}>{value}</option>)}
      </select>
    )
  }
  if (kind === "boolean") {
    return (
      <select aria-label={label} className={select} value={clause.value} onChange={(event) => onChange({ ...clause, value: event.target.value })}>
        <option value="true">Yes</option><option value="false">No</option>
      </select>
    )
  }
  const typed = kind === "date" ? { type: "date" } : kind === "decimal" || kind === "integer" ? { inputMode: "decimal" as const } : {}
  const placeholder = kind === "timestamp" ? "2026-11-06T09:00:00Z" : kind === "duration" ? "P1D" : kind === "decimal" ? "Exact number" : undefined
  const one = (key: "value" | "high", aria: string) => (
    <Input {...typed} aria-label={aria} placeholder={placeholder} className="h-8 w-40" value={clause[key]} onChange={(event) => onChange({ ...clause, [key]: event.target.value })} />
  )
  return clause.op === "between" ? <span className="flex items-center gap-1">{one("value", `${label}, from`)}<span className="text-meta text-muted-foreground">and</span>{one("high", `${label}, to`)}</span> : one("value", label)
}

/** Writes the filter; the server applies it. Every refusal shows under the condition it points at. */
export function FilterEditor({ fields, draft, onDraft, onApply, onClear, applied, diagnostics }: {
  readonly fields: readonly Field[]
  readonly draft: readonly DraftClause[]
  readonly onDraft: (draft: readonly DraftClause[]) => void
  readonly onApply: () => void
  readonly onClear: () => void
  readonly applied: readonly DraftClause[]
  readonly diagnostics: readonly QueryDiagnostic[]
}) {
  const id = useId()
  const options = filterable(fields)
  const dirty = !sameDraft(draft, applied)
  const set = (index: number, next: DraftClause) => onDraft(draft.map((clause, i) => i === index ? next : clause))
  const general = diagnosticsAt(diagnostics, "/filter").filter(({ pointer }) => !pointer?.startsWith("/filter/all/"))
  return (
    <form className="grid gap-3" aria-label="Filter records" onSubmit={(event) => { event.preventDefault(); onApply() }}>
      {draft.length === 0 && <p className="text-sm text-muted-foreground">No conditions. Every record is shown.</p>}
      <ol className="grid gap-2">
        {draft.map((clause, index) => {
          const field = options.find(({ id: fid }) => fid === clause.property) ?? options[0]
          if (field === undefined) return null
          const label = `Condition ${index + 1}`
          const problems = diagnosticsAt(diagnostics, `/filter/all/${index}`)
          return (
            <li key={index} className="grid gap-1">
              <div className="flex flex-wrap items-start gap-2">
                <select aria-label={`${label} property`} className={select} value={clause.property}
                  onChange={(event) => { const next = options.find(({ id: fid }) => fid === event.target.value); if (next) set(index, blankClause(next)) }}>
                  {options.map((option) => <option key={option.id} value={option.id}>{option.id.replaceAll("_", " ")}</option>)}
                </select>
                <select aria-label={`${label} test`} className={select} value={clause.op} onChange={(event) => set(index, { ...clause, op: event.target.value as Op })}>
                  {opsFor(field.scalar).map((op) => <option key={op} value={op}>{OP_LABEL[op]}</option>)}
                </select>
                <ValueInput field={field} clause={clause} label={`${label} value`} onChange={(next) => set(index, next)} />
                <Button type="button" variant="ghost" size="icon" aria-label={`Remove condition ${index + 1}`} onClick={() => onDraft(draft.filter((_, i) => i !== index))}><X /></Button>
              </div>
              {problems.map((problem) => <p key={`${problem.pointer}:${problem.code}`} id={`${id}-${index}`} role="alert" className="text-meta text-destructive">{problem.message}</p>)}
            </li>
          )
        })}
      </ol>
      {general.map((problem) => <p key={problem.code} role="alert" className="text-meta text-destructive">{problem.message}</p>)}
      <div className="flex flex-wrap items-center gap-2">
        <Button type="button" variant="outline" size="sm" disabled={options.length === 0 || draft.length >= LIMITS.clauses}
          onClick={() => { const first = options[0]; if (first) onDraft([...draft, blankClause(first)]) }}>
          <Plus /> Add condition
        </Button>
        <span className="flex-1" />
        {dirty && <span className="text-meta text-muted-foreground">Not applied yet</span>}
        <Button type="button" variant="ghost" size="sm" disabled={draft.length === 0 && applied.length === 0} onClick={onClear}>Clear</Button>
        <Button type="submit" size="sm" disabled={!dirty}>Apply</Button>
      </div>
      {draft.length >= LIMITS.clauses && <p className="text-meta text-muted-foreground">A filter has at most {LIMITS.clauses} conditions.</p>}
    </form>
  )
}
