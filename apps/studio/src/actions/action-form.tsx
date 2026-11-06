import { AlertTriangle, Check, CheckCircle2, ChevronLeft, Lock, Pencil, Plus, X } from "lucide-react"
import { useEffect, useId, useMemo, useState, type ReactNode } from "react"
import { CopyId } from "@/components/copy-id"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { cn } from "@/lib/utils"
import {
  ACTIONS_UNAVAILABLE, callAction, decimalExample, inputPointer, itemNoun, newIdempotencyKey, pastTense, risky as isRisky, writesWhole, draftAt, emptyDraft, encodeObject, fieldsOf, groupsOf, problemsAt, sectionLabel, sectionsOf, unplaced, unsupported, withDraftAt,
  type ActionReceipt, type ActionSummary, type CallResult, type Diagnostic, type Draft, type FormField, type Section, type Spec,
} from "../lib/actions.ts"

type Call = (actionId: string, input: string, key: string) => Promise<CallResult>

type Phase =
  | { readonly kind: "editing" }
  | { readonly kind: "confirm" }
  | { readonly kind: "sending" }
  | { readonly kind: "refused"; readonly message: string; readonly code: string; readonly diagnostics: readonly Diagnostic[] }
  | { readonly kind: "failed"; readonly message: string }
  | { readonly kind: "done"; readonly receipt: ActionReceipt }

const newKey = newIdempotencyKey
const control = "h-9 w-full rounded-lg border border-node-border bg-background px-3 text-sm aria-invalid:border-destructive aria-invalid:ring-2 aria-invalid:ring-destructive/20"

/**
 * One governed action as a form built from its admitted input schema. The cell validates and
 * decides; the form shows each refusal at the field its pointer names, and the receipt on success.
 * A draft keeps one idempotency key until it changes, so a retry after an outage never commits twice.
 */
