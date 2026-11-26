/**
 * One page's component tree, drawn from the admitted Widget UX document (#1762 milestone C).
 *
 * Every node draws only what the document declares and the server answered: a read through
 * `matchReadState`, a call through `matchActionState`, a control enabled exactly when the current
 * machine state has a transition on one of its events (`machine/controls.ts`). A person's input
 * becomes the machine's declared `user` event; nothing here calls the server or decides permission.
 * Values carry the agent marks the server's projection implies (`agent.ts`, L6).
 */
import { AlertDialog, AlertDialogBackdrop, AlertDialogContent, AlertDialogDescription, AlertDialogTitle } from "@ui-sdk/react/alert-dialog"
import { Button } from "@ui-sdk/react/button"
import { Checkbox, CheckboxIndicator } from "@ui-sdk/react/checkbox"
import { Dialog, DialogBackdrop, DialogClose, DialogContent, DialogDescription, DialogTitle } from "@ui-sdk/react/dialog"
import { Field, FieldDescription, FieldError, FieldLabel } from "@ui-sdk/react/field"
import { Input } from "@ui-sdk/react/input"
import { Select, SelectContent, SelectEmpty, SelectItem, SelectItemText, SelectLabel, SelectTrigger, SelectValue } from "@ui-sdk/react/select"
import { Tabs, TabsList, TabsPanel, TabsTab } from "@ui-sdk/react/tabs"
import { Textarea } from "@ui-sdk/react/textarea"
import { useEffect, useId, useLayoutEffect, useRef, useState, type ComponentType, type FormEvent, type KeyboardEvent, type ReactNode } from "react"
import { confirmMark, confirmReason, formConfirmReason, rowMark, valueMark, type ActionEffects, type AgentAttributes, type MaskedProperty } from "./agent.ts"
import { primitiveOf } from "./capabilities.ts"
import { tierOf, type IslandRegistry, type IslandStates } from "./islands.ts"
import { enabled, eventsOf } from "./machine/controls.ts"
import { EngineNotReady } from "./machine/evaluate.ts"
import type { Fields, Machine, MachineEvent } from "./machine/machine.ts"
import { messageOf, type BindingCallState, type BindingReadState } from "./reads.ts"
import type { MachineRuntime, PinnedRecord } from "./runtime.ts"
import { matchActionState, matchReadState } from "./state.ts"
import {
  nodesOf,
  text,
  type ActionConfirmation,
  type Component,
  type DismissalMethod,
  type FieldFormat,
  type FieldPresentation,
  type PropertyRef,
  type Translate,
  type WidgetForm,
  type WidgetNode,
  type WidgetPage,
  type WidgetUx,
  type ZenCondition,
} from "./ux.ts"

/** Everything a node needs from its page. */
export interface PageScope {
  readonly ux: WidgetUx
  readonly page: WidgetPage
  readonly machine: Machine
  readonly runtime: MachineRuntime
  readonly islands: IslandRegistry
  readonly locale: string
  /** Text direction for RTL-safe logical navigation. */
  readonly dir?: "ltr" | "rtl"
  /** Page context parameters supplied by the host route. */
  readonly context?: Readonly<Record<string, unknown>>
  /** The host's translations of the page's messages; a message without one draws its source text. */
  readonly translate?: Translate | undefined
  readonly actionEffects: (actionId: string) => ActionEffects | undefined
  readonly navigate: (itemId: string) => void
  /** The node whose event a person or agent raised last: where focus returns when a layer it opened closes. */
  readonly raisedBy: { current: string | null }
}

const BUTTON = "inline-flex h-9 items-center justify-center gap-1.5 rounded-full border border-border bg-background px-4 text-body text-foreground outline-none transition-colors hover:bg-accent focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring disabled:opacity-45 aria-disabled:opacity-45 max-sm:min-h-[44px]"
const INPUT = "h-9 w-full rounded-[var(--radius)] border border-input bg-background px-3 text-body text-foreground outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring disabled:opacity-50 max-sm:min-h-[44px]"
const TEXTAREA = "min-h-20 w-full rounded-[var(--radius)] border border-input bg-background px-3 py-2 text-body text-foreground outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring disabled:opacity-50"
const MUTED = "text-meta text-muted-foreground"
const DIALOG_PANEL = "fixed start-1/2 top-1/2 z-50 grid w-[min(28rem,calc(100vw-2rem))] -translate-y-1/2 gap-3 rounded-[var(--radius-popup)] border border-border bg-popover p-5 text-popover-foreground shadow-lg ltr:-translate-x-1/2 rtl:translate-x-1/2"
// No exit animation runs, so the backdrop never outlives the layer to swallow a press.
const DIALOG_BACKDROP = "fixed inset-0 z-40 bg-black/40"
const ROW_FOCUS = "outline-none focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-ring"

