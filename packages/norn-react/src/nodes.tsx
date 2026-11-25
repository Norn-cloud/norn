/**
 * One page's component tree, drawn from the admitted Widget UX document (#1762 milestone C).
 *
 * Every node draws only what the document declares and the server answered: a read through
 * `matchReadState`, a call through `matchActionState`, a control enabled exactly when the current
 * machine state has a transition on one of its events (`machine/controls.ts`). A person's input
 * becomes the machine's declared `user` event; nothing here calls the server or decides permission.
 * Values carry the agent marks the server's projection implies (`agent.ts`, L6).
 */
import { AlertDialog, AlertDialogContent, AlertDialogTitle } from "@ui-sdk/react/alert-dialog"
import { Button } from "@ui-sdk/react/button"
import { Dialog, DialogContent, DialogTitle } from "@ui-sdk/react/dialog"
import { Input } from "@ui-sdk/react/input"
import type { ReactNode } from "react"
import { confirmMark, confirmReason, rowMark, valueMark, type ActionEffects, type MaskedProperty } from "./agent.ts"
import { primitiveOf } from "./capabilities.ts"
import { tierOf, type IslandRegistry, type IslandStates } from "./islands.ts"
import { enabled, eventsOf } from "./machine/controls.ts"
import { EngineNotReady } from "./machine/evaluate.ts"
import type { Fields, Machine, MachineEvent } from "./machine/machine.ts"
import { messageOf, type BindingCallState, type BindingReadState } from "./reads.ts"
import type { MachineRuntime } from "./runtime.ts"
import { matchActionState, matchReadState } from "./state.ts"
import { text, type PropertyRef, type WidgetNode, type WidgetPage, type WidgetUx } from "./ux.ts"

/** Everything a node needs from its page. */
export interface PageScope {
  readonly ux: WidgetUx
  readonly page: WidgetPage
  readonly machine: Machine
  readonly runtime: MachineRuntime
  readonly islands: IslandRegistry
  readonly locale: string
  readonly actionEffects: (actionId: string) => ActionEffects | undefined
  readonly navigate: (itemId: string) => void
}

const BUTTON = "inline-flex h-9 items-center justify-center gap-1.5 rounded-full border border-border bg-background px-4 text-body text-foreground outline-none transition-colors hover:bg-accent focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring disabled:opacity-45 aria-disabled:opacity-45 max-sm:min-h-[44px]"
const MUTED = "text-meta text-muted-foreground"

type Doc = Readonly<Record<string, unknown>>
const asDoc = (value: unknown): Doc | null => (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Doc) : null)

/** A value as the cell sent it: text (an exact number's text included) as is, anything else as JSON. */
const shown = (value: unknown): string => (value === null || value === undefined ? "—" : typeof value === "string" ? value : JSON.stringify(value))

/** The event of `node` that the current state can take now, for a person's input or a primitive's callback. */
function eventFor(scope: PageScope, node: WidgetNode, emits?: string): MachineEvent | undefined {
  const state = scope.runtime.snapshot.stateId
  return eventsOf(scope.machine, node.nodeId).find((event) =>
    (event.source.kind !== "user" || emits === undefined || event.source.emits === emits) &&
    scope.machine.transitions.some((transition) => transition.from === state && transition.on === event.eventId))
}

/** Raise `node`'s event, with the payload members the event declares. */
function raise(scope: PageScope, node: WidgetNode, payload: Fields = {}, emits?: string): void {
  const event = eventFor(scope, node, emits)
  if (event === undefined) return
  const declared = new Set((event.payload ?? []).map((member) => member.fieldId))
  const kept = Object.fromEntries(Object.entries(payload).filter(([member]) => declared.has(member)))
  scope.runtime.send({ kind: "user", eventId: event.eventId, payload: kept })
}

/** Whether `node` can act now; a guard the engine has not loaded for counts as not yet. */
function canAct(scope: PageScope, node: WidgetNode): { readonly on: boolean; readonly waiting: boolean } {
  try {
    return { on: enabled(scope.machine, scope.runtime.evaluate, scope.runtime.snapshot, node.nodeId), waiting: false }
  } catch (error) {
    if (error instanceof EngineNotReady) return { on: false, waiting: true }
    throw error
  }
}

/** A catalog node's props, each its ZEN expression over the page's fields; one waiting for the engine is left out. */
function propsOf(scope: PageScope, node: WidgetNode): Record<string, unknown> {
  if (node.component.kind !== "catalog") return {}
  const out: Record<string, unknown> = {}
  for (const [name, expression] of Object.entries(node.component.props ?? {})) {
    try {
      out[name] = scope.runtime.evaluate(expression, { fields: scope.runtime.snapshot.fields, event: {} })
    } catch (error) {
      if (!(error instanceof EngineNotReady)) throw error
    }
  }
  return out
}