export function ActionForm({ action, objectTitle, technical = false, prefill = {}, onDirty, onClose, onView, remembered, onDraft, onCommitted, call = callAction }: {
  readonly action: ActionSummary
  /** The object type in words ("Holding"), for the business sentence on success. */
  readonly objectTitle: string
  /** Technical view: receipt details open by default. */
  readonly technical?: boolean
  /** Open the committed record (its drawer in the grid). */
  readonly onView?: (objectId: string) => void
  /** Values the entry point already knows (the record's id for a transition); shown, not editable. */
  readonly prefill?: Readonly<Record<string, string>>
  readonly onDirty?: (dirty: boolean) => void
  readonly onClose?: () => void
  /** A draft kept from earlier in this session, to start from instead of the empty form. */
  readonly remembered?: Draft
  /** Every change to the draft, so the sheet can keep it in memory if the user closes and keeps it. */
  readonly onDraft?: (draft: Draft) => void
  readonly onCommitted?: () => void
  readonly call?: Call
}) {
  const fields = useMemo(() => fieldsOf(action.inputSchema), [action])
  const sections = useMemo(() => sectionsOf(fields, groupsOf(action.inputSchema), action.objectIdField), [fields, action])
  const blocked = useMemo(() => unsupported(fields), [fields])
  const initial = useMemo(() => Object.entries(prefill).reduce<Draft>((draft, [name, value]) => withDraftAt(draft, `/${name}`, value), emptyDraft(fields)), [fields, prefill])
  const [draft, setDraft] = useState<Draft>(remembered ?? initial)
  // A kept draft is unsent work: the sheet treats it as dirty from the start.
  useEffect(() => { if (remembered !== undefined && JSON.stringify(remembered) !== JSON.stringify(initial)) onDirty?.(true) }, [])
  // A schema with two or more groups is a wizard: one step per section, then a review.
  const wizard = groupsOf(action.inputSchema).length >= 2 && sections.length >= 2
  const review = sections.length
  const [step, setStep] = useState(0)
  const [reached, setReached] = useState(remembered === undefined ? 0 : review)
  const [key, setKey] = useState(newKey)
  const [phase, setPhase] = useState<Phase>({ kind: "editing" })
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial) && phase.kind !== "done"
  const diagnostics = phase.kind === "refused" ? phase.diagnostics : []
  const risky = isRisky(action)

  const edit = (pointer: string, value: Draft) => {
    const next = withDraftAt(draft, pointer, value)
    setDraft(next)
    onDraft?.(next)
    setKey(newKey()) // a different call: a different key
    if (phase.kind === "failed" || phase.kind === "confirm") setPhase({ kind: "editing" })
    onDirty?.(JSON.stringify(next) !== JSON.stringify(initial))
  }

  const send = async () => {
    setPhase({ kind: "sending" })
    try {
      const result = await call(action.actionId, encodeObject(fields, draft as { readonly [name: string]: Draft }), key)
      if (result.kind === "receipt") { setPhase({ kind: "done", receipt: result.receipt }); onDirty?.(false); onCommitted?.() }
      else if (result.kind === "refused") {
        setPhase({ kind: "refused", message: result.message, code: result.code, diagnostics: result.diagnostics })
        // In a wizard, go to the first step the cell refused something on; a refusal on no field stays on the review.
        if (wizard) { const first = problemsBySection(sections, result.diagnostics).findIndex((n) => n > 0); if (first >= 0) setStep(first) }
      }
      else setPhase({ kind: "failed", message: result.kind === "no-cell" ? "This workspace has no data cell yet." : ACTIONS_UNAVAILABLE })
    } catch (error) {
      setPhase({ kind: "failed", message: error instanceof Error ? error.message : "The cell did not answer; nothing was saved." })
    }
  }

  if (phase.kind === "done") {
    return <Receipt action={action} objectTitle={objectTitle} technical={technical} onView={onView} receipt={phase.receipt} onAgain={writesWhole(action) ? () => { setDraft(emptyDraft(fields)); setKey(newKey()); setStep(0); setReached(0); setPhase({ kind: "editing" }) } : undefined} onClose={onClose} />
  }

  const top = unplaced(diagnostics, fields)
  const counts = problemsBySection(sections, diagnostics)
  const go = (to: number) => { setStep(to); setReached((r) => Math.max(r, to)); if (phase.kind === "confirm") setPhase({ kind: "editing" }) }
  const shown = wizard ? (step < review ? [sections[step]!] : []) : sections
  return (
    <form noValidate className="grid gap-5" aria-label={action.title} aria-busy={phase.kind === "sending"}
      onSubmit={(event) => {
        event.preventDefault()
        if (phase.kind === "sending") return
        if (wizard && step < review) { go(step + 1); return } // the cell validates once, on submit; steps never block
        if (risky && phase.kind !== "confirm") setPhase({ kind: "confirm" }); else void send()
      }}>
      {action.description && <p className="text-sm text-muted-foreground">{action.description}</p>}
      {blocked !== null && <p role="alert" className="rounded-lg border border-node-border bg-muted/50 p-3 text-sm">Studio can't build a form for this action yet ({blocked}). It's listed so you know it exists.</p>}
      {phase.kind === "refused" && (
        <div role="alert" className="grid gap-1 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
          <span className="font-medium">{phase.message}</span>
          {top.map((d) => <span key={`${d.pointer}:${d.code}:${d.message}`} className="text-meta">{d.message}</span>)}
          {top.length === 0 && phase.diagnostics.length > 0 && <span className="text-meta">Each problem is shown at its field below.</span>}
        </div>
      )}
      {phase.kind === "failed" && (
        <div role="alert" className="flex flex-wrap items-center gap-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
          <span>{phase.message}</span><span className="flex-1" />
          <Button type="button" variant="outline" size="sm" onClick={() => void send()}>Retry</Button>
        </div>
      )}
      {wizard && <Steps sections={sections} step={step} reached={reached} counts={counts} onGo={go} />}
      {wizard && step === review && <Review sections={sections} fields={fields} draft={draft} counts={counts} prefill={prefill} onEdit={go} />}
      {shown.map((section) => {
        const heading = section.title ?? (sections.length > 1 ? "More" : null)
        const headingId = `section-${section.id ?? "rest"}`
        return (
          <section key={section.id ?? "rest"} aria-labelledby={heading === null ? undefined : headingId} aria-label={heading === null ? action.title : undefined}
            className="grid min-w-0 gap-4 rounded-node border border-node-border bg-card p-4 shadow-[var(--node-highlight),var(--elevation-node)]">
            {heading !== null && <h3 id={headingId} className={cn("text-sm font-medium", section.title === null && "text-muted-foreground")}>{heading}</h3>}
            {section.fields.map((field) => (
              <FieldRow key={field.pointer} field={field} label={sectionLabel(fields, field)} draft={draft} diagnostics={diagnostics}
                locked={prefill[field.name] !== undefined && field.pointer === `/${field.name}`} onEdit={edit} />
            ))}
          </section>
        )
      })}
      {phase.kind === "confirm" && (
        <div role="alertdialog" aria-labelledby="confirm-title" className="grid gap-2 rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm">
          <p id="confirm-title" className="flex items-center gap-2 font-medium text-destructive"><AlertTriangle className="size-4" aria-hidden />
            {action.uiEffects.irreversible ? "This can't be undone." : "This removes business data."}</p>
          <p className="text-muted-foreground">{action.title}{action.uiEffects.destructive && action.uiEffects.irreversible ? ": it removes business data and can't be undone." : "."} The cell still checks it before anything changes.</p>
        </div>
      )}
      {/* The submit bar stays in view on a long form: a footer on the sheet's surface with a hairline above. */}
      <div className="sticky bottom-0 z-10 -mx-6 flex flex-wrap items-center gap-2 border-t border-node-border bg-card/95 px-6 py-3 backdrop-blur-sm">
        {dirty && phase.kind === "editing" && <span className="text-meta text-muted-foreground">Not sent yet</span>}
        <span className="flex-1" />
        {phase.kind === "confirm"
          ? <Button type="button" variant="ghost" onClick={() => setPhase({ kind: "editing" })}>Back</Button>
          : wizard && step > 0 && <Button type="button" variant="ghost" onClick={() => go(step - 1)}><ChevronLeft /> Back</Button>}
        <Button type="submit" variant={phase.kind === "confirm" ? "destructive" : "default"} disabled={blocked !== null || phase.kind === "sending"}>
          {phase.kind === "sending" ? "Sending…" : phase.kind === "confirm" ? `Yes, ${action.title.toLowerCase()}`
            : wizard && step < review - 1 ? "Next" : wizard && step === review - 1 ? "Review" : action.title}
        </Button>
      </div>
    </form>
  )
}