type Doc = Readonly<Record<string, unknown>>
const asDoc = (value: unknown): Doc | null => (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Doc) : null)
const own = (doc: Doc | null, key: string): unknown => (doc !== null && Object.hasOwn(doc, key) ? doc[key] : undefined)
const useIsomorphicLayoutEffect = typeof document !== "undefined" ? useLayoutEffect : useEffect
const escapeSelector = (value: string): string =>
  typeof CSS !== "undefined" && typeof CSS.escape === "function"
    ? CSS.escape(value)
    : value.replace(/["\\\]\[]/g, "\\$&")
const safeDomId = (value: string): string => value.replace(/[^A-Za-z0-9_-]/g, "_")

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
function raise(scope: PageScope, node: WidgetNode, payload: Fields = {}, emits?: string): boolean {
  const event = eventFor(scope, node, emits)
  if (event === undefined) return false
  const declared = new Set((event.payload ?? []).map((member) => member.fieldId))
  const kept = Object.fromEntries(Object.entries(payload).filter(([member]) => declared.has(member)))
  scope.raisedBy.current = node.nodeId
  scope.runtime.send({ kind: "user", eventId: event.eventId, payload: kept })
  return true
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

/**
 * A catalog node's props, each its ZEN expression over the page's fields. `waiting` names the props
 * whose expressions wait for the engine: they are left out, and an island is not drawn until none is.
 */
function propsOf(scope: PageScope, node: WidgetNode): { readonly props: Record<string, unknown>; readonly waiting: readonly string[] } {
  if (node.component.kind !== "catalog") return { props: {}, waiting: [] }
  const props: Record<string, unknown> = {}
  const waiting: string[] = []
  for (const [name, expression] of Object.entries(node.component.props ?? {})) {
    try {
      props[name] = scope.runtime.evaluate(expression, { fields: scope.runtime.snapshot.fields, event: {} })
    } catch (error) {
      if (!(error instanceof EngineNotReady)) throw error
      waiting.push(name)
    }
  }
  return { props, waiting }
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

/** Human-readable label for a server `RefusalCategory` (`norn/refusal@2`). */
export function formatRefusalCategory(category: string | null): string {
  switch (category) {
    case "visibility": return "Not visible"
    case "permission": return "Permission refused"
    case "precondition": return "Precondition failed"
    case "conflict": return "Conflict"
    case "invalid": return "Invalid input"
    case "limit": return "Limit reached"
    case "internal": return "Server error"
    default: return "Refused"
  }
}

/** One governed call's state, as a status line. */
function CallLine({ state }: { readonly state: BindingCallState | undefined }) {
  if (state === undefined) return null
  const line = (kind: string, said: string) => <p role="status" className={MUTED} data-call={kind}>{said}</p>
  return matchActionState(state, {
    editing: () => null,
    confirming: () => line("confirming", "Waiting for you to confirm."),
    sending: () => <p role="status" aria-busy="true" className={MUTED} data-call="sending">Sending…</p>,
    pending_approval: () => line("pending_approval", "Sent for approval: nothing is committed until it is decided."),
    committed: () => line("committed", "Saved."),
    refused: ({ refusal }) => (
      <p
        role="status"
        className="text-meta text-destructive-text"
        data-call="refused"
        data-category={refusal.category ?? undefined}
        data-code={refusal.code || undefined}
      >
        {`${formatRefusalCategory(refusal.category)}: ${messageOf(refusal.document, `The cell refused this call (${refusal.code}).`)}`}
      </p>
    ),
    not_served: () => line("not_served", "This workspace serves no cell to call: nothing was sent."),
    unavailable: () => line("unavailable", "The cell answered without applying the call: nothing committed."),
    timed_out: ({ message }) => line("timed_out", message ?? "The answer was lost; the call may have applied."),
    undoing: () => <p role="status" aria-busy="true" className={MUTED} data-call="undoing">Undoing…</p>,
    undo_failed: ({ message }) => line("undo_failed", message ?? "The undo did not come back."),
    undone: () => line("undone", "Undone."),
  })
}

/** One value with the mark the server's projection implies. */
function Value({ record, propertyId }: { readonly record: Doc; readonly propertyId: string }) {
  const masked = own(asDoc(record.masked), propertyId) as MaskedProperty | undefined
  const withheld = record.withheld === true || (Array.isArray(record.withheldProperties) && record.withheldProperties.includes(propertyId))
  const mark = valueMark(masked, withheld)
  if ("data-agent-hidden" in mark) return <span {...mark} className={MUTED} data-value="withheld">Withheld</span>
  const values = asDoc(record.values)
  return <span {...mark} data-value={masked === undefined ? "plain" : "masked"} data-mask={masked?.mode}>{shown(own(values, propertyId))}</span>
}

const propertyOf = (field: FieldPresentation | undefined): PropertyRef | undefined =>
  field !== undefined && "property" in field.target ? field.target.property : undefined

const fieldLabel = (scope: PageScope, ref: PropertyRef): string =>
  text(scope.ux.fields.find((field) => {
    const prop = propertyOf(field)
    return prop?.bindingId === ref.bindingId && prop.propertyId === ref.propertyId
  })?.label ?? { en: ref.propertyId }, scope.locale, scope.translate)

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

// ---- Tabs (WidgetSectionKind::Tab) --------------------------------------------------------------

type TabNode = WidgetNode & { readonly component: Extract<Component, { readonly kind: "section" }> }

const isTabSection = (node: WidgetNode): node is TabNode =>
  node.component.kind === "section" && node.component.section.kind === "tab"

export type ChildSegment =
  | { readonly kind: "node"; readonly node: WidgetNode }
  | { readonly kind: "tabs"; readonly nodes: readonly TabNode[] }

/** Partition sibling children into contiguous tab runs (sorted by `section.order`) and single nodes. */
export function partitionTabRuns(children: readonly WidgetNode[]): ChildSegment[] {
  const segments: ChildSegment[] = []
  let currentTabs: TabNode[] = []
  const flushTabs = () => {
    if (currentTabs.length === 0) return
    const ordered = [...currentTabs].sort((left, right) => left.component.section.order - right.component.section.order)
    segments.push({ kind: "tabs", nodes: ordered })
    currentTabs = []
  }
  for (const child of children) {
    if (isTabSection(child)) currentTabs.push(child)
    else {
      flushTabs()
      segments.push({ kind: "node", node: child })
    }
  }
  flushTabs()
  return segments
}

/** Logical RTL-safe roving target index for horizontal WAI-ARIA tabs (`ArrowLeft`/`ArrowRight`/`Home`/`End`). */
export function nextTabIndex(current: number, key: string, count: number, dir: "ltr" | "rtl" = "ltr"): number | null {
  if (count <= 0) return null
  const forward = dir === "rtl" ? "ArrowLeft" : "ArrowRight"
  const backward = dir === "rtl" ? "ArrowRight" : "ArrowLeft"
  if (key === forward) return (current + 1) % count
  if (key === backward) return (current - 1 + count) % count
  if (key === "Home") return 0
  if (key === "End") return count - 1
  return null
}

function TabSectionGroup({ scope, tabs }: { readonly scope: PageScope; readonly tabs: readonly TabNode[] }) {
  const groupId = useId()
  const [selectedNodeId, setSelectedNodeId] = useState<string>(() => tabs[0]?.nodeId ?? "")
  const activeNodeId = tabs.some((tab) => tab.nodeId === selectedNodeId) ? selectedNodeId : (tabs[0]?.nodeId ?? "")
  const dir = scope.dir ?? "ltr"
  const tabRefs = useRef<Map<string, HTMLButtonElement | null>>(new Map())

  const activate = (nodeId: string) => {
    setSelectedNodeId(nodeId)
    const tab = tabs.find((candidate) => candidate.nodeId === nodeId)
    if (tab !== undefined) {
      raise(scope, tab, { sectionId: tab.component.section.sectionId, value: tab.component.section.sectionId })
    }
  }

  const onTabListKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return
    const focusedId = (event.target instanceof Element ? event.target.getAttribute("data-tab-node") : null) ?? activeNodeId
    const currentIndex = tabs.findIndex((tab) => tab.nodeId === focusedId)
    if (currentIndex === -1) return
    const targetIndex = nextTabIndex(currentIndex, event.key, tabs.length, dir)
    if (targetIndex === null) return
    event.preventDefault()
    const target = tabs[targetIndex]!
    activate(target.nodeId)
    tabRefs.current.get(target.nodeId)?.focus()
  }

  return (
    <Tabs
      value={activeNodeId}
      onValueChange={(next) => {
        if (typeof next === "string" && next !== activeNodeId) activate(next)
      }}
      orientation="horizontal"
      loop
      dir={dir}
      style={dir === "rtl" ? { direction: "rtl" } : undefined}
      className="grid gap-3"
      data-tab-group=""
    >
      <TabsList
        className="inline-flex w-fit flex-wrap items-center gap-1 border-b border-border"
        aria-orientation="horizontal"
        onKeyDown={onTabListKeyDown}
      >
        {tabs.map((tab) => {
          const safe = safeDomId(tab.nodeId)
          const tabDomId = `${groupId}-tab-${safe}`
          const panelDomId = `${groupId}-panel-${safe}`
          const label = text(tab.component.section.label, scope.locale, scope.translate)
          const tabConfirm = confirmMark(confirmReason(scope.ux, scope.machine, tab.nodeId, null, scope.actionEffects, scope.locale, scope.translate))
          return (
            <TabsTab
              {...tabConfirm}
              key={tab.nodeId}
              value={tab.nodeId}
              id={tabDomId}
              aria-controls={panelDomId}
              data-tab-node={tab.nodeId}
              ref={(el: HTMLButtonElement | null) => {
                tabRefs.current.set(tab.nodeId, el)
              }}
              className="relative inline-flex h-9 items-center justify-center rounded-t-[var(--radius)] border-b-2 border-transparent px-3 text-body text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring data-[selected]:border-foreground data-[selected]:font-medium data-[selected]:text-foreground max-sm:min-h-[44px]"
            >
              {label}
            </TabsTab>
          )
        })}
      </TabsList>
      {tabs.map((tab) => {
        const safe = safeDomId(tab.nodeId)
        const tabDomId = `${groupId}-tab-${safe}`
        const panelDomId = `${groupId}-panel-${safe}`
        const isActive = tab.nodeId === activeNodeId
        const binding = tab.component.section.fields[0]?.bindingId
        const read = binding === undefined ? undefined : scope.runtime.reads.get(binding)
        return (
          <TabsPanel
            key={tab.nodeId}
            value={tab.nodeId}
            id={panelDomId}
            aria-labelledby={tabDomId}
            keepMounted
            data-node={tab.nodeId}
            className="grid gap-3 outline-none"
          >
            {isActive && (
              <>
                {binding !== undefined && (
                  <ReadView state={read}>
                    {(document) => <ObjectFields scope={scope} record={asDoc(document) ?? {}} refs={tab.component.section.fields} />}
                  </ReadView>
                )}
                {renderChildren(scope, tab.children)}
              </>
            )}
          </TabsPanel>
        )
      })}
    </Tabs>
  )
}

function renderChildren(scope: PageScope, children: readonly WidgetNode[]): ReactNode {
  return partitionTabRuns(children).map((segment) =>
    segment.kind === "tabs"
      ? <TabSectionGroup key={`tabs:${segment.nodes.map((node) => node.nodeId).join(",")}`} scope={scope} tabs={segment.nodes} />
      : <Node key={segment.node.nodeId} scope={scope} node={segment.node} />,
  )
}

// ---- Selectable Lists (WidgetComponent::List) ---------------------------------------------------

/** Extract a stable row key from a projected record. */
export function rowIdOf(row: Doc, index: number): string {
  if (typeof row.id === "string" && row.id !== "") return row.id
  if (typeof row.key === "string" && row.key !== "") return row.key
  const values = asDoc(row.values)
  if (typeof values?.id === "string" && values.id !== "") return values.id
  return String(index)
}

/** Next selected row id set for `"single"` or `"multiple"` list selection. */
export function nextSelection(mode: "single" | "multiple", current: readonly string[], rowId: string): string[] {
  if (mode === "single") return current.length === 1 && current[0] === rowId ? [] : [rowId]
  return current.includes(rowId) ? current.filter((candidate) => candidate !== rowId) : [...current, rowId]
}

/** Vertical keyboard navigation index for selectable list rows (`ArrowDown`/`ArrowUp`/`Home`/`End`). */
export function nextListRowIndex(current: number, key: string, count: number): number | null {
  if (count <= 0) return null
  if (key === "ArrowDown") return Math.min(count - 1, current + 1)
  if (key === "ArrowUp") return Math.max(0, current - 1)
  if (key === "Home") return 0
  if (key === "End") return count - 1
  return null
}

/** Build the payload for a list selection `user` event, filtered strictly to the event's declared payload fields. */
export function selectionEventPayload(
  event: MachineEvent | undefined,
  mode: "single" | "multiple",
  rowId: string,
  nextIds: readonly string[],
  selectedRows: readonly Doc[],
): Fields {
  if (event === undefined) return {}
  const selectedNow = nextIds.includes(rowId)
  const firstId = nextIds[0] ?? null
  const out: Record<string, unknown> = {}
  for (const field of event.payload ?? []) {
    switch (field.fieldId) {
      case "id":
      case "key":
      case "recordId":
        out[field.fieldId] = firstId ?? (field.required && field.type === "text" ? "" : null)
        break
      case "rowId":
        out[field.fieldId] = rowId
        break
      case "ids":
      case "selectedIds":
        out[field.fieldId] = field.type === "text" ? nextIds.join(",") : [...nextIds]
        break
      case "selected":
      case "selection":
        if (field.type === "boolean") out[field.fieldId] = selectedNow
        else if (field.type === "text") out[field.fieldId] = mode === "single" ? (firstId ?? "") : nextIds.join(",")
        else out[field.fieldId] = [...nextIds]
        break
      case "value":
        out[field.fieldId] = mode === "single"
          ? (firstId ?? (field.required && field.type === "text" ? "" : null))
          : (field.type === "text" ? nextIds.join(",") : [...nextIds])
        break
      case "record":
        out[field.fieldId] = mode === "single" ? (selectedRows[0] ?? null) : [...selectedRows]
        break
      case "count":
        out[field.fieldId] = nextIds.length
        break
      default:
        break
    }
  }
  return out
}

/** Flatten a projected list row into the top-level entity property shape expected by `input.selection` (`types.rs`). */
export function flattenSelectionRecord(row: Doc): Record<string, unknown> {
  const values = asDoc(row.values)
  const out: Record<string, unknown> = {}
  if (values !== null) {
    Object.assign(out, values)
  } else {
    for (const [key, value] of Object.entries(row)) {
      if (key === "values" || key === "masked" || key === "withheld" || key === "withheldProperties") continue
      out[key] = value
    }
  }
  if (typeof row.id === "string" && row.id !== "" && !Object.hasOwn(out, "id")) out.id = row.id
  return out
}

/** Reconcile selected row ids against a refreshed list snapshot, pruning removed or withheld rows. */
export function reconcileSelection(
  selectedIds: readonly string[],
  rows: readonly Doc[],
): { readonly ids: string[]; readonly records: Record<string, unknown>[]; readonly changed: boolean } {
  const available = new Map<string, Doc>()
  for (let index = 0; index < rows.length; index += 1) {
    const row = rows[index]!
    if (row.withheld === true) continue
    available.set(rowIdOf(row, index), row)
  }
  const ids: string[] = []
  const records: Record<string, unknown>[] = []
  for (const id of selectedIds) {
    const row = available.get(id)
    if (row !== undefined) {
      ids.push(id)
      records.push(flattenSelectionRecord(row))
    }
  }
  const changed = ids.length !== selectedIds.length || ids.some((id, index) => id !== selectedIds[index])
  return { ids, records, changed }
}

function ListRows({
  scope,
  node,
  document,
  refs,
  dataBindingId,
  selection,
}: {
  readonly scope: PageScope
  readonly node: WidgetNode
  readonly document: unknown
  readonly refs: readonly PropertyRef[]
  readonly dataBindingId: string
  readonly selection: "none" | "single" | "multiple"
}) {
  const page = asDoc(document)
  const rows = (Array.isArray(page?.records) ? page.records : Array.isArray(page?.items) ? page.items : []).flatMap((row) => {
    const doc = asDoc(row)
    return doc === null ? [] : [doc]
  })
  const [selectedIds, setSelectedIds] = useState<readonly string[]>([])
  const [focusIndex, setFocusIndex] = useState<number>(0)
  const rowRefs = useRef<Array<HTMLTableRowElement | null>>([])

  const reconciled = selection === "none" ? { ids: [] as string[], records: [] as Record<string, unknown>[], changed: false } : reconcileSelection(selectedIds, rows)
  const reconciledKey = selection === "none" ? "" : JSON.stringify(reconciled.records)

  // Reconcile runtime selection immediately when a refresh removes or withholds selected rows.
  useEffect(() => {
    if (selection === "none") return
    if (reconciled.changed) setSelectedIds(reconciled.ids)
    scope.runtime.setSelection?.(dataBindingId, reconciled.records)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selection, dataBindingId, reconciledKey, reconciled.changed])

  if (rows.length === 0) return <p className={MUTED} data-rows="0">No records.</p>

  if (selection === "none") {
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

  const rowIds = rows.map((row, index) => rowIdOf(row, index))
  const activeSelected = reconciled.ids
  const clampedFocus = Math.min(Math.max(0, focusIndex), rows.length - 1)
  const listConfirmMark = confirmMark(confirmReason(scope.ux, scope.machine, node.nodeId, null, scope.actionEffects, scope.locale, scope.translate))

  const toggleRow = (index: number) => {
    const row = rows[index]
    if (row === undefined || row.withheld === true) return
    const rowId = rowIds[index]!
    const next = nextSelection(selection, activeSelected, rowId)
    setSelectedIds(next)
    setFocusIndex(index)
    const nextReconciled = reconcileSelection(next, rows)
    scope.runtime.setSelection?.(dataBindingId, nextReconciled.records)
    const event = eventFor(scope, node)
    if (event !== undefined) {
      const payload = selectionEventPayload(event, selection, rowId, nextReconciled.ids, nextReconciled.records)
      raise(scope, node, payload)
    }
  }

  const onRowKeyDown = (index: number, event: KeyboardEvent<HTMLTableRowElement>) => {
    if (event.altKey || event.ctrlKey || event.metaKey) return
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault()
      toggleRow(index)
      return
    }
    const target = nextListRowIndex(index, event.key, rows.length)
    if (target === null || target === index) return
    event.preventDefault()
    setFocusIndex(target)
    rowRefs.current[target]?.focus()
  }

  return (
    <div className="overflow-x-auto">
      <table
        className="w-full text-start text-body"
        data-rows={rows.length}
        data-selection={selection}
        aria-multiselectable={selection === "multiple" ? "true" : undefined}
      >
        <thead>
          <tr>
            <th scope="col" className="w-9 px-2 py-1.5 text-start font-normal">
              <span className="sr-only">Select</span>
            </th>
            {refs.map((ref) => (
              <th key={ref.propertyId} scope="col" className={`${MUTED} px-2 py-1.5 text-start font-normal`}>{fieldLabel(scope, ref)}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, index) => {
            const rowId = rowIds[index]!
            const withheld = row.withheld === true
            const isSelected = !withheld && activeSelected.includes(rowId)
            const indicator = selection === "single" ? "radio" : "checkbox"
            return (
              <tr
                key={rowId}
                ref={(el) => {
                  rowRefs.current[index] = el
                }}
                {...rowMark(withheld)}
                {...(withheld ? {} : listConfirmMark)}
                data-row-id={rowId}
                data-selected={isSelected ? "" : undefined}
                aria-selected={isSelected ? "true" : "false"}
                aria-disabled={withheld ? "true" : undefined}
                tabIndex={clampedFocus === index ? 0 : -1}
                onClick={() => toggleRow(index)}
                onKeyDown={(event) => onRowKeyDown(index, event)}
                className={`border-t border-border transition-colors ${withheld ? "cursor-not-allowed opacity-55" : "cursor-pointer hover:bg-accent/50 data-[selected]:bg-accent"} ${ROW_FOCUS}`}
              >
                <td className="w-9 px-2 py-1.5">
                  <span
                    aria-hidden="true"
                    data-indicator={indicator}
                    data-checked={isSelected ? "" : undefined}
                    className={
                      selection === "single"
                        ? "inline-flex size-4 items-center justify-center rounded-full border border-input bg-background align-middle data-[checked]:border-foreground"
                        : "inline-flex size-4 items-center justify-center rounded-[3px] border border-input bg-background text-[11px] leading-none text-background align-middle data-[checked]:border-foreground data-[checked]:bg-foreground"
                    }
                  >
                    {isSelected && (selection === "single" ? <span className="size-2 rounded-full bg-foreground" /> : "✓")}
                  </span>
                </td>
                {refs.map((ref) => <td key={ref.propertyId} className="px-2 py-1.5"><Value record={row} propertyId={ref.propertyId} /></td>)}
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}

/** A person's control: a UI SDK button (trigger) or a text input, raising the node's declared events. */
function Control({ scope, node, label, placementId }: { readonly scope: PageScope; readonly node: WidgetNode; readonly label: string; readonly placementId: string | null }) {
  const { on, waiting } = canAct(scope, node)
  const mark = confirmMark(confirmReason(scope.ux, scope.machine, node.nodeId, placementId, scope.actionEffects, scope.locale, scope.translate))
  if (node.component.kind === "control" && node.component.input === "text") {
    const value = Object.hasOwn(scope.runtime.snapshot.fields, node.nodeId) ? scope.runtime.snapshot.fields[node.nodeId] : undefined
    return (
      <label className="grid gap-1" data-node={node.nodeId}>
        <span className={MUTED}>{label}</span>
        <Input {...mark} className={INPUT} disabled={!on} data-engine={waiting ? "waiting" : undefined}
          defaultValue={typeof value === "string" ? value : ""} onValueChange={(next) => raise(scope, node, { value: next })} />
      </label>
    )
  }
  return (
    // Focusable while disabled: a layer whose controls all wait on a call keeps focus inside it.
    <Button {...mark} className={BUTTON} data-node={node.nodeId} disabled={!on} focusableWhenDisabled data-engine={waiting ? "waiting" : undefined} onClick={() => raise(scope, node)}>
      {label}
    </Button>
  )
}

// ---- Dialog (WidgetComponent::Dialog) -----------------------------------------------------------

type DialogComponent = Extract<Component, { readonly kind: "dialog" }>

/** Whether a `component:dialog` is currently L1-blocked because its bound call is pending. */
export function isDialogBlocked(scope: PageScope, component: DialogComponent): boolean {
  if (!component.blocking || component.bindingId === undefined) return false
  const call = scope.runtime.calls.get(component.bindingId)
  if (call?.kind === "sending" || call?.kind === "undoing") return true
  const current = scope.machine.states.find((state) => state.stateId === scope.runtime.snapshot.stateId)
  return current?.role === "pending" && current.awaits?.kind === "action" && current.awaits.bindingId === component.bindingId
}

/**
 * Whether a dismissal `reason` is allowed for a `component:dialog` given its L1 `blocked` state
 * and declared `dismissalMethods`. Fails closed: while `blocked`, every dismissal is refused.
 */
export function canDismissDialog(blocked: boolean, dismissalMethods: readonly DismissalMethod[], reason: string): boolean {
  if (blocked) return false
  switch (reason) {
    case "escape-key": return dismissalMethods.includes("escape")
    case "outside-press": return dismissalMethods.includes("scrim")
    case "swipe": return dismissalMethods.includes("swipe")
    case "close-press": return dismissalMethods.includes("visible_close")
    case "back": return dismissalMethods.includes("escape") || dismissalMethods.includes("swipe")
    default: return false
  }
}

function DialogNode({ scope, node, component }: { readonly scope: PageScope; readonly node: WidgetNode; readonly component: DialogComponent }) {
  const blocked = isDialogBlocked(scope, component)
  const methods: readonly DismissalMethod[] = component.dismissalMethods ?? []
  const [dismissed, setDismissed] = useState(false)
  useEffect(() => {
    if (blocked) setDismissed(false)
  }, [blocked])
  const open = blocked || !dismissed
  const titleSection = node.children.find((child) => child.component.kind === "section" && child.component.section.kind === "section")
  const titled = titleSection?.component.kind === "section"
    ? text(titleSection.component.section.label, scope.locale, scope.translate)
    : scope.page.name
  const asked = nodesOf(node).flatMap((child) => (child.component.kind === "action" ? [child.component.placementId] : []))
  const message =
    scope.ux.actionPlacements.find((placement) =>
      asked.includes(placement.placementId) || (component.bindingId !== undefined && placement.bindingId === component.bindingId),
    )?.confirmation.message

  // Focus restore: remember what had focus when the dialog opened.
  const wasOpen = useRef(false)
  const returnTo = useRef<HTMLElement | null>(null)
  useIsomorphicLayoutEffect(() => {
    if (open && !wasOpen.current && typeof document !== "undefined") {
      const candidate = document.activeElement instanceof HTMLElement && document.activeElement !== document.body ? document.activeElement : null
      const active = candidate !== null && candidate.closest("[data-layer],[data-dialog]") === null ? candidate : null
      const raised = scope.raisedBy.current
      returnTo.current = active ?? (raised === null ? null : document.querySelector<HTMLElement>(`[data-node="${escapeSelector(raised)}"]`))
    }
    wasOpen.current = open
  }, [open, scope.raisedBy])

  const allowsBackdrop = !blocked && methods.includes("scrim")
  const allowsDismiss = !blocked && (methods.includes("escape") || methods.includes("scrim") || methods.includes("swipe"))
  const allowsVisibleClose = methods.includes("visible_close")
  const bodyChildren = node.children.flatMap((child) => (child === titleSection ? child.children : [child]))
  const callState = component.bindingId === undefined ? undefined : scope.runtime.calls.get(component.bindingId)

  return (
    <Dialog
      open={open}
      modal={true}
      dismissible={allowsDismiss}
      closeOnOutsidePress={allowsBackdrop}
      onOpenChange={(next, details) => {
        if (!next) {
          const reason = details.reason ?? ""
          if (!canDismissDialog(blocked, methods, reason)) {
            details.cancel()
            raise(scope, node, { reason }, "onDismissAttempt")
            return
          }
          if (!raise(scope, node, { value: false, reason }, "onOpenChange")) {
            raise(scope, node, { value: false, reason })
          }
          setDismissed(true)
        }
      }}
      onDismissAttempt={(details) => {
        raise(scope, node, { reason: details.reason ?? "" }, "onDismissAttempt")
      }}
    >
      <DialogBackdrop className={DIALOG_BACKDROP} />
      <DialogContent
        className={DIALOG_PANEL}
        data-node={node.nodeId}
        data-dialog="true"
        data-blocking={component.blocking ? "true" : "false"}
        data-pending={blocked ? "true" : "false"}
        data-dismissal-methods={methods.join(",")}
        aria-busy={blocked ? "true" : undefined}
        finalFocus={() => returnTo.current}
      >
        <div className="flex items-start justify-between gap-3">
          <DialogTitle className="text-heading font-medium">{titled}</DialogTitle>
          {allowsVisibleClose && (
            blocked ? (
              <Button
                type="button"
                className={BUTTON}
                data-dialog-close=""
                disabled
                focusableWhenDisabled
                aria-label="Close"
              >
                Close
              </Button>
            ) : (
              <DialogClose className={BUTTON} data-dialog-close="" aria-label="Close">
                Close
              </DialogClose>
            )
          )}
        </div>
        {message !== undefined && <DialogDescription className={MUTED}>{text(message, scope.locale, scope.translate)}</DialogDescription>}
        {callState !== undefined && <CallLine state={callState} />}
        {renderChildren(scope, bodyChildren)}
      </DialogContent>
    </Dialog>
  )
}

// ---- Form (WidgetComponent::Form) ---------------------------------------------------------------

const MAX_SAFE_INT = BigInt(Number.MAX_SAFE_INTEGER)
const MIN_SAFE_INT = BigInt(Number.MIN_SAFE_INTEGER)

/**
 * Parse an exact whole number into a JSON-safe JS integer without silent float rounding.
 * Rejects decimals, non-digits, and values outside IEEE-754 safe-integer bounds.
 */
export function parseExactInteger(value: string): { readonly ok: true; readonly value: number } | { readonly ok: false; readonly message: string } {
  const trimmed = value.trim()
  if (!/^-?\d+$/.test(trimmed)) return { ok: false, message: "Enter a whole number." }
  const big = BigInt(trimmed)
  if (big < MIN_SAFE_INT || big > MAX_SAFE_INT) {
    return { ok: false, message: "Whole number exceeds exact integer range." }
  }
  const num = Number(big)
  return { ok: true, value: Object.is(num, -0) ? 0 : num }
}

/** Validate exact numeric text without converting decimals through binary IEEE-754 float. */
export function validateExactNumber(value: string, format?: FieldFormat): string | null {
  const trimmed = value.trim()
  if (trimmed === "") return null
  if (format === "integer") {
    const parsed = parseExactInteger(trimmed)
    return parsed.ok ? null : parsed.message
  }
  return /^-?\d+(\.\d+)?$/.test(trimmed) ? null : "Enter a valid decimal number."
}

export type ConfirmationDecision =
  | { readonly kind: "dispatch" }
  | { readonly kind: "confirm" }
  | { readonly kind: "error"; readonly message: string }

/**
 * Gate form submission against its declared `WidgetActionConfirmation` (`strength: "none" | "click" | "typed"`).
 * Fails closed when `"typed"` confirmation lacks a non-empty phrase or the typed text does not match verbatim.
 */
export function canDispatchFormConfirmation(
  confirmation: ActionConfirmation,
  state: { readonly confirming: boolean; readonly typedPhrase: string },
  locale: string,
  translate?: Translate,
): ConfirmationDecision {
  const strength = confirmation.strength as string
  if (strength === "none") return { kind: "dispatch" }
  if (!state.confirming) return { kind: "confirm" }
  if (strength === "typed") {
    const required = confirmation.phrase === undefined ? "" : text(confirmation.phrase, locale, translate).trim()
    if (required === "") return { kind: "error", message: "Confirmation phrase is not configured." }
    if (state.typedPhrase !== required) return { kind: "error", message: `Type "${required}" to confirm.` }
    return { kind: "dispatch" }
  }
  return { kind: "dispatch" }
}

/** Stable draft key for a `FieldPresentation`. */
export function extractFieldKey(field: FieldPresentation): string {
  const target = field.target
  if (target.kind === "form_input" && "inputName" in target && typeof target.inputName === "string") return target.inputName
  if (target.kind === "parent_property" && "inputName" in target && "propertyId" in target && typeof target.inputName === "string" && typeof target.propertyId === "string") {
    return `${target.inputName}.${target.propertyId}`
  }
  if (target.kind === "property" && target.property !== undefined) return target.property.propertyId
  return field.fieldId
}

/**
 * Build the structured action draft payload from flat field draft state:
 * - Decimals stay exact text strings (VAL-EXACT-1, zero binary float loss).
 * - Integers (`format: "integer"`) parse to exact JSON integers via `parseExactInteger` (never silently rounded).
 */
export function buildFormDraftPayload(fields: readonly FieldPresentation[], draft: Readonly<Record<string, unknown>>): Record<string, unknown> {
  const out: Record<string, unknown> = {}
  for (const field of fields) {
    if (field.control === "display") continue
    const key = extractFieldKey(field)
    if (!Object.hasOwn(draft, key)) continue
    let value = draft[key]
    if (field.control === "number" && field.format === "integer" && typeof value === "string") {
      const trimmed = value.trim()
      if (trimmed === "") continue
      const parsed = parseExactInteger(trimmed)
      if (parsed.ok) value = parsed.value
    }
    const target = field.target
    if (target.kind === "form_input" && "inputName" in target && typeof target.inputName === "string") {
      out[target.inputName] = value
    } else if (
      target.kind === "parent_property" &&
      "inputName" in target &&
      "propertyId" in target &&
      typeof target.inputName === "string" &&
      typeof target.propertyId === "string"
    ) {
      const group = asDoc(out[target.inputName])
      out[target.inputName] = { ...(group ?? {}), [target.propertyId]: value }
    } else if (target.kind === "property" && target.property !== undefined) {
      out[target.property.propertyId] = value
    } else {
      out[field.fieldId] = value
    }
  }
  return out
}

/** Whether `draft` diverges from `initial` on any field key. */
export function computeFormDirty(initial: Readonly<Record<string, unknown>>, draft: Readonly<Record<string, unknown>>): boolean {
  const keys = new Set([...Object.keys(initial), ...Object.keys(draft)])
  for (const key of keys) {
    if (!Object.is(initial[key], draft[key])) return true
  }
  return false
}

export interface PrefillInfo {
  readonly value: unknown
  readonly masked: MaskedProperty | undefined
  readonly withheld: boolean
}

function prefillRefOf(form: WidgetForm, field: FieldPresentation): PropertyRef | undefined {
  const target = field.target
  if (target.kind === "form_input" && "initial" in target && target.initial !== undefined) return target.initial
  if (target.kind === "property" && target.property !== undefined) return target.property
  if (form.dataBindingId !== undefined) {
    if (target.kind === "parent_property" && "propertyId" in target && typeof target.propertyId === "string") {
      return { bindingId: form.dataBindingId, propertyId: target.propertyId }
    }
    if (target.kind === "form_input" && "inputName" in target && typeof target.inputName === "string") {
      return { bindingId: form.dataBindingId, propertyId: target.inputName }
    }
  }
  return undefined
}

/** Read a form field's server prefill together with its L6 masking/withholding metadata. */
export function readPrefill(scope: PageScope, form: WidgetForm, field: FieldPresentation): PrefillInfo {
  const ref = prefillRefOf(form, field)
  if (ref === undefined) return { value: undefined, masked: undefined, withheld: false }
  const state = scope.runtime.reads.get(ref.bindingId)
  if (state?.kind !== "ok") return { value: undefined, masked: undefined, withheld: false }
  const doc = asDoc(state.document)
  if (doc === null) return { value: undefined, masked: undefined, withheld: false }
  const masked = own(asDoc(doc.masked), ref.propertyId) as MaskedProperty | undefined
  const withheld =
    doc.withheld === true ||
    masked?.mode === "refuse" ||
    (Array.isArray(doc.withheldProperties) && doc.withheldProperties.includes(ref.propertyId))
  if (withheld) return { value: undefined, masked, withheld: true }
  const values = asDoc(doc.values) ?? doc
  return { value: own(values, ref.propertyId), masked, withheld: false }
}

function defaultFieldValue(field: FieldPresentation, prefill: unknown): unknown {
  if (field.control === "checkbox") return typeof prefill === "boolean" ? prefill : false
  if (prefill === undefined || prefill === null) return ""
  return typeof prefill === "string" ? prefill : String(prefill)
}

function buildInitialDraft(scope: PageScope, form: WidgetForm, fields: readonly FieldPresentation[]): Record<string, unknown> {
  const initial: Record<string, unknown> = {}
  for (const field of fields) {
    const key = extractFieldKey(field)
    const prefill = readPrefill(scope, form, field)
    initial[key] = defaultFieldValue(field, prefill.withheld ? undefined : prefill.value)
  }
  return initial
}

function evaluateCondition(
  scope: PageScope,
  condition: ZenCondition | undefined,
  draftPayload: Readonly<Record<string, unknown>>,
  fallback: boolean,
): boolean {
  if (condition === undefined) return fallback
  const recordBindingId = condition.inputs.recordBindingId
  const recordState = recordBindingId === undefined ? undefined : scope.runtime.reads.get(recordBindingId)
  const recordDoc = recordState?.kind === "ok" ? asDoc(recordState.document) : null
  const recordValues = recordDoc !== null ? (asDoc(recordDoc.values) ?? recordDoc) : undefined
  const sources = {
    context: scope.context ?? {},
    ...(recordValues !== undefined ? { record: recordValues } : {}),
    ...(condition.inputs.draftFormId !== undefined ? { draft: draftPayload } : {}),
  }
  try {
    const result = scope.runtime.evaluate(condition.expression, sources as never)
    return typeof result === "boolean" ? result : fallback
  } catch (error) {
    if (error instanceof EngineNotReady) return fallback
    return fallback
  }
}

function extractSelectOptions(document: unknown): Array<{ readonly value: string; readonly label: string }> {
  const doc = asDoc(document)
  const raw = Array.isArray(doc?.records)
    ? doc.records
    : Array.isArray(doc?.items)
      ? doc.items
      : Array.isArray(doc?.options)
        ? doc.options
        : []
  const options: Array<{ readonly value: string; readonly label: string }> = []
  for (let index = 0; index < raw.length; index += 1) {
    const item = raw[index]
    if (typeof item === "string") {
      options.push({ value: item, label: item })
      continue
    }
    const row = asDoc(item)
    if (row === null || row.withheld === true) continue
    const value = rowIdOf(row, index)
    const values = asDoc(row.values) ?? row
    const candidate = [values.label, values.name, values.title, values.code].find((entry): entry is string => typeof entry === "string" && entry !== "")
    options.push({ value, label: candidate ?? value })
  }
  return options
}

function FormCallStatus({ state }: { readonly state: BindingCallState | undefined }) {
  if (state === undefined) return null
  return matchActionState(state, {
    editing: () => null,
    confirming: () => <p role="status" className={MUTED} data-call="confirming">Waiting for you to confirm.</p>,
    sending: () => <p role="status" aria-busy="true" className={MUTED} data-call="sending">Submitting…</p>,
    pending_approval: () => <p role="status" className={MUTED} data-call="pending_approval">Sent for approval: nothing is committed until it is decided.</p>,
    committed: () => <p role="status" className="text-meta text-foreground" data-call="committed">Saved.</p>,
    refused: ({ refusal }) => (
      <div
        role="alert"
        data-call="refused"
        data-category={refusal.category ?? undefined}
        data-code={refusal.code || undefined}
        className="grid gap-1 rounded-[var(--radius)] border border-border bg-card p-3 text-meta text-destructive-text"
      >
        <span className="font-medium" data-refusal-category={refusal.category ?? "unknown"}>{formatRefusalCategory(refusal.category)}</span>
        <span>{messageOf(refusal.document, `The cell refused this call (${refusal.code}).`)}</span>
      </div>
    ),
    not_served: () => <p role="status" className={MUTED} data-call="not_served">This workspace serves no cell to call: nothing was sent.</p>,
    unavailable: () => <p role="status" className={MUTED} data-call="unavailable">The cell answered without applying the call: nothing committed.</p>,
    timed_out: ({ message }) => <p role="status" className={MUTED} data-call="timed_out">{message ?? "The answer was lost; the call may have applied."}</p>,
    undoing: () => <p role="status" aria-busy="true" className={MUTED} data-call="undoing">Undoing…</p>,
    undo_failed: ({ message }) => <p role="status" className={MUTED} data-call="undo_failed">{message ?? "The undo did not come back."}</p>,
    undone: () => <p role="status" className={MUTED} data-call="undone">Undone.</p>,
  })
}

function FormFieldControlView({
  scope,
  field,
  fieldKey,
  value,
  agentMark,
  withheld,
  disabled,
  readOnly,
  required,
  invalid,
  errorText,
  onChange,
}: {
  readonly scope: PageScope
  readonly field: FieldPresentation
  readonly fieldKey: string
  readonly value: unknown
  readonly agentMark: AgentAttributes
  readonly withheld: boolean
  readonly disabled: boolean
  readonly readOnly: boolean
  readonly required: boolean
  readonly invalid: boolean
  readonly errorText: string | null
  readonly onChange: (next: unknown) => void
}) {
  const label = text(field.label, scope.locale, scope.translate)
  const help = field.help === undefined ? undefined : text(field.help, scope.locale, scope.translate)
  const control = field.control ?? "text"
  const stringValue = withheld ? "" : (typeof value === "string" ? value : value === null || value === undefined ? "" : String(value))
  const effectiveDisabled = disabled || withheld
  const effectiveReadOnly = readOnly || withheld

  if (control === "display") {
    const prop = propertyOf(field)
    const propRead = prop === undefined ? undefined : scope.runtime.reads.get(prop.bindingId)
    const propRecord = propRead?.kind === "ok" ? asDoc(propRead.document) : null
    return (
      <div className="grid gap-1" data-field={field.fieldId} data-control="display">
        <span className={MUTED}>{label}</span>
        {prop !== undefined && propRecord !== null
          ? <span className="text-body text-foreground"><Value record={propRecord} propertyId={prop.propertyId} /></span>
          : withheld
            ? <span {...agentMark} className={MUTED} data-value="withheld">Withheld</span>
            : <span {...agentMark} className="text-body text-foreground" data-value="plain">{shown(value)}</span>}
        {help !== undefined && <p className={MUTED}>{help}</p>}
      </div>
    )
  }

  if (control === "checkbox") {
    const checked = !withheld && value === true
    return (
      <Field name={fieldKey} disabled={effectiveDisabled || effectiveReadOnly} invalid={invalid} className="grid gap-1" data-field={field.fieldId} data-control="checkbox">
        <div className="flex items-center gap-2">
          <Checkbox
            {...agentMark}
            checked={checked}
            disabled={effectiveDisabled}
            readOnly={effectiveReadOnly}
            required={required}
            aria-required={required ? "true" : undefined}
            aria-invalid={invalid ? "true" : undefined}
            onCheckedChange={(next) => {
              if (!effectiveReadOnly) onChange(next === true)
            }}
            className="inline-flex size-4 items-center justify-center rounded-[3px] border border-input bg-background text-[11px] leading-none text-background outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring data-[checked]:border-foreground data-[checked]:bg-foreground disabled:opacity-50"
          >
            <CheckboxIndicator>✓</CheckboxIndicator>
          </Checkbox>
          <FieldLabel className="text-body text-foreground">
            {label}
            {required && <span aria-hidden="true" className="ms-0.5 text-destructive-text">*</span>}
          </FieldLabel>
        </div>
        {help !== undefined && <FieldDescription className={MUTED}>{help}</FieldDescription>}
        {errorText !== null && <FieldError match={true} className="text-meta text-destructive-text">{errorText}</FieldError>}
      </Field>
    )
  }

  if (control === "select" || control === "link") {
    const renderSelect = (options: readonly { readonly value: string; readonly label: string }[]) => (
      <Select
        value={stringValue === "" ? null : stringValue}
        disabled={effectiveDisabled || effectiveReadOnly}
        required={required}
        onValueChange={(next) => {
          if (!effectiveReadOnly) onChange(typeof next === "string" ? next : "")
        }}
      >
        <SelectLabel className={MUTED}>
          {label}
          {required && <span aria-hidden="true" className="ms-0.5 text-destructive-text">*</span>}
        </SelectLabel>
        <SelectTrigger
          {...agentMark}
          aria-required={required ? "true" : undefined}
          aria-invalid={invalid ? "true" : undefined}
          className="inline-flex h-9 w-full items-center justify-between gap-2 rounded-[var(--radius)] border border-input bg-background px-3 text-body text-foreground outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring disabled:opacity-50 max-sm:min-h-[44px]"
        >
          <SelectValue placeholder={withheld ? "Withheld" : `Select ${label}`} />
        </SelectTrigger>
        <SelectContent className="z-50 rounded-[var(--radius-popup)] border border-border bg-popover p-1 text-popover-foreground shadow-md">
          {options.length === 0 ? (
            <SelectEmpty className={`${MUTED} px-2 py-1.5`}>No options available.</SelectEmpty>
          ) : (
            options.map((option) => (
              <SelectItem
                key={option.value}
                value={option.value}
                className="flex cursor-pointer items-center justify-between rounded-[var(--radius)] px-2 py-1.5 text-body outline-none data-[highlighted]:bg-accent"
              >
                <SelectItemText>{option.label}</SelectItemText>
              </SelectItem>
            ))
          )}
        </SelectContent>
      </Select>
    )
    const optionState = field.optionQueryBindingId === undefined ? undefined : scope.runtime.reads.get(field.optionQueryBindingId)
    return (
      <Field name={fieldKey} disabled={effectiveDisabled || effectiveReadOnly} invalid={invalid} className="grid gap-1" data-field={field.fieldId} data-control={control}>
        {field.optionQueryBindingId !== undefined ? (
          <ReadView state={optionState}>
            {(document) => renderSelect(extractSelectOptions(document))}
          </ReadView>
        ) : (
          renderSelect([])
        )}
        {help !== undefined && <FieldDescription className={MUTED}>{help}</FieldDescription>}
        {errorText !== null && <FieldError match={true} className="text-meta text-destructive-text">{errorText}</FieldError>}
      </Field>
    )
  }

  return (
    <Field name={fieldKey} disabled={effectiveDisabled} invalid={invalid} className="grid gap-1" data-field={field.fieldId} data-control={control} data-format={field.format}>
      <FieldLabel className={MUTED}>
        {label}
        {required && <span aria-hidden="true" className="ms-0.5 text-destructive-text">*</span>}
      </FieldLabel>
      {control === "text_area" ? (
        <Textarea
          {...agentMark}
          className={TEXTAREA}
          value={stringValue}
          placeholder={withheld ? "Withheld" : undefined}
          disabled={effectiveDisabled}
          readOnly={effectiveReadOnly}
          required={required}
          aria-required={required ? "true" : undefined}
          aria-invalid={invalid ? "true" : undefined}
          onValueChange={(next) => {
            if (!effectiveReadOnly) onChange(next)
          }}
        />
      ) : control === "number" ? (
        // Exact-string numeric input: never passes through binary float (`type="number"` / `Number()`).
        <Input
          {...agentMark}
          type="text"
          inputMode={field.format === "integer" ? "numeric" : "decimal"}
          className={`${INPUT} tabular-nums`}
          value={stringValue}
          placeholder={withheld ? "Withheld" : undefined}
          disabled={effectiveDisabled}
          readOnly={effectiveReadOnly}
          required={required}
          aria-required={required ? "true" : undefined}
          aria-invalid={invalid ? "true" : undefined}
          onValueChange={(next) => {
            if (!effectiveReadOnly) onChange(next)
          }}
        />
      ) : control === "date" ? (
        <Input
          {...agentMark}
          type="date"
          className={`${INPUT} tabular-nums`}
          value={stringValue}
          disabled={effectiveDisabled}
          readOnly={effectiveReadOnly}
          required={required}
          aria-required={required ? "true" : undefined}
          aria-invalid={invalid ? "true" : undefined}
          onValueChange={(next) => {
            if (!effectiveReadOnly) onChange(next)
          }}
        />
      ) : control === "date_time" ? (
        // Zoned ISO-8601 timestamp text: preserves explicit timezone offset without browser stripping.
        <Input
          {...agentMark}
          type="text"
          placeholder={withheld ? "Withheld" : "YYYY-MM-DDTHH:MM:SSZ"}
          className={`${INPUT} tabular-nums`}
          value={stringValue}
          disabled={effectiveDisabled}
          readOnly={effectiveReadOnly}
          required={required}
          aria-required={required ? "true" : undefined}
          aria-invalid={invalid ? "true" : undefined}
          onValueChange={(next) => {
            if (!effectiveReadOnly) onChange(next)
          }}
        />
      ) : (
        <Input
          {...agentMark}
          type="text"
          className={INPUT}
          value={stringValue}
          placeholder={withheld ? "Withheld" : undefined}
          disabled={effectiveDisabled}
          readOnly={effectiveReadOnly}
          required={required}
          aria-required={required ? "true" : undefined}
          aria-invalid={invalid ? "true" : undefined}
          onValueChange={(next) => {
            if (!effectiveReadOnly) onChange(next)
          }}
        />
      )}
      {help !== undefined && <FieldDescription className={MUTED}>{help}</FieldDescription>}
      {errorText !== null && <FieldError match={true} className="text-meta text-destructive-text">{errorText}</FieldError>}
    </Field>
  )
}

function FormNode({ scope, node, formId }: { readonly scope: PageScope; readonly node: WidgetNode; readonly formId: string }) {
  const form = scope.ux.forms.find((candidate) => candidate.formId === formId)
  if (form === undefined) {
    return <p role="status" className={MUTED} data-node={node.nodeId} data-form-missing={formId}>{`Form "${formId}" is not declared.`}</p>
  }

  const fieldChildren = node.children.filter((child): child is WidgetNode & { readonly component: Extract<Component, { readonly kind: "field" }> } => child.component.kind === "field")
  const footerActions = node.children.filter((child) => child.component.kind === "action")
  const formFields: FieldPresentation[] = fieldChildren.length > 0
    ? fieldChildren.flatMap((child) => {
        const found = scope.ux.fields.find((candidate) => candidate.fieldId === child.component.fieldId)
        return found === undefined ? [] : [found]
      })
    : scope.ux.fields.filter((candidate) => "formId" in candidate.target && candidate.target.formId === form.formId)

  const baselineBindingId = form.versionConflictBinding?.property.bindingId ?? form.dataBindingId
  const baselineRead = baselineBindingId === undefined ? undefined : scope.runtime.reads.get(baselineBindingId)
  const liveBaselineRecord: PinnedRecord | undefined =
    baselineBindingId !== undefined && baselineRead?.kind === "ok"
      ? { bindingId: baselineBindingId, document: baselineRead.document }
      : undefined
  const liveBaselineKey = liveBaselineRecord === undefined ? "" : JSON.stringify(liveBaselineRecord.document)

  const prefillSnapshot = buildInitialDraft(scope, form, formFields)
  const prefillKey = JSON.stringify(prefillSnapshot)
  const [initialDraft, setInitialDraft] = useState<Record<string, unknown>>(() => prefillSnapshot)
  const [draft, setDraft] = useState<Record<string, unknown>>(() => prefillSnapshot)
  const [pinnedRecord, setPinnedRecord] = useState<PinnedRecord | undefined>(() => liveBaselineRecord)
  const [submitted, setSubmitted] = useState(false)
  const [confirming, setConfirming] = useState(false)
  const [typedPhrase, setTypedPhrase] = useState("")
  const [confirmError, setConfirmError] = useState<string | null>(null)
  const dirty = computeFormDirty(initialDraft, draft)

  // Hydrate from server prefill and advance pinned baseline ONLY while the form is clean.
  // While dirty, keep the edited baseline revision pinned so version checks never submit a newer revision.
  useEffect(() => {
    if (!dirty) {
      setInitialDraft(prefillSnapshot)
      setDraft(prefillSnapshot)
      setPinnedRecord(liveBaselineRecord)
      if (liveBaselineRecord !== undefined) {
        scope.runtime.setDraft?.(form.formId, buildFormDraftPayload(formFields, prefillSnapshot), liveBaselineRecord)
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [prefillKey, liveBaselineKey])

  const draftPayload = buildFormDraftPayload(formFields, draft)
  const callState = scope.runtime.calls.get(form.submitBindingId)
  const currentState = scope.machine.states.find((state) => state.stateId === scope.runtime.snapshot.stateId)
  const isPending =
    callState?.kind === "sending" ||
    callState?.kind === "undoing" ||
    (currentState?.role === "pending" && currentState.awaits?.kind === "action" && currentState.awaits.bindingId === form.submitBindingId)

  const hasMachineEvent = eventsOf(scope.machine, node.nodeId).length > 0
  const { on: machineAllows, waiting: engineWaiting } = hasMachineEvent ? canAct(scope, node) : { on: true, waiting: false }
  const submitDisabled = isPending || !machineAllows
  const submitLabel = text(form.submit.label, scope.locale, scope.translate)
  const submitMark = confirmMark(formConfirmReason(scope.ux, scope.machine, node.nodeId, form, scope.actionEffects, scope.locale, scope.translate))
  const confirmation = form.submit.confirmation
  const requiredPhrase = confirmation.phrase === undefined ? "" : text(confirmation.phrase, scope.locale, scope.translate).trim()
  const confirmMessage = confirmation.message === undefined ? undefined : text(confirmation.message, scope.locale, scope.translate)

  const fieldStates = formFields.map((field) => {
    const key = extractFieldKey(field)
    const prefill = readPrefill(scope, form, field)
    const agentMark = valueMark(prefill.masked, prefill.withheld)
    const value = prefill.withheld ? "" : (Object.hasOwn(draft, key) ? draft[key] : defaultFieldValue(field, undefined))
    const visible = evaluateCondition(scope, field.visibleWhen, draftPayload, true)
    const required = evaluateCondition(scope, field.requiredWhen, draftPayload, false)
    const readOnly = evaluateCondition(scope, field.readOnlyWhen, draftPayload, false)
    let hint: string | null = null
    if (visible && !prefill.withheld && field.control !== "display") {
      if (field.control === "number" && typeof value === "string") {
        hint = validateExactNumber(value, field.format)
      }
      if (hint === null && required && submitted) {
        const empty = field.control === "checkbox" ? value !== true : (typeof value === "string" ? value.trim() === "" : value === null || value === undefined)
        if (empty) hint = "This field is required."
      }
    }
    return { field, key, value, agentMark, withheld: prefill.withheld, visible, required, readOnly, hint }
  })

  const updateField = (key: string, nextValue: unknown) => {
    const nextDraft = { ...draft, [key]: nextValue }
    setDraft(nextDraft)
    if (confirming) {
      setConfirming(false)
      setTypedPhrase("")
      setConfirmError(null)
    }
    scope.runtime.setDraft?.(form.formId, buildFormDraftPayload(formFields, nextDraft), pinnedRecord)
  }

  const handleSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (isPending || submitDisabled) return
    setSubmitted(true)
    // Check inline syntax/required hints; never enforce permissions on the client.
    const hasClientHint = fieldStates.some(({ field, value, visible, withheld, required }) => {
      if (!visible || withheld || field.control === "display") return false
      if (field.control === "number" && typeof value === "string" && validateExactNumber(value, field.format) !== null) return true
      if (required) {
        return field.control === "checkbox" ? value !== true : (typeof value === "string" ? value.trim() === "" : value === null || value === undefined)
      }
      return false
    })
    if (hasClientHint) return

    const gate = canDispatchFormConfirmation(confirmation, { confirming, typedPhrase }, scope.locale, scope.translate)
    if (gate.kind === "confirm") {
      setConfirming(true)
      setConfirmError(null)
      return
    }
    if (gate.kind === "error") {
      setConfirmError(gate.message)
      return
    }

    setConfirming(false)
    setTypedPhrase("")
    setConfirmError(null)
    scope.runtime.setDraft?.(form.formId, draftPayload, pinnedRecord)
    if (hasMachineEvent) {
      raise(scope, node, { draft: draftPayload, ...draftPayload })
    } else {
      scope.runtime.submitForm?.(form.formId, form.submitBindingId, draftPayload, pinnedRecord)
    }
  }

  const prefillState = form.dataBindingId === undefined ? undefined : scope.runtime.reads.get(form.dataBindingId)
  const typedMismatch = confirmation.strength === "typed" && (requiredPhrase === "" || typedPhrase !== requiredPhrase)

  return (
    <form
      noValidate
      onSubmit={handleSubmit}
      data-node={node.nodeId}
      data-form={form.formId}
      data-form-mode={form.mode}
      data-dirty={dirty ? "true" : "false"}
      data-confirming={confirming ? "true" : "false"}
      data-pending={isPending ? "true" : "false"}
      aria-busy={isPending ? "true" : undefined}
      className="grid gap-4"
    >
      {form.mode === "edit" && prefillState !== undefined && prefillState.kind !== "ok" && (
        <ReadView state={prefillState}>{() => null}</ReadView>
      )}
      <div className="grid gap-3">
        {fieldStates.map(({ field, key, value, agentMark, withheld, visible, required, readOnly, hint }) => {
          if (!visible) {
            return (
              <input
                {...agentMark}
                key={field.fieldId}
                type="hidden"
                name={key}
                data-field={field.fieldId}
                data-control="hidden"
                value={withheld ? "" : (typeof value === "string" ? value : value === null || value === undefined ? "" : String(value))}
              />
            )
          }
          return (
            <FormFieldControlView
              key={field.fieldId}
              scope={scope}
              field={field}
              fieldKey={key}
              value={value}
              agentMark={agentMark}
              withheld={withheld}
              disabled={isPending}
              readOnly={readOnly}
              required={required}
              invalid={hint !== null}
              errorText={hint}
              onChange={(next) => updateField(key, next)}
            />
          )
        })}
      </div>
      {confirming && (
        <div
          role="region"
          aria-label="Confirm submission"
          data-form-confirm={confirmation.strength}
          className="grid gap-2 rounded-[var(--radius)] border border-border bg-card p-3"
        >
          <p className="text-body text-foreground">{confirmMessage ?? `Confirm ${submitLabel}.`}</p>
          {confirmation.strength === "typed" && (
            <label className="grid gap-1">
              <span className={MUTED} data-confirm-phrase={requiredPhrase}>
                {requiredPhrase === "" ? "Confirmation phrase is not configured." : `Type "${requiredPhrase}" to confirm.`}
              </span>
              <Input
                type="text"
                className={INPUT}
                data-form-confirm-input={form.formId}
                value={typedPhrase}
                disabled={isPending}
                onValueChange={(next) => {
                  setTypedPhrase(next)
                  setConfirmError(null)
                }}
              />
            </label>
          )}
          {confirmError !== null && <p role="alert" className="text-meta text-destructive-text">{confirmError}</p>}
          <div className="flex flex-wrap items-center gap-2">
            <Button
              {...submitMark}
              type="submit"
              className={BUTTON}
              data-form-confirm-submit={form.formId}
              disabled={submitDisabled || typedMismatch}
              focusableWhenDisabled
              aria-busy={isPending ? "true" : undefined}
            >
              {`Confirm ${submitLabel}`}
            </Button>
            <Button
              type="button"
              className={BUTTON}
              data-form-confirm-cancel={form.formId}
              disabled={isPending}
              onClick={() => {
                setConfirming(false)
                setTypedPhrase("")
                setConfirmError(null)
              }}
            >
              Cancel
            </Button>
          </div>
        </div>
      )}
      <FormCallStatus state={callState} />
      <div className="flex flex-wrap items-center gap-2">
        {!confirming && (
          <Button
            {...submitMark}
            type="submit"
            className={BUTTON}
            data-form-submit={form.formId}
            disabled={submitDisabled}
            focusableWhenDisabled
            aria-busy={isPending ? "true" : undefined}
            data-engine={engineWaiting ? "waiting" : undefined}
          >
            {isPending ? "Submitting…" : submitLabel}
          </Button>
        )}
        {footerActions.map((child) => <Node key={child.nodeId} scope={scope} node={child} />)}
      </div>
    </form>
  )
}

/**
 * A UI SDK layer held open in the machine states its node names (`heldIn`). `heldIn` is the one source
 * of whether it is open (the checker's L1 refuses a set `open`, and `defaultOpen` on a held or locking
 * layer): its own `open`/`defaultOpen` props are never read here.
 */
function Layer({ scope, node, primitive }: { readonly scope: PageScope; readonly node: WidgetNode; readonly primitive: string }) {
  const heldIn = node.component.kind === "catalog" ? node.component.heldIn ?? [] : []
  const open = heldIn.includes(scope.runtime.snapshot.stateId)
  const { props } = propsOf(scope, node)
  const title = node.children.find((child) => child.component.kind === "section")
  const titled = title?.component.kind === "section" ? text(title.component.section.label, scope.locale, scope.translate) : scope.page.name
  // The layer's message: the confirmation message of the action it asks about (an alert dialog points at it).
  const asked = nodesOf(node).flatMap((child) => (child.component.kind === "action" ? [child.component.placementId] : []))
  const message = scope.ux.actionPlacements.find((placement) => asked.includes(placement.placementId))?.confirmation.message
  // Where focus returns when the machine closes the layer: what had it when the layer opened.
  const wasOpen = useRef(false)
  const returnTo = useRef<HTMLElement | null>(null)
  useIsomorphicLayoutEffect(() => {
    if (open && !wasOpen.current && typeof document !== "undefined") {
      // What had focus before the layer opened (ignoring focus already moved inside the layer by
      // child layout effects), or the control whose event opened it.
      const candidate = document.activeElement instanceof HTMLElement && document.activeElement !== document.body ? document.activeElement : null
      const active = candidate !== null && candidate.closest("[data-layer]") === null ? candidate : null
      const raised = scope.raisedBy.current
      returnTo.current = active ?? (raised === null ? null : document.querySelector<HTMLElement>(`[data-node="${escapeSelector(raised)}"]`))
    }
    wasOpen.current = open
  }, [open, scope.raisedBy])
  const finalFocus = () => returnTo.current
  const options = {
    open,
    ...(typeof props.dismissible === "boolean" ? { dismissible: props.dismissible } : {}),
    ...(typeof props.pendingAnnouncement === "string" ? { pendingAnnouncement: props.pendingAnnouncement } : {}),
    // A callback's typed value reaches the event as `value`; a close the machine does not take keeps the layer open.
    onOpenChange: (next: boolean, details: { readonly reason?: string }) => raise(scope, node, { value: next, reason: details.reason ?? "" }, "onOpenChange"),
    onDismissAttempt: (details: { readonly reason?: string }) => raise(scope, node, { reason: details.reason ?? "" }, "onDismissAttempt"),
  }
  // The section that names the layer is its title: its own children draw in the body, its label once.
  const bodyChildren = node.children.flatMap((child) => (child === title ? child.children : [child]))
  const children = renderChildren(scope, bodyChildren)
  return primitive === "alert-dialog" ? (
    <AlertDialog {...options}>
      <AlertDialogBackdrop className={DIALOG_BACKDROP} />
      <AlertDialogContent className={DIALOG_PANEL} data-node={node.nodeId} data-layer={primitive} finalFocus={finalFocus}>
        <AlertDialogTitle className="text-heading font-medium">{titled}</AlertDialogTitle>
        {message !== undefined && <AlertDialogDescription className={MUTED}>{text(message, scope.locale, scope.translate)}</AlertDialogDescription>}
        {children}
      </AlertDialogContent>
    </AlertDialog>
  ) : (
    <Dialog {...options}>
      <DialogBackdrop className={DIALOG_BACKDROP} />
      <DialogContent className={DIALOG_PANEL} data-node={node.nodeId} data-layer={primitive} finalFocus={finalFocus}>
        <DialogTitle className="text-heading font-medium">{titled}</DialogTitle>
        {message !== undefined && <DialogDescription className={MUTED}>{text(message, scope.locale, scope.translate)}</DialogDescription>}
        {children}
      </DialogContent>
    </Dialog>
  )
}

/**
 * A catalog island, given its props, its emit and, for Tier 2, the page's server states. It is held
 * (a placeholder) until every prop evaluates, so it never sees a prop its generated type says is
 * there missing. An island whose events start an action that needs a person carries the mark (L6).
 */
function Island({ scope, node }: { readonly scope: PageScope; readonly node: WidgetNode }) {
  if (node.component.kind !== "catalog") return null
  // An island held in states (`heldIn`) is drawn only in them: what the page shows in each state.
  const heldIn = node.component.heldIn ?? []
  if (heldIn.length > 0 && !heldIn.includes(scope.runtime.snapshot.stateId)) return null
  const entryId = node.component.entry.entryId
  const entry = scope.ux.catalog?.find((candidate) => candidate.entryId === entryId)
  const IslandComponent = (Object.hasOwn(scope.islands, entryId) ? scope.islands[entryId] : undefined) as ComponentType<Record<string, unknown>> | undefined
  const tier = entry === undefined ? null : tierOf(entry.trust)
  if (IslandComponent === undefined || tier === null) return null
  const mark = confirmMark(confirmReason(scope.ux, scope.machine, node.nodeId, null, scope.actionEffects, scope.locale, scope.translate))
  const { props, waiting } = propsOf(scope, node)
  if (waiting.length > 0) {
    return <div {...mark} data-node={node.nodeId} data-island={entryId} data-tier={tier} data-engine="waiting" aria-busy="true" className="h-9 animate-pulse rounded-[var(--radius)] bg-muted motion-reduce:animate-none" />
  }
  const input: Record<string, unknown> = {
    props,
    emit: (event: string, payload: Fields = {}) => raise(scope, node, payload, event),
    ...(tier === "stateful"
      ? { states: { reads: Object.fromEntries(scope.runtime.reads), calls: Object.fromEntries(scope.runtime.calls) } satisfies IslandStates }
      : {}),
  }
  return <div {...mark} data-node={node.nodeId} data-island={entryId} data-tier={tier}><IslandComponent {...input} /></div>
}

function ListNode({
  scope,
  node,
  component,
}: {
  readonly scope: PageScope
  readonly node: WidgetNode
  readonly component: Extract<Component, { readonly kind: "list" }>
}) {
  const readState = scope.runtime.reads.get(component.dataBindingId)
  const isOk = readState?.kind === "ok"
  // Clear runtime selection whenever a selectable list's read leaves `ok` so stale rows never linger.
  useEffect(() => {
    if (component.selection !== "none" && !isOk) {
      scope.runtime.setSelection?.(component.dataBindingId, [])
    }
  }, [component.selection, component.dataBindingId, isOk, scope.runtime])

  return (
    <div data-node={node.nodeId} className="grid gap-3">
      <ReadView state={readState}>
        {(document) => (
          <ListRows
            scope={scope}
            node={node}
            document={document}
            refs={component.fields}
            dataBindingId={component.dataBindingId}
            selection={component.selection}
          />
        )}
      </ReadView>
      {renderChildren(scope, node.children)}
    </div>
  )
}

/** One node of the tree. */
export function Node({ scope, node }: { readonly scope: PageScope; readonly node: WidgetNode }): ReactNode {
  const component = node.component
  switch (component.kind) {
    case "section": {
      const label = text(component.section.label, scope.locale, scope.translate)
      const binding = component.section.fields[0]?.bindingId
      const read = binding === undefined ? undefined : scope.runtime.reads.get(binding)
      // The page's root section is the page itself: its label names the page's region, and the host's
      // shell (or a node inside it) titles the page. A nested section is headed by its label.
      const root = node === scope.page.root
      return (
        <section aria-label={label} data-node={node.nodeId} className={root ? "grid gap-6" : "grid gap-3"}>
          {!root && <h2 className="text-heading font-medium">{label}</h2>}
          {binding !== undefined && <ReadView state={read}>{(document) => <ObjectFields scope={scope} record={asDoc(document) ?? {}} refs={component.section.fields} />}</ReadView>}
          {renderChildren(scope, node.children)}
        </section>
      )
    }
    case "detail": {
      const refs = scope.ux.fields.flatMap((field) => {
        const prop = propertyOf(field)
        return prop?.bindingId === component.dataBindingId ? [prop] : []
      })
      return (
        <div data-node={node.nodeId} className="grid gap-3">
          <ReadView state={scope.runtime.reads.get(component.dataBindingId)}>{(document) => <ObjectFields scope={scope} record={asDoc(document) ?? {}} refs={refs} />}</ReadView>
          {renderChildren(scope, node.children)}
        </div>
      )
    }
    case "list":
      return <ListNode scope={scope} node={node} component={component} />
    case "field": {
      const field = scope.ux.fields.find((candidate) => candidate.fieldId === component.fieldId)
      const ref = propertyOf(field)
      const read = ref === undefined ? undefined : scope.runtime.reads.get(ref.bindingId)
      return (
        <div data-node={node.nodeId} className="grid gap-1">
          <span className={MUTED}>{text(field?.label ?? { en: component.fieldId }, scope.locale, scope.translate)}</span>
          {ref !== undefined && <ReadView state={read}>{(document) => <Value record={asDoc(document) ?? {}} propertyId={ref.propertyId} />}</ReadView>}
        </div>
      )
    }
    case "action": {
      const placement = scope.ux.actionPlacements.find((candidate) => candidate.placementId === component.placementId)
      return (
        <div data-node-group={node.nodeId} className="flex flex-wrap items-center gap-2">
          <Control scope={scope} node={node} label={text(placement?.label ?? { en: component.placementId }, scope.locale, scope.translate)} placementId={component.placementId} />
          <CallLine state={placement === undefined ? undefined : scope.runtime.calls.get(placement.bindingId)} />
        </div>
      )
    }
    case "control":
      return <Control scope={scope} node={node} label={text(component.label, scope.locale, scope.translate)} placementId={null} />
    case "navigation": {
      const item = scope.ux.navigationItems.find((candidate) => candidate.itemId === component.itemId)
      const label = text(item?.label ?? { en: component.itemId }, scope.locale, scope.translate)
      return eventsOf(scope.machine, node.nodeId).length > 0
        ? <Control scope={scope} node={node} label={label} placementId={null} />
        : <Button className={BUTTON} data-node={node.nodeId} onClick={() => scope.navigate(component.itemId)}>{label}</Button>
    }
    case "catalog": {
      const primitive = primitiveOf(node)
      return primitive === null ? <Island scope={scope} node={node} /> : <Layer scope={scope} node={node} primitive={primitive} />
    }
    case "dialog":
      return <DialogNode scope={scope} node={node} component={component} />
    case "form":
      return <FormNode scope={scope} node={node} formId={component.formId} />
    case "child_grid":
    case "composition":
      // Refused before the page draws (`missingCapabilities`): never drawn in part.
      return null
  }
}