/** One read, drawn through its closed state. */
function ReadView({ state, children }: { readonly state: BindingReadState | undefined; readonly children: (document: unknown, freshness: string) => ReactNode }) {
  if (state === undefined) return <p className={MUTED} data-read="idle">Not read yet.</p>
  return matchReadState(state, {
    pending: () => <p className={MUTED} data-read="pending" aria-busy="true">Reading…</p>,
    ok: ({ document, freshness }) => (
      <div data-read="ok" data-freshness={freshness}>
        {freshness === "stale" && <p className={MUTED} data-freshness-note="">No longer live: this is the last answer read.</p>}
        {children(document, freshness)}
      </div>
    ),
    refused: ({ refusal }) => <p role="status" className="text-meta text-destructive-text" data-read="refused" data-code={refusal.code}>{messageOf(refusal.document, `The cell refused this read (${refusal.code}).`)}</p>,
    not_served: ({ error }) => <p role="status" className={MUTED} data-read="not_served">{error.message}</p>,
    unavailable: ({ error }) => <p role="status" className={MUTED} data-read="unavailable">{error.message}</p>,
    answer_moved: ({ error }) => <p role="status" className={MUTED} data-read="answer_moved">{error.message}</p>,
    timed_out: ({ error }) => <p role="status" className={MUTED} data-read="timed_out">{error.message}</p>,
  })
}

/** One governed call's state, as a status line. */
function CallLine({ state }: { readonly state: BindingCallState | undefined }) {
  if (state === undefined) return null
  const line = (kind: string, said: string) => <p role="status" className={MUTED} data-call={kind}>{said}</p>
  return matchActionState(state, {
    editing: () => null,
    confirming: () => line("confirming", "Waiting for you to confirm."),
    sending: () => line("sending", "Sending…"),
    pending_approval: () => line("pending_approval", "Sent for approval: nothing is committed until it is decided."),
    committed: () => line("committed", "Saved."),
    refused: ({ refusal }) => line("refused", messageOf(refusal.document, `The cell refused this call (${refusal.code}).`)),
    not_served: () => line("not_served", "This workspace serves no cell to call: nothing was sent."),
    unavailable: () => line("unavailable", "The cell answered without applying the call: nothing committed."),
    timed_out: ({ message }) => line("timed_out", message ?? "The answer was lost; the call may have applied."),
    undoing: () => line("undoing", "Undoing…"),
    undo_failed: ({ message }) => line("undo_failed", message ?? "The undo did not come back."),
    undone: () => line("undone", "Undone."),
  })
}

/** One value with the mark the server's projection implies. */
function Value({ record, propertyId }: { readonly record: Doc; readonly propertyId: string }) {
  const masked = asDoc(record.masked)?.[propertyId] as MaskedProperty | undefined
  const withheld = record.withheld === true || (Array.isArray(record.withheldProperties) && record.withheldProperties.includes(propertyId))
  const mark = valueMark(masked, withheld)
  if ("data-agent-hidden" in mark) return <span {...mark} className={MUTED} data-value="withheld">Withheld</span>
  const values = asDoc(record.values) ?? {}
  return <span {...mark} data-value={masked === undefined ? "plain" : "masked"} data-mask={masked?.mode}>{shown(values[propertyId])}</span>
}

const fieldLabel = (scope: PageScope, ref: PropertyRef): string =>
  text(scope.ux.fields.find((field) => field.target.property?.bindingId === ref.bindingId && field.target.property.propertyId === ref.propertyId)?.label ?? { en: ref.propertyId }, scope.locale)

function ObjectFields({ scope, record, refs }: { readonly scope: PageScope; readonly record: Doc; readonly refs: readonly PropertyRef[] }) {
  return (
    <dl className="grid grid-cols-[minmax(0,10rem)_minmax(0,1fr)] gap-x-4 gap-y-1.5">
      {refs.map((ref) => (
        <div key={ref.propertyId} className="contents">
          <dt className={MUTED}>{fieldLabel(scope, ref)}</dt>
          <dd className="min-w-0 break-words text-body"><Value record={record} propertyId={ref.propertyId} /></dd>
        </div>
      ))}
    </dl>
  )
}