function FieldRow({ field, label, draft, diagnostics, locked, onEdit }: {
  readonly field: FormField; readonly label: string; readonly draft: Draft; readonly diagnostics: readonly Diagnostic[]; readonly locked: boolean
  readonly onEdit: (pointer: string, value: Draft) => void
}) {
  const id = useId()
  // A record (or a list of records) shows only its own problems; its members show theirs.
  const own = field.spec.kind === "object" || (field.spec.kind === "array" && field.spec.item.kind === "object")
  const problems = own ? diagnostics.filter((d) => inputPointer(d) === field.pointer) : problemsAt(diagnostics, field.pointer)
  const value = draftAt(draft, field.pointer)
  const described = [field.description ? `${id}-help` : "", problems.length > 0 ? `${id}-problem` : ""].filter(Boolean).join(" ") || undefined
  const aria = { id, "aria-describedby": described, "aria-invalid": problems.length > 0 || undefined, "aria-required": field.required || undefined }
  return (
    <div className="grid gap-1.5" data-pointer={field.pointer}>
      <label htmlFor={id} className="flex items-center gap-1 text-sm font-medium">
        {label}
        {field.spec.kind === "decimal" && field.spec.unit && <span className="text-meta font-normal text-muted-foreground">({field.spec.unit})</span>}
        {field.required && <span aria-hidden className="text-destructive">*</span>}
      </label>
      {field.description && <p id={`${id}-help`} className="text-meta text-muted-foreground">{field.description}</p>}
      {locked
        ? (
          <div className="relative">
            <Lock className="pointer-events-none absolute top-1/2 left-3 size-3.5 -translate-y-1/2 text-muted-foreground" aria-hidden />
            <Input id={id} readOnly aria-readonly value={String(value ?? "")} title="This record: set by where you opened the action"
              className="h-9 cursor-default bg-muted/70 pl-8 font-mono text-xs text-muted-foreground focus-visible:ring-0" />
          </div>
        )
        : field.nullable
          ? <Nullable field={field} value={value ?? ""} onEdit={onEdit} widget={<Widget spec={field.spec} pointer={field.pointer} label={field.label} value={value ?? ""} root={draft} diagnostics={diagnostics} onEdit={onEdit} aria={aria} />} />
          : <Widget spec={field.spec} pointer={field.pointer} label={field.label} value={value ?? ""} root={draft} diagnostics={diagnostics} onEdit={onEdit} aria={aria} />}
      {problems.length > 0 && <div id={`${id}-problem`} role="alert" className="grid text-meta text-destructive">{problems.map((p) => <span key={`${p.pointer}:${p.code}:${p.message}`}>{p.message}</span>)}</div>}
    </div>
  )
}

