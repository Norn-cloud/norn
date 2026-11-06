import { Link } from "@tanstack/react-router"
import { AlertTriangle, CheckCircle2, Minus, Plus } from "lucide-react"
import { useId, useMemo, useState, type ReactNode } from "react"
import { CopyId } from "@/components/copy-id"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { cn } from "@/lib/utils"
import {
  ACTIONS_UNAVAILABLE, callAction, inputPointer, newIdempotencyKey, risky as isRisky, writesWhole, draftAt, emptyDraft, encodeObject, fieldsOf, groupsOf, problemsAt, sectionLabel, sectionsOf, unplaced, unsupported, withDraftAt,
  type ActionReceipt, type ActionSummary, type CallResult, type Diagnostic, type Draft, type FormField, type Spec,
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
export function ActionForm({ action, prefill = {}, onDirty, onClose, call = callAction }: {
  readonly action: ActionSummary
  /** Values the entry point already knows (the record's id for a transition); shown, not editable. */
  readonly prefill?: Readonly<Record<string, string>>
  readonly onDirty?: (dirty: boolean) => void
  readonly onClose?: () => void
  readonly call?: Call
}) {
  const fields = useMemo(() => fieldsOf(action.inputSchema), [action])
  const sections = useMemo(() => sectionsOf(fields, groupsOf(action.inputSchema)), [fields, action])
  const blocked = useMemo(() => unsupported(fields), [fields])
  const initial = useMemo(() => Object.entries(prefill).reduce<Draft>((draft, [name, value]) => withDraftAt(draft, `/${name}`, value), emptyDraft(fields)), [fields, prefill])
  const [draft, setDraft] = useState<Draft>(initial)
  const [key, setKey] = useState(newKey)
  const [phase, setPhase] = useState<Phase>({ kind: "editing" })
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial) && phase.kind !== "done"
  const diagnostics = phase.kind === "refused" ? phase.diagnostics : []
  const risky = isRisky(action)

  const edit = (pointer: string, value: Draft) => {
    const next = withDraftAt(draft, pointer, value)
    setDraft(next)
    setKey(newKey()) // a different call: a different key
    if (phase.kind === "failed" || phase.kind === "confirm") setPhase({ kind: "editing" })
    onDirty?.(JSON.stringify(next) !== JSON.stringify(initial))
  }

  const send = async () => {
    setPhase({ kind: "sending" })
    try {
      const result = await call(action.actionId, encodeObject(fields, draft as { readonly [name: string]: Draft }), key)
      if (result.kind === "receipt") { setPhase({ kind: "done", receipt: result.receipt }); onDirty?.(false) }
      else if (result.kind === "refused") setPhase({ kind: "refused", message: result.message, code: result.code, diagnostics: result.diagnostics })
      else setPhase({ kind: "failed", message: result.kind === "no-cell" ? "This workspace has no data cell yet." : ACTIONS_UNAVAILABLE })
    } catch (error) {
      setPhase({ kind: "failed", message: error instanceof Error ? error.message : "The cell did not answer; nothing was saved." })
    }
  }

  if (phase.kind === "done") {
    return <Receipt action={action} receipt={phase.receipt} onAgain={writesWhole(action) ? () => { setDraft(emptyDraft(fields)); setKey(newKey()); setPhase({ kind: "editing" }) } : undefined} onClose={onClose} />
  }

  const top = unplaced(diagnostics, fields)
  return (
    <form noValidate className="grid gap-5" aria-label={action.title} aria-busy={phase.kind === "sending"}
      onSubmit={(event) => { event.preventDefault(); if (phase.kind === "sending") return; if (risky && phase.kind !== "confirm") setPhase({ kind: "confirm" }); else void send() }}>
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
      {sections.map((section) => (
        <fieldset key={section.id ?? "rest"} className="grid gap-4 rounded-node border border-node-border p-4">
          {section.title !== null ? <legend className="px-1 text-sm font-medium">{section.title}</legend> : sections.length > 1 && <legend className="px-1 text-sm font-medium text-muted-foreground">More</legend>}
          {section.fields.map((field) => (
            <FieldRow key={field.pointer} field={field} label={sectionLabel(fields, field)} draft={draft} diagnostics={diagnostics}
              locked={prefill[field.name] !== undefined && field.pointer === `/${field.name}`} onEdit={edit} />
          ))}
        </fieldset>
      ))}
      {phase.kind === "confirm" && (
        <div role="alertdialog" aria-labelledby="confirm-title" className="grid gap-2 rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm">
          <p id="confirm-title" className="flex items-center gap-2 font-medium text-destructive"><AlertTriangle className="size-4" aria-hidden />
            {action.uiEffects.irreversible ? "This can't be undone." : "This removes business data."}</p>
          <p className="text-muted-foreground">{action.title}{action.uiEffects.destructive && action.uiEffects.irreversible ? ": it removes business data and can't be undone." : "."} The cell still checks it before anything changes.</p>
        </div>
      )}
      <div className="flex flex-wrap items-center gap-2">
        {dirty && phase.kind === "editing" && <span className="text-meta text-muted-foreground">Not sent yet</span>}
        <span className="flex-1" />
        {phase.kind === "confirm" && <Button type="button" variant="ghost" onClick={() => setPhase({ kind: "editing" })}>Back</Button>}
        <Button type="submit" variant={phase.kind === "confirm" ? "destructive" : "default"} disabled={blocked !== null || phase.kind === "sending"}>
          {phase.kind === "sending" ? "Sending…" : phase.kind === "confirm" ? `Yes, ${action.title.toLowerCase()}` : action.title}
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
        {label}{field.required && <span aria-hidden className="text-destructive">*</span>}
        {field.spec.kind === "decimal" && field.spec.unit && <span className="text-meta font-normal text-muted-foreground">({field.spec.unit})</span>}
      </label>
      {field.description && <p id={`${id}-help`} className="text-meta text-muted-foreground">{field.description}</p>}
      {locked
        ? <p className="flex items-center gap-2 text-sm"><code>{String(value ?? "")}</code><span className="text-meta text-muted-foreground">this record</span></p>
        : field.nullable
          ? <Nullable field={field} value={value ?? ""} onEdit={onEdit} widget={<Widget spec={field.spec} pointer={field.pointer} value={value ?? ""} root={draft} diagnostics={diagnostics} onEdit={onEdit} aria={aria} />} />
          : <Widget spec={field.spec} pointer={field.pointer} value={value ?? ""} root={draft} diagnostics={diagnostics} onEdit={onEdit} aria={aria} />}
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

function Widget({ spec, pointer, value, root, diagnostics, onEdit, aria }: {
  readonly spec: Spec; readonly pointer: string; readonly value: Draft; readonly root: Draft; readonly diagnostics: readonly Diagnostic[]
  readonly onEdit: (pointer: string, value: Draft) => void; readonly aria: Aria
}) {
  const text = typeof value === "string" ? value : ""
  switch (spec.kind) {
    case "text": return <Input {...aria} className="h-9" value={text} onChange={(e) => onEdit(pointer, e.target.value)} />
    case "integer":
    case "number":
    case "decimal":
      return <Input {...aria} className="h-9 tabular-nums" inputMode={spec.kind === "integer" ? "numeric" : "decimal"} autoComplete="off"
        placeholder={spec.kind === "decimal" && spec.scale !== null ? (spec.scale === 0 ? "0" : `0.${"0".repeat(spec.scale)}`) : undefined}
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
        <div className="grid gap-3 rounded-lg border border-node-border p-3">
          {spec.fields.map((child) => <FieldRow key={child.pointer} field={child} label={child.label} draft={root} diagnostics={diagnostics} locked={false} onEdit={onEdit} />)}
        </div>
      )
    case "array": {
      const items = Array.isArray(value) ? value as readonly Draft[] : []
      const blank: Draft = spec.item.kind === "object" ? emptyDraft(spec.item.fields) : ""
      return (
        <div className="grid gap-2">
          {items.map((item, index) => (
            <div key={index} className="flex items-start gap-2">
              <div className="min-w-0 flex-1">
                <Widget spec={spec.item.kind === "object" ? { ...spec.item, fields: rebase(spec.item.fields, `${pointer}/0`, `${pointer}/${index}`) } : spec.item}
                  pointer={`${pointer}/${index}`} value={item} root={root} diagnostics={diagnostics} onEdit={onEdit} aria={{ "aria-label": `Item ${index + 1}` }} />
              </div>
              <Button type="button" variant="ghost" size="icon" aria-label={`Remove item ${index + 1}`} onClick={() => onEdit(pointer, items.filter((_, i) => i !== index))}><Minus /></Button>
            </div>
          ))}
          <Button type="button" variant="outline" size="sm" className="justify-self-start" onClick={() => onEdit(pointer, [...items, blank])}><Plus /> Add item</Button>
        </div>
      )
    }
    case "unsupported": return <p className="text-meta text-muted-foreground">Not offered here: {spec.reason}</p>
  }
}

function Receipt({ action, receipt, onAgain, onClose }: { readonly action: ActionSummary; readonly receipt: ActionReceipt; readonly onAgain: (() => void) | undefined; readonly onClose: (() => void) | undefined }) {
  const at = new Date(receipt.receipt.recordedAtMs).toISOString()
  return (
    <section aria-label="Receipt" className="grid gap-4">
      <p role="status" className="flex items-center gap-2 text-sm font-medium">
        <CheckCircle2 className="size-4 text-[var(--tint-outcome)]" aria-hidden />
        {receipt.outcome === "replayed" ? `${action.title}: already done; the cell returned the first receipt.` : `${action.title}: done.`}
      </p>
      <dl className="grid grid-cols-[minmax(8rem,auto)_1fr] gap-x-4 gap-y-2 rounded-node border border-node-border p-4 text-sm">
        <dt className="text-muted-foreground">Record</dt><dd><Link to="/sources/$concept/$" params={{ concept: receipt.objectType, _splat: receipt.objectId }} className="underline-offset-4 hover:underline">{receipt.objectId}</Link></dd>
        <dt className="text-muted-foreground">Version</dt><dd className="tabular-nums">{receipt.version}</dd>
        <dt className="text-muted-foreground">Receipt</dt><dd className="min-w-0"><CopyId value={receipt.receipt.receiptHash} short={16} /></dd>
        <dt className="text-muted-foreground">Commit</dt><dd className="tabular-nums">{receipt.receipt.commitSeq}</dd>
        <dt className="text-muted-foreground">Recorded</dt><dd><time dateTime={at} className="font-mono text-xs">{at}</time></dd>
      </dl>
      <div className={cn("flex flex-wrap gap-2")}>
        {onAgain && <Button type="button" variant="outline" onClick={onAgain}><Plus /> Another</Button>}
        <span className="flex-1" />
        {onClose && <Button type="button" onClick={onClose}>Done</Button>}
      </div>
    </section>
  )
}