function ListRows({ scope, document, refs }: { readonly scope: PageScope; readonly document: unknown; readonly refs: readonly PropertyRef[] }) {
  const page = asDoc(document)
  const rows = (Array.isArray(page?.records) ? page.records : Array.isArray(page?.items) ? page.items : []).flatMap((row) => {
    const doc = asDoc(row)
    return doc === null ? [] : [doc]
  })
  if (rows.length === 0) return <p className={MUTED} data-rows="0">No records.</p>
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-start text-body" data-rows={rows.length}>
        <thead><tr>{refs.map((ref) => <th key={ref.propertyId} scope="col" className={`${MUTED} px-2 py-1.5 text-start font-normal`}>{fieldLabel(scope, ref)}</th>)}</tr></thead>
        <tbody>
          {rows.map((row, index) => (
            <tr key={typeof row.id === "string" ? row.id : index} {...rowMark(row.withheld === true)} className="border-t border-border">
              {refs.map((ref) => <td key={ref.propertyId} className="px-2 py-1.5"><Value record={row} propertyId={ref.propertyId} /></td>)}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

/** A person's control: a UI SDK button (trigger) or a text input, raising the node's declared events. */
function Control({ scope, node, label, placementId }: { readonly scope: PageScope; readonly node: WidgetNode; readonly label: string; readonly placementId: string | null }) {
  const { on, waiting } = canAct(scope, node)
  const mark = confirmMark(confirmReason(scope.ux, scope.machine, node.nodeId, placementId, scope.actionEffects, scope.locale))
  if (node.component.kind === "control" && node.component.input === "text") {
    const value = scope.runtime.snapshot.fields[node.nodeId]
    return (
      <label className="grid gap-1" data-node={node.nodeId}>
        <span className={MUTED}>{label}</span>
        <Input {...mark} className="h-9 rounded-[var(--radius)] border border-input bg-background px-3 text-body text-foreground" disabled={!on} data-engine={waiting ? "waiting" : undefined}
          defaultValue={typeof value === "string" ? value : ""} onValueChange={(next) => raise(scope, node, { value: next })} />
      </label>
    )
  }
  return (
    <Button {...mark} className={BUTTON} data-node={node.nodeId} disabled={!on} data-engine={waiting ? "waiting" : undefined} onClick={() => raise(scope, node)}>
      {label}
    </Button>
  )
}

/** A UI SDK layer held open in the machine states its node names (`heldIn`). */
function Layer({ scope, node, primitive }: { readonly scope: PageScope; readonly node: WidgetNode; readonly primitive: string }) {
  const heldIn = node.component.kind === "catalog" ? node.component.heldIn ?? [] : []
  const open = heldIn.includes(scope.runtime.snapshot.stateId)
  const props = propsOf(scope, node)
  const title = node.children.find((child) => child.component.kind === "section")
  const titled = title?.component.kind === "section" ? text(title.component.section.label, scope.locale) : scope.page.name
  const options = {
    open,
    ...(typeof props.dismissible === "boolean" ? { dismissible: props.dismissible } : {}),
    ...(typeof props.pendingAnnouncement === "string" ? { pendingAnnouncement: props.pendingAnnouncement } : {}),
    // A callback's typed value reaches the event as `value`; a close the machine does not take keeps the layer open.
    onOpenChange: (next: boolean, details: { readonly reason?: string }) => raise(scope, node, { value: next, reason: details.reason ?? "" }, "onOpenChange"),
    onDismissAttempt: (details: { readonly reason?: string }) => raise(scope, node, { reason: details.reason ?? "" }, "onDismissAttempt"),
  }
  const children = node.children.map((child) => <Node key={child.nodeId} scope={scope} node={child} />)
  const panel = "m-auto w-[min(28rem,calc(100vw-2rem))] rounded-[var(--radius-popup)] border border-border bg-popover p-5 text-popover-foreground shadow-lg backdrop:bg-black/40"
  return primitive === "alert-dialog" ? (
    <AlertDialog {...options}>
      <AlertDialogContent className={panel} data-node={node.nodeId} data-layer={primitive}>
        <AlertDialogTitle className="text-heading font-medium">{titled}</AlertDialogTitle>
        {children}
      </AlertDialogContent>
    </AlertDialog>
  ) : (
    <Dialog {...options}>
      <DialogContent className={panel} data-node={node.nodeId} data-layer={primitive}>
        <DialogTitle className="text-heading font-medium">{titled}</DialogTitle>
        {children}
      </DialogContent>
    </Dialog>
  )
}

/** A catalog island, given its props, its emit and, for Tier 2, the page's server states. */
function Island({ scope, node }: { readonly scope: PageScope; readonly node: WidgetNode }) {
  if (node.component.kind !== "catalog") return null
  const entryId = node.component.entry.entryId
  const entry = scope.ux.catalog?.find((candidate) => candidate.entryId === entryId)
  const island = scope.islands[entryId] as ((input: unknown) => ReactNode) | undefined
  const tier = entry === undefined ? null : tierOf(entry.trust)
  if (island === undefined || tier === null) return null
  const states: IslandStates = { reads: Object.fromEntries(scope.runtime.reads), calls: Object.fromEntries(scope.runtime.calls) }
  const input = {
    props: propsOf(scope, node),
    emit: (event: string, payload: Fields = {}) => raise(scope, node, payload, event),
    ...(tier === "stateful" ? { states } : {}),
  }
  return <div data-node={node.nodeId} data-island={entryId} data-tier={tier}>{island(input)}</div>
}

/** One node of the tree. */
export function Node({ scope, node }: { readonly scope: PageScope; readonly node: WidgetNode }): ReactNode {
  const children = node.children.map((child) => <Node key={child.nodeId} scope={scope} node={child} />)
  const component = node.component
  switch (component.kind) {
    case "section": {
      const label = text(component.section.label, scope.locale)
      const binding = component.section.fields[0]?.bindingId
      const read = binding === undefined ? undefined : scope.runtime.reads.get(binding)
      return (
        <section aria-label={label} data-node={node.nodeId} className="grid gap-3">
          <h2 className="text-heading font-medium">{label}</h2>
          {binding !== undefined && <ReadView state={read}>{(document) => <ObjectFields scope={scope} record={asDoc(document) ?? {}} refs={component.section.fields} />}</ReadView>}
          {children}
        </section>
      )
    }
    case "detail": {
      const refs = scope.ux.fields.flatMap((field) => (field.target.property?.bindingId === component.dataBindingId ? [field.target.property] : []))
      return (
        <div data-node={node.nodeId} className="grid gap-3">
          <ReadView state={scope.runtime.reads.get(component.dataBindingId)}>{(document) => <ObjectFields scope={scope} record={asDoc(document) ?? {}} refs={refs} />}</ReadView>
          {children}
        </div>
      )
    }
    case "list":
      return (
        <div data-node={node.nodeId} className="grid gap-3">
          <ReadView state={scope.runtime.reads.get(component.dataBindingId)}>{(document) => <ListRows scope={scope} document={document} refs={component.fields} />}</ReadView>
          {children}
        </div>
      )
    case "field": {
      const field = scope.ux.fields.find((candidate) => candidate.fieldId === component.fieldId)
      const ref = field?.target.property
      const read = ref === undefined ? undefined : scope.runtime.reads.get(ref.bindingId)
      return (
        <div data-node={node.nodeId} className="grid gap-1">
          <span className={MUTED}>{text(field?.label ?? { en: component.fieldId }, scope.locale)}</span>
          {ref !== undefined && read?.kind === "ok" && <Value record={asDoc(read.document) ?? {}} propertyId={ref.propertyId} />}
        </div>
      )
    }
    case "action": {
      const placement = scope.ux.actionPlacements.find((candidate) => candidate.placementId === component.placementId)
      return (
        <div data-node-group={node.nodeId} className="flex flex-wrap items-center gap-2">
          <Control scope={scope} node={node} label={text(placement?.label ?? { en: component.placementId }, scope.locale)} placementId={component.placementId} />
          <CallLine state={placement === undefined ? undefined : scope.runtime.calls.get(placement.bindingId)} />
        </div>
      )
    }
    case "control":
      return <Control scope={scope} node={node} label={text(component.label, scope.locale)} placementId={null} />
    case "navigation": {
      const item = scope.ux.navigationItems.find((candidate) => candidate.itemId === component.itemId)
      const label = text(item?.label ?? { en: component.itemId }, scope.locale)
      return eventsOf(scope.machine, node.nodeId).length > 0
        ? <Control scope={scope} node={node} label={label} placementId={null} />
        : <Button className={BUTTON} data-node={node.nodeId} onClick={() => scope.navigate(component.itemId)}>{label}</Button>
    }
    case "dialog":
      return <div role="group" aria-label={scope.page.name} data-node={node.nodeId} className="grid gap-3">{children}<CallLine state={component.bindingId === undefined ? undefined : scope.runtime.calls.get(component.bindingId)} /></div>
    case "catalog": {
      const primitive = primitiveOf(node)
      return primitive === null ? <Island scope={scope} node={node} /> : <Layer scope={scope} node={node} primitive={primitive} />
    }
    case "form":
    case "child_grid":
    case "composition":
      // Refused before the page draws (`missingCapabilities`): never drawn in part.
      return null
  }
}