function Nullable({ field, value, onEdit, widget }: { readonly field: FormField; readonly value: Draft; readonly onEdit: (pointer: string, value: Draft) => void; readonly widget: ReactNode }) {
  const none = value === null
  return (
    <div className="grid gap-1.5">
      {!none && widget}
      <label className="flex items-center gap-2 text-meta text-muted-foreground">
        <input type="checkbox" checked={none} onChange={(event) => onEdit(field.pointer, event.target.checked ? null : "")} /> No value
      </label>
    </div>
  )
}

type Aria = Record<string, string | boolean | undefined>

/** Fields of a record item re-pointed from the schema's item `0` to the item they edit. */
const rebase = (fields: readonly FormField[], from: string, to: string): readonly FormField[] =>
  fields.map((f) => ({ ...f, pointer: to + f.pointer.slice(from.length), spec: f.spec.kind === "object" ? { ...f.spec, fields: rebase(f.spec.fields, from, to) } : f.spec }))

function Widget({ spec, pointer, label, value, root, diagnostics, onEdit, aria, bare = false }: {
  readonly spec: Spec; readonly pointer: string; readonly label: string; readonly value: Draft; readonly bare?: boolean; readonly root: Draft; readonly diagnostics: readonly Diagnostic[]
  readonly onEdit: (pointer: string, value: Draft) => void; readonly aria: Aria
}) {
  const text = typeof value === "string" ? value : ""
  switch (spec.kind) {
    case "text": return <Input {...aria} className="h-9" value={text} onChange={(e) => onEdit(pointer, e.target.value)} />
    case "integer":
    case "number":
    case "decimal":
      return <Input {...aria} className="h-9 tabular-nums" inputMode={spec.kind === "integer" ? "numeric" : "decimal"} autoComplete="off"
        placeholder={spec.kind === "decimal" ? decimalExample(spec.scale) : spec.kind === "integer" ? decimalExample(0) : undefined}
        value={text} onChange={(e) => onEdit(pointer, e.target.value)} />
    case "boolean":
      return (
        <select {...aria} className={control} value={text} onChange={(e) => onEdit(pointer, e.target.value)}>
          <option value="">Choose…</option><option value="true">Yes</option><option value="false">No</option>
        </select>
      )
    case "choice":
      return (
        <select {...aria} className={control} value={text} onChange={(e) => onEdit(pointer, e.target.value)}>
          <option value="">Choose…</option>
          {spec.values.map((v) => <option key={v} value={v}>{v}</option>)}
        </select>
      )
    case "fixed": return <p {...aria} className="text-sm"><code>{spec.value}</code></p>
    case "json": return <Textarea {...aria} className="min-h-20 font-mono text-xs" spellCheck={false} placeholder="JSON" value={text} onChange={(e) => onEdit(pointer, e.target.value)} />
    case "object":
      return (
        <div className={cn("grid gap-3", !bare && "rounded-lg border border-node-border p-3")}>
          {spec.fields.map((child) => <FieldRow key={child.pointer} field={child} label={child.label} draft={root} diagnostics={diagnostics} locked={false} onEdit={onEdit} />)}
        </div>
      )
    case "array": {
      const items = Array.isArray(value) ? value as readonly Draft[] : []
      const blank: Draft = spec.item.kind === "object" ? emptyDraft(spec.item.fields) : ""
      const noun = itemNoun(spec.itemTitle, label)
      return (
        <div className="grid gap-2">
          {items.map((item, index) => (
            <div key={index} className="relative rounded-lg border border-node-border bg-card p-3 pr-11">
              <Widget spec={spec.item.kind === "object" ? { ...spec.item, fields: rebase(spec.item.fields, `${pointer}/0`, `${pointer}/${index}`) } : spec.item}
                pointer={`${pointer}/${index}`} label={`${noun} ${index + 1}`} value={item} root={root} diagnostics={diagnostics} onEdit={onEdit} bare
                aria={{ "aria-label": `${noun.charAt(0).toUpperCase()}${noun.slice(1)} ${index + 1}` }} />
              <Button type="button" variant="ghost" size="icon" className="absolute top-2 right-2 size-7 text-muted-foreground hover:text-foreground"
                aria-label={`Remove ${noun} ${index + 1}`} title={`Remove ${noun} ${index + 1}`} onClick={() => onEdit(pointer, items.filter((_, i) => i !== index))}><X /></Button>
            </div>
          ))}
          <Button type="button" variant="outline" size="sm" className="justify-self-start border-node-border" onClick={() => onEdit(pointer, [...items, blank])}><Plus /> Add {noun}</Button>
        </div>
      )
    }
    case "unsupported": return <p className="text-meta text-muted-foreground">Not offered here: {spec.reason}</p>
  }
}

/** How many of the cell's diagnostics land on each section's fields. */
function problemsBySection(sections: readonly Section[], diagnostics: readonly Diagnostic[]): readonly number[] {
  return sections.map((section) => section.fields.reduce((n, field) => n + problemsAt(diagnostics, field.pointer).length, 0))
}

/** The wizard's steps: each section, then the review. Steps already reached can be revisited; one with a refusal says how many. */
function Steps({ sections, step, reached, counts, onGo }: {
  readonly sections: readonly Section[]; readonly step: number; readonly reached: number; readonly counts: readonly number[]; readonly onGo: (to: number) => void
}) {
  const names = [...sections.map((s) => s.title ?? "More"), "Review"]
  return (
    <nav aria-label="Steps">
      <ol className="flex flex-wrap items-center gap-1.5">
        {names.map((name, index) => {
          const current = index === step
          const done = index < step || (index <= reached && !current)
          const problems = counts[index] ?? 0
          return (
            <li key={name + index} className="flex items-center gap-1.5">
              {index > 0 && <span aria-hidden className="h-px w-3 bg-node-border" />}
              <button type="button" disabled={index > reached} aria-current={current ? "step" : undefined} onClick={() => onGo(index)}
                className={cn("inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs transition-colors disabled:cursor-default disabled:opacity-50",
                  current ? "border-transparent bg-primary text-primary-foreground" : "border-node-border bg-card hover:bg-muted",
                  problems > 0 && !current && "border-destructive/40 text-destructive")}>
                <span className={cn("inline-flex size-4 items-center justify-center rounded-full text-[10px] tabular-nums", current ? "bg-primary-foreground/20" : "bg-muted")}>
                  {problems > 0 ? "!" : done ? <Check className="size-2.5" aria-hidden /> : index + 1}
                </span>
                {name}
                {problems > 0 && <span className="sr-only">, {problems} {problems === 1 ? "problem" : "problems"}</span>}
              </button>
            </li>
          )
        })}
      </ol>
    </nav>
  )
}

/** What a draft value reads as on the review: as typed, or plainly not given. */
function shownValue(field: FormField, value: Draft | undefined): ReactNode {
  if (value === null) return <span className="text-muted-foreground">No value</span>
  if (value === undefined || value === "") return <span className="text-muted-foreground">Not given</span>
  if (Array.isArray(value)) return value.length === 0 ? <span className="text-muted-foreground">None</span> : `${value.length} ${value.length === 1 ? "entry" : "entries"}`
  if (typeof value === "object") return <span className="text-muted-foreground">See below</span>
  if (field.spec.kind === "boolean") return value === "true" ? "Yes" : value === "false" ? "No" : String(value)
  if (field.spec.kind === "decimal" && field.spec.unit) return <span className="tabular-nums">{String(value)} {field.spec.unit}</span>
  return String(value)
}

/** The review before submit: every section's values, each with Edit; the cell checks them only when you submit. */
function Review({ sections, fields, draft, counts, prefill, onEdit }: {
  readonly sections: readonly Section[]; readonly fields: readonly FormField[]; readonly draft: Draft; readonly counts: readonly number[]
  readonly prefill: Readonly<Record<string, string>>; readonly onEdit: (step: number) => void
}) {
  const rows = (field: FormField): readonly FormField[] => field.spec.kind === "object" ? field.spec.fields.flatMap(rows) : [field]
  return (
    <section aria-label="Review" className="grid gap-3">
      <p className="text-sm text-muted-foreground">Check what you entered. The cell checks it when you submit.</p>
      {sections.map((section, index) => (
        <div key={section.id ?? "rest"} className="grid gap-2 rounded-node border border-node-border bg-card p-4 shadow-[var(--node-highlight),var(--elevation-node)]">
          <div className="flex items-center gap-2">
            <h3 className="text-sm font-medium">{section.title ?? "More"}</h3>
            {(counts[index] ?? 0) > 0 && <span className="text-meta text-destructive">{counts[index]} to fix</span>}
            <span className="flex-1" />
            <Button type="button" variant="ghost" size="sm" className="h-7" aria-label={`Edit ${section.title ?? "More"}`} onClick={() => onEdit(index)}><Pencil /> Edit</Button>
          </div>
          <dl className="grid grid-cols-[minmax(8rem,auto)_1fr] gap-x-4 gap-y-1.5 text-sm">
            {section.fields.flatMap(rows).map((field) => (
              <div key={field.pointer} className="contents">
                <dt className="text-muted-foreground">{sectionLabel(fields, field)}</dt>
                <dd className="min-w-0 break-words">
                  {prefill[field.name] !== undefined && field.pointer === `/${field.name}` && <Lock className="mr-1 inline size-3 text-muted-foreground" aria-label="This record" />}
                  {shownValue(field, draftAt(draft, field.pointer))}
                </dd>
              </div>
            ))}
          </dl>
        </div>
      ))}
    </section>
  )
}

/** "just now", "4 min ago", or the local date and time. */
function when(ms: number, now = Date.now()): string {
  const seconds = Math.round((now - ms) / 1000)
  if (seconds >= 0 && seconds < 60) return "just now"
  if (seconds >= 0 && seconds < 3600) return `${Math.round(seconds / 60)} min ago`
  return new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })
}

function Receipt({ action, objectTitle, technical, receipt, onView, onAgain, onClose }: {
  readonly action: ActionSummary; readonly objectTitle: string; readonly technical: boolean; readonly receipt: ActionReceipt
  readonly onView: ((objectId: string) => void) | undefined; readonly onAgain: (() => void) | undefined; readonly onClose: (() => void) | undefined
}) {
  const at = new Date(receipt.receipt.recordedAtMs)
  const verb = pastTense(action.title) ?? "saved"
  const sentence = `${objectTitle} ${receipt.objectId} ${verb} · ${when(receipt.receipt.recordedAtMs)}`
  return (
    <section aria-label="Receipt" className="grid gap-4">
      <div className="grid gap-3 rounded-node border border-node-border bg-card p-4 shadow-[var(--node-highlight),var(--elevation-node)]">
        <p role="status" className="flex items-start gap-2 text-sm font-medium">
          <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-[var(--tint-outcome)]" aria-hidden />
          <span>{sentence}{receipt.outcome === "replayed" && <span className="font-normal text-muted-foreground"> (already done: the cell returned the first receipt)</span>}</span>
        </p>
        {onView && <Button type="button" variant="outline" size="sm" className="justify-self-start border-node-border" onClick={() => onView(receipt.objectId)}>View record</Button>}
        <details open={technical} className="group text-sm">
          <summary className="cursor-pointer text-meta text-muted-foreground select-none hover:text-foreground">Details</summary>
          <dl className="mt-3 grid grid-cols-[minmax(7rem,auto)_1fr] gap-x-4 gap-y-2">
            <dt className="text-muted-foreground">Receipt</dt><dd className="min-w-0"><CopyId value={receipt.receipt.receiptHash} short={16} /></dd>
            <dt className="text-muted-foreground">Commit</dt><dd className="tabular-nums">{receipt.receipt.commitSeq}</dd>
            <dt className="text-muted-foreground">Recorded</dt><dd><time dateTime={at.toISOString()} title={at.toISOString()} className="whitespace-nowrap">{at.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "medium" })}</time></dd>
            <dt className="text-muted-foreground">Version</dt><dd className="tabular-nums">{receipt.version}</dd>
            <dt className="text-muted-foreground">Action</dt><dd><code className="text-xs">{action.actionId}</code></dd>
          </dl>
        </details>
      </div>
      <div className="flex flex-wrap gap-2">
        {onAgain && <Button type="button" variant="outline" className="border-node-border" onClick={onAgain}><Plus /> Another</Button>}
        <span className="flex-1" />
        {onClose && <Button type="button" onClick={onClose}>Done</Button>}
      </div>
    </section>
  )
}
