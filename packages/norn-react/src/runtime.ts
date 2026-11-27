/**
 * The React runtime of one page's `WidgetMachine` (UI-MACHINE-1, #1762 milestone C).
 *
 * `useWidgetMachine` runs the admitted machine with the framework-free driver (`machine/driver.ts`):
 * guard-free transitions fire at once, in plain TypeScript; a transition that needs a ZEN guard,
 * assignment or input waits for the checker's engine, which loads lazily after the page's first
 * paint (`lazyEngine`), so the WASM never sits on the initial load path (DESIGN.md: WASM loaded on
 * intent, LCP ≤ 2.5s). Reads go through the decided workspace's reader and calls through the
 * host's calls; each answer becomes a closed `ReadState` / `ActionCallState` the page draws.
 *
 * `createPageRuntime` is that runtime without React: one per driver, so everything it read or sent
 * belongs to the one workspace its reader and calls were given for. A record read in one workspace
 * is never mapped into a call to another, and a rebuilt runtime starts with no reads or calls.
 *
 * When the engine does not load, or does not come within the wait limit of a message that needs it
 * (the pending state's own `deadlineMs`, else {@link ENGINE_WAIT_MS}), the page says the engine is
 * unavailable and offers a person's Retry (`retryEngine`). The waiting messages are kept, in order,
 * and replayed once a Retry loads it: nothing that needed the engine has run until then.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { createDriver, DEFAULT_CLOCK, type Clock, type Driver, type Transport } from "./machine/driver.ts"
import { type EngineLoader, lazyEngine, literal, type LazyEngine } from "./machine/evaluate.ts"
import { init, type Machine, type Msg, type Snapshot } from "./machine/machine.ts"
import { bindingRead, callStateOf, contextRead, followed, readStateOf, type BindingCallState, type BindingReadState, type WidgetCalls, type WidgetLive, type WidgetReader } from "./reads.ts"
import { nodesOf, type WidgetUx } from "./ux.ts"

export type EngineState = "idle" | "loading" | "ready" | "failed"

/** How long a message in a state with no deadline of its own waits for the engine before the page says it is unavailable. */
export const ENGINE_WAIT_MS = 15_000

/** Default deadline for direct form submits when no pending machine state declares a `deadlineMs` for the binding. */
export const DEFAULT_SUBMIT_DEADLINE_MS = 10_000

/** Baseline record snapshot pinned while a form has dirty edits so optimistic version checks submit the edited revision. */
export interface PinnedRecord {
  readonly bindingId: string
  readonly document: unknown
}

/**
 * Synchronous L6 masking/withholding snapshot captured in `runtime.ts` on every `ok` read, before
 * React batching can collapse an `ok -> pending` transition and hide new withholding from a dirty form.
 */
export interface ReadSecurity {
  readonly withheld: boolean
  readonly masked: Readonly<Record<string, unknown>>
  readonly withheldProperties: readonly string[]
}

/**
 * Compaction-free per-binding L6 security watermark ledger keyed by monotonic `ok`-read sequence
 * numbers. Stores only the latest `seq` per security attribute (O(#properties), never grows with
 * snapshot count and never compacts away clean baselines). Querying `securitySince(log, afterSeq)`
 * returns exact monotonic security (`refuse > redacted`) for every `ok` read strictly newer than
 * `afterSeq`, while never resurrecting withholding cleared before `afterSeq`.
 */
export interface ReadSecurityLog {
  readonly latestSeq: number
  readonly recordWithheldSeq: number
  readonly withheldPropertySeq: Readonly<Record<string, number>>
  readonly refuseMaskSeq: Readonly<Record<string, { readonly seq: number; readonly mask: unknown }>>
  readonly latestMaskSeq: Readonly<Record<string, { readonly seq: number; readonly mask: unknown }>>
}

const isRefuseMask = (entry: unknown): boolean =>
  typeof entry === "object" && entry !== null && !Array.isArray(entry) && (entry as { readonly mode?: unknown }).mode === "refuse"

/** Merge two `ReadSecurity` snapshots monotonically: never downgrade `withheld` or `mode: "refuse"`. */
export function mergeReadSecurity(previous: ReadSecurity | undefined, incoming: ReadSecurity): ReadSecurity {
  if (previous === undefined) return incoming
  const masked: Record<string, unknown> = { ...previous.masked }
  for (const key of Object.keys(incoming.masked)) {
    if (!Object.hasOwn(incoming.masked, key)) continue
    if (isRefuseMask(previous.masked[key]) && !isRefuseMask(incoming.masked[key])) continue
    masked[key] = incoming.masked[key]
  }
  const withheldSet = new Set([...previous.withheldProperties, ...incoming.withheldProperties])
  for (const key of Object.keys(masked)) {
    if (Object.hasOwn(masked, key) && isRefuseMask(masked[key])) withheldSet.add(key)
  }
  return {
    withheld: previous.withheld || incoming.withheld,
    masked,
    withheldProperties: [...withheldSet],
  }
}

/** Exact monotonic `ReadSecurity` for all `ok` reads strictly newer than `afterSeq`. */
export function securitySince(log: ReadSecurityLog | undefined, afterSeq: number): ReadSecurity | undefined {
  if (log === undefined || log.latestSeq <= afterSeq) return undefined
  const withheld = log.recordWithheldSeq > afterSeq
  const withheldSet = new Set<string>()
  for (const key of Object.keys(log.withheldPropertySeq)) {
    if (Object.hasOwn(log.withheldPropertySeq, key) && log.withheldPropertySeq[key]! > afterSeq) {
      withheldSet.add(key)
    }
  }
  const masked: Record<string, unknown> = {}
  for (const key of Object.keys(log.latestMaskSeq)) {
    if (!Object.hasOwn(log.latestMaskSeq, key)) continue
    const entry = log.latestMaskSeq[key]!
    if (entry.seq > afterSeq) masked[key] = entry.mask
  }
  for (const key of Object.keys(log.refuseMaskSeq)) {
    if (!Object.hasOwn(log.refuseMaskSeq, key)) continue
    const entry = log.refuseMaskSeq[key]!
    if (entry.seq > afterSeq) {
      masked[key] = entry.mask
      withheldSet.add(key)
    }
  }
  if (!withheld && withheldSet.size === 0 && Object.keys(masked).length === 0) return undefined
  return {
    withheld,
    masked,
    withheldProperties: [...withheldSet],
  }
}

/** What the page draws from its runtime: all of it from the machine and the server's answers. */
export interface RuntimeView {
  readonly snapshot: Snapshot
  readonly engine: EngineState
  readonly reads: ReadonlyMap<string, BindingReadState>
  readonly calls: ReadonlyMap<string, BindingCallState>
  /** Monotonic per-binding L6 masking/withholding seen (survives synchronous `ok -> pending` batching). */
  readonly readSecurity?: ReadonlyMap<string, ReadSecurity>
  /** Watermarked per-binding `ok`-read security log so clean restores reset prior withholding cleanly. */
  readonly readSecurityLog?: ReadonlyMap<string, ReadSecurityLog>
  /** Messages waiting for the engine. */
  readonly waiting: number
  /** The last step the engine refused (it fired nothing; the page stayed where it was), until a person's next step. */
  readonly fault: Error | null
}

export interface MachineRuntime extends RuntimeView {
  readonly evaluate: LazyEngine["evaluate"]
  readonly send: (msg: Msg) => void
  /** A person's Retry: load the engine again; the waiting messages replay once it is here. */
  readonly retryEngine: () => void
  /** Record a form's current local draft (and optional pinned baseline record) so `ActionBinding` maps from it. */
  readonly setDraft?: (formId: string, draft: Readonly<Record<string, unknown>>, pinnedRecord?: PinnedRecord) => void
  /** Record a selectable list's selected records paired with their canonical envelope row ids so `ActionBinding` (`input.selection`) maps from it. */
  readonly setSelection?: (bindingId: string, records: readonly Readonly<Record<string, unknown>>[], ids: readonly string[]) => void
  /** Submit a form's declared action binding directly when no machine event owns the form node. */
  readonly submitForm?: (formId: string, bindingId: string, draft: Readonly<Record<string, unknown>>, pinnedRecord?: PinnedRecord) => void
}

export interface MachineOptions {
  readonly ux: WidgetUx
  readonly machine: Machine
  readonly reader: WidgetReader
  readonly calls: WidgetCalls
  readonly live?: WidgetLive
  readonly engine: EngineLoader
  readonly context: Readonly<Record<string, unknown>>
  readonly objectTypeOf: (artifactId: string) => string | undefined
  readonly actionIdOf: (artifactId: string) => string | undefined
  readonly navigate?: (itemId: string) => void
  readonly clock?: Clock
}

/** One runtime's inputs: the workspace's reader and calls are fixed for its life; the rest is read as the host last gave it. */
export interface PageRuntimeOptions {
  readonly ux: WidgetUx
  readonly machine: Machine
  readonly reader: WidgetReader
  readonly calls: WidgetCalls
  readonly engine: LazyEngine
  readonly host: () => Pick<MachineOptions, "live" | "context" | "objectTypeOf" | "actionIdOf" | "navigate">
  readonly clock?: Clock
}

export interface PageRuntime {
  readonly view: () => RuntimeView
  readonly send: (msg: Msg) => void
  readonly retryEngine: () => void
  readonly setDraft: (formId: string, draft: Readonly<Record<string, unknown>>, pinnedRecord?: PinnedRecord) => void
  readonly setSelection: (bindingId: string, records: readonly Readonly<Record<string, unknown>>[], ids: readonly string[]) => void
  readonly submitForm: (formId: string, bindingId: string, draft: Readonly<Record<string, unknown>>, pinnedRecord?: PinnedRecord) => void
  /** Start the engine loading, as the page does after its first paint. */
  readonly preload: () => void
  readonly dispose: () => void
}

/** A read document's values (an object record's `values`), or the document itself. */
function valuesOf(document: unknown): unknown {
  const values = typeof document === "object" && document !== null ? (document as { readonly values?: unknown }).values : undefined
  return values ?? document
}

/** `work`, unless `signal` aborts first: a pending state's deadline or Cancel never waits on the engine. */
function abortable<T>(work: Promise<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted) return Promise.reject(signal.reason)
  let onAbort: () => void = () => {}
  const aborted = new Promise<never>((_, reject) => {
    onAbort = () => reject(signal.reason)
    signal.addEventListener("abort", onAbort, { once: true })
  })
  return Promise.race([work, aborted]).finally(() => signal.removeEventListener("abort", onAbort))
}

const errorOf = (error: unknown): Error => (error instanceof Error ? error : new Error(String(error)))

/** The view of a page that has not started: its initial state, nothing read, nothing called. */
export function initialView(machine: Machine, engine: LazyEngine): RuntimeView {
  const state = engine.state()
  return {
    snapshot: init(machine).snapshot,
    engine: state,
    reads: new Map(),
    calls: new Map(),
    readSecurity: new Map(),
    readSecurityLog: new Map(),
    waiting: 0,
    fault: null,
  }
}

/**
 * Runs one page machine against one workspace's reader and calls, reporting each change to
 * `onChange`. Everything it reads and sends stays its own: a new workspace gets a new runtime.
 */
export function createPageRuntime(options: PageRuntimeOptions, onChange: (view: RuntimeView) => void): PageRuntime {
  const { ux, machine, reader, calls, engine } = options
  const clock = options.clock ?? DEFAULT_CLOCK
  let live = true
  let view = initialView(machine, engine)
  const feeds = new Map<string, { readonly key: string; readonly stop: () => void }>()
  const drafts = new Map<string, Readonly<Record<string, unknown>>>()
  const draftRecords = new Map<string, PinnedRecord>()
  const selections = new Map<string, readonly { readonly id: string; readonly record: Readonly<Record<string, unknown>> }[]>()
  const formKeys = new Map<string, string>()
  const formInflight = new Set<{ readonly controller: AbortController; readonly deadline: unknown }>()
  let wait: unknown = null

  const DEFINITIVE_OUTCOMES = new Set(["accepted", "refused", "pending_approval", "answer_moved"])

  /**
   * Build the exact `WidgetIR` evaluation input slots accepted by `norn-checker-wasm`
   * (`evaluate.rs:parse_expression_inputs`): only `context`, `record`, `recordKey` (paired with
   * `record` and nonempty), `draft`, and `selection` — never unknown top-level slots.
   */
  const actionSources = (binding: WidgetUx["actionBindings"][number], context: Readonly<Record<string, unknown>>): Record<string, unknown> => {
    const pinned = binding.inputs.draftFormId === undefined ? undefined : draftRecords.get(binding.inputs.draftFormId)
    const pinnedDoc = pinned !== undefined && pinned.bindingId === binding.inputs.recordBindingId ? pinned.document : undefined
    const record = binding.inputs.recordBindingId === undefined ? undefined : view.reads.get(binding.inputs.recordBindingId)
    const activeDocument = pinnedDoc !== undefined ? pinnedDoc : (record?.kind === "ok" ? record.document : undefined)
    const recordDoc = typeof activeDocument === "object" && activeDocument !== null && !Array.isArray(activeDocument)
      ? (activeDocument as Readonly<Record<string, unknown>>)
      : undefined
    // Only the slots that hold a record: the engine refuses a present slot that is not one.
    const recordValues = activeDocument !== undefined ? valuesOf(activeDocument) : undefined
    const hasRecord = typeof recordValues === "object" && recordValues !== null && !Array.isArray(recordValues)
    const recordKey = hasRecord && typeof recordDoc?.id === "string" && recordDoc.id !== "" ? recordDoc.id : undefined
    const draft = binding.inputs.draftFormId === undefined ? undefined : drafts.get(binding.inputs.draftFormId)
    const selectionEntries = binding.inputs.selectionBindingId === undefined ? undefined : selections.get(binding.inputs.selectionBindingId)
    const selection = selectionEntries === undefined ? undefined : selectionEntries.map((entry) => entry.record)
    return {
      context,
      ...(hasRecord ? { record: recordValues } : {}),
      ...(recordKey !== undefined ? { recordKey } : {}),
      ...(draft !== undefined ? { draft } : {}),
      ...(selection !== undefined ? { selection } : {}),
    }
  }

  const change = (next: Partial<RuntimeView>) => {
    if (!live) return
    view = { ...view, ...next }
    onChange(view)
  }

  /**
   * Synchronously prune/refresh a collection binding's stored selection BEFORE any machine
   * subscription or read-outcome event is dispatched, so event-triggered actions never consume a
   * removed or withheld row from an earlier snapshot.
   */
  const reconcileRuntimeSelection = (bindingId: string, state: BindingReadState) => {
    const current = selections.get(bindingId)
    if (current === undefined || current.length === 0) return
    if (state.kind !== "ok") {
      selections.set(bindingId, [])
      return
    }
    const page = typeof state.document === "object" && state.document !== null && !Array.isArray(state.document)
      ? (state.document as Readonly<Record<string, unknown>>)
      : null
    const rawRows = Array.isArray(page?.records) ? page.records : Array.isArray(page?.items) ? page.items : null
    if (rawRows === null) return
    const available = new Map<string, Record<string, unknown>>()
    for (let index = 0; index < rawRows.length; index += 1) {
      const item = rawRows[index]
      if (typeof item !== "object" || item === null || Array.isArray(item)) continue
      const row = item as Readonly<Record<string, unknown>>
      if (row.withheld === true) continue
      const rowId =
        typeof row.id === "string" && row.id !== ""
          ? row.id
          : typeof row.key === "string" && row.key !== ""
            ? row.key
            : null
      // Never infer identity from `values.id` or row index when the envelope ID is absent.
      if (rowId === null) continue
      const rowValues = typeof row.values === "object" && row.values !== null && !Array.isArray(row.values)
        ? (row.values as Readonly<Record<string, unknown>>)
        : null
      const flattened: Record<string, unknown> = {}
      if (rowValues !== null) {
        Object.assign(flattened, rowValues)
      } else {
        for (const [key, value] of Object.entries(row)) {
          if (key === "values" || key === "masked" || key === "withheld" || key === "withheldProperties") continue
          flattened[key] = value
        }
      }
      if (typeof row.id === "string" && row.id !== "" && !Object.hasOwn(flattened, "id")) flattened.id = row.id
      available.set(rowId, flattened)
    }
    const pruned: { readonly id: string; readonly record: Readonly<Record<string, unknown>> }[] = []
    for (const entry of current) {
      const fresh = available.get(entry.id)
      if (fresh !== undefined) pruned.push({ id: entry.id, record: fresh })
    }
    selections.set(bindingId, pruned)
  }

  let readSeq = 0

  const extractReadSecurity = (document: unknown): ReadSecurity | null => {
    if (typeof document !== "object" || document === null || Array.isArray(document)) return null
    const doc = document as Readonly<Record<string, unknown>>
    const rawMasked = typeof doc.masked === "object" && doc.masked !== null && !Array.isArray(doc.masked)
      ? (doc.masked as Readonly<Record<string, unknown>>)
      : {}
    const masked: Record<string, unknown> = {}
    const withheldSet = new Set<string>(
      Array.isArray(doc.withheldProperties)
        ? doc.withheldProperties.filter((entry): entry is string => typeof entry === "string")
        : [],
    )
    for (const key of Object.keys(rawMasked)) {
      if (!Object.hasOwn(rawMasked, key)) continue
      const mask = rawMasked[key]
      masked[key] = mask
      if (isRefuseMask(mask)) withheldSet.add(key)
    }
    return {
      withheld: doc.withheld === true,
      masked,
      withheldProperties: [...withheldSet],
    }
  }

  const appendReadSecurityLog = (previous: ReadSecurityLog | undefined, seq: number, security: ReadSecurity): ReadSecurityLog => {
    const withheldPropertySeq: Record<string, number> = Object.assign(
      Object.create(null) as Record<string, number>,
      previous?.withheldPropertySeq,
    )
    for (const prop of security.withheldProperties) {
      withheldPropertySeq[prop] = seq
    }
    const refuseMaskSeq: Record<string, { readonly seq: number; readonly mask: unknown }> = Object.assign(
      Object.create(null) as Record<string, { readonly seq: number; readonly mask: unknown }>,
      previous?.refuseMaskSeq,
    )
    const latestMaskSeq: Record<string, { readonly seq: number; readonly mask: unknown }> = Object.assign(
      Object.create(null) as Record<string, { readonly seq: number; readonly mask: unknown }>,
      previous?.latestMaskSeq,
    )
    for (const key of Object.keys(security.masked)) {
      if (!Object.hasOwn(security.masked, key)) continue
      const mask = security.masked[key]
      latestMaskSeq[key] = { seq, mask }
      if (isRefuseMask(mask)) {
        refuseMaskSeq[key] = { seq, mask }
        withheldPropertySeq[key] = seq
      }
    }
    return {
      latestSeq: seq,
      recordWithheldSeq: security.withheld ? seq : (previous?.recordWithheldSeq ?? 0),
      withheldPropertySeq,
      refuseMaskSeq,
      latestMaskSeq,
    }
  }

  const setRead = (bindingId: string, state: BindingReadState) => {
    reconcileRuntimeSelection(bindingId, state)
    const nextReads = new Map(view.reads).set(bindingId, state)
    let nextSecurity = view.readSecurity
    let nextSecurityLog = view.readSecurityLog
    if (state.kind === "ok") {
      const incoming = extractReadSecurity(state.document)
      if (incoming !== null) {
        readSeq += 1
        const merged = mergeReadSecurity(view.readSecurity?.get(bindingId), incoming)
        nextSecurity = new Map(view.readSecurity).set(bindingId, merged)
        const updatedLog = appendReadSecurityLog(view.readSecurityLog?.get(bindingId), readSeq, incoming)
        nextSecurityLog = new Map(view.readSecurityLog).set(bindingId, updatedLog)
      }
    }
    change({
      reads: nextReads,
      ...(nextSecurity === undefined ? {} : { readSecurity: nextSecurity }),
      ...(nextSecurityLog === undefined ? {} : { readSecurityLog: nextSecurityLog }),
    })
  }
  const setCall = (bindingId: string, state: BindingCallState) => change({ calls: new Map(view.calls).set(bindingId, state) })
  const stopWaiting = () => {
    if (wait !== null) clock.clearTimeout(wait)
    wait = null
  }

  /**
   * While a message waits, the engine has until the wait limit to come: the pending state's own
   * deadline, else {@link ENGINE_WAIT_MS}. Past it the page says the engine is unavailable.
   */
  const armWait = () => {
    if (wait !== null || driver.deferred() === 0) return
    const state = machine.states.find((candidate) => candidate.stateId === driver.snapshot().stateId)
    const limit = state?.role === "pending" && state.deadlineMs !== undefined ? state.deadlineMs : ENGINE_WAIT_MS
    wait = clock.setTimeout(() => {
      wait = null
      if (engine.state() !== "ready") change({ engine: "failed" })
    }, limit)
  }

  const ready = () => {
    stopWaiting()
    change({ engine: "ready" })
    driver.resume()
    change({ waiting: driver.deferred() })
  }

  /**
   * Follow one load attempt to its end, once per attempt: the engine may be loading already (another
   * runtime on the same engine asked for it, as a rebuilt or StrictMode-remounted page does).
   */
  let observed: Promise<void> | null = null
  const observe = (attempt: Promise<void>) => {
    if (attempt === observed) return
    observed = attempt
    attempt.then(
      () => {
        if (live && observed === attempt) ready()
      },
      () => {
        if (!live || observed !== attempt) return
        stopWaiting()
        change({ engine: "failed" })
      },
    )
  }

  /** Load the engine; a person's Retry (`fresh`) starts a new attempt even if one never settled. */
  const loadEngine = (fresh: boolean) => {
    if (!live || engine.state() === "ready") return
    change({ engine: "loading" })
    armWait()
    observe(fresh ? engine.reload() : engine.load())
  }

  /** A message waits for the engine: load it if nothing has asked yet, and bound the wait. */
  const deferred = () => {
    change({ waiting: driver.deferred() })
    const state = engine.state()
    // Ready already (another runtime loaded it): replay now. On a microtask, never inside the driver's own step.
    if (state === "ready") queueMicrotask(() => { if (live) ready() })
    else if (state === "idle") loadEngine(false)
    else {
      if (state === "loading") observe(engine.load())
      if (view.engine !== "failed") armWait()
    }
  }

  const follow = (bindingId: string, read: NonNullable<ReturnType<typeof bindingRead>>) => {
    const feed = options.host().live
    if (feed === undefined) return
    const key = JSON.stringify(read)
    const existing = feeds.get(bindingId)
    if (existing !== undefined) {
      if (existing.key === key) return
      existing.stop()
      feeds.delete(bindingId)
    }
    let active = true
    const entry: { readonly key: string; stop: () => void } = {
      key,
      stop: () => {
        active = false
      },
    }
    feeds.set(bindingId, entry)
    try {
      const unsubscribe = feed.subscribe(read, {
        snapshot: (document) => {
          if (!live || !active) return
          setRead(bindingId, { kind: "ok", document, freshness: "live" })
          driver.dispatch({ kind: "subscription", bindingId })
        },
        lapsed: () => {
          if (!live || !active) return
          const before = view.reads.get(bindingId)
          if (before?.kind === "ok") setRead(bindingId, { ...before, freshness: "stale" })
        },
        refused: () => {
          if (!live || !active) return
          const before = view.reads.get(bindingId)
          if (before?.kind === "ok") setRead(bindingId, { ...before, freshness: "snapshot" })
        },
      })
      if (!live || !active) {
        unsubscribe()
        return
      }
      entry.stop = () => {
        active = false
        unsubscribe()
      }
    } catch (error) {
      active = false
      if (feeds.get(bindingId) === entry) feeds.delete(bindingId)
      throw error
    }
  }

  const transport: Transport = {
    async call(call, input, callOptions) {
      const { context, objectTypeOf, actionIdOf } = options.host()
      if (call.kind === "read") {
        const binding = ux.dataBindings.find((candidate) => candidate.bindingId === call.bindingId)
        const named = ux.contextReads?.find((candidate) => candidate.bindingId === call.bindingId)
        const read = binding !== undefined ? bindingRead(binding, context, objectTypeOf) : named !== undefined ? contextRead(named, context) : null
        if (read === null) return { outcome: "not_served", payload: {} }
        const answer = await reader.widget(read).queryFn({ signal: callOptions.signal })
        if (live && !callOptions.signal.aborted && answer.outcome === "accepted" && followed(machine.events, call.bindingId)) follow(call.bindingId, read)
        return answer
      }
      const binding = call.kind === "action" ? ux.actionBindings.find((candidate) => candidate.bindingId === call.bindingId) : undefined
      const actionId = binding === undefined ? undefined : actionIdOf(binding.action.id)
      if (binding === undefined || actionId === undefined) return { outcome: "not_served", payload: {} }
      // The binding maps its own inputs (`inputMapping`), each a ZEN expression over its declared sources.
      // The record is this runtime's own read, in this workspace: never one another workspace answered.
      const sources = actionSources(binding, context)
      const mapped: Record<string, unknown> = {}
      for (const [member, expression] of Object.entries(binding.inputMapping)) mapped[member] = await abortable(engine.evaluateWith(expression, sources), callOptions.signal)
      // A deadline or Cancel that passed while the engine mapped the inputs sends nothing.
      callOptions.signal.throwIfAborted()
      return calls.call(actionId, { ...mapped, ...input }, callOptions)
    },
  }

  const driver: Driver = createDriver({
    machine,
    evaluate: engine.evaluate,
    transport,
    ...(options.clock === undefined ? {} : { clock: options.clock }),
    onStep: (step) => change({ snapshot: step.snapshot }),
    onStart: (cmd, key) => {
      if (cmd.call.kind === "read") setRead(cmd.call.bindingId, { kind: "pending" })
      else if (cmd.call.kind === "action") setCall(cmd.call.bindingId, { kind: "sending", sent: { input: JSON.stringify(cmd.input), key } })
    },
    onAnswer: (cmd, answer, key) => {
      if (cmd.call.kind === "read") {
        const bindingId = cmd.call.bindingId
        setRead(bindingId, readStateOf(answer, view.reads.get(bindingId), followed(machine.events, bindingId)))
      } else if (cmd.call.kind === "action") {
        setCall(cmd.call.bindingId, callStateOf(answer, { input: JSON.stringify(cmd.input), key }))
      }
    },
    onDefer: deferred,
    onError: (error) => change({ fault: errorOf(error) }),
    navigate: (itemId) => options.host().navigate?.(itemId),
  })
  change({ snapshot: driver.snapshot() })
  driver.dispatch({ kind: "page_opened" })

  const setDraft = (formId: string, draft: Readonly<Record<string, unknown>>, pinnedRecord?: PinnedRecord) => {
    drafts.set(formId, draft)
    if (pinnedRecord !== undefined) draftRecords.set(formId, pinnedRecord)
  }

  const deadlineForBinding = (bindingId: string): number => {
    const pending = machine.states.find(
      (state) => state.role === "pending" && state.awaits?.kind === "action" && state.awaits.bindingId === bindingId && (state.deadlineMs ?? 0) > 0,
    )
    return pending?.deadlineMs ?? DEFAULT_SUBMIT_DEADLINE_MS
  }

  const submitForm = (formId: string, bindingId: string, draft: Readonly<Record<string, unknown>>, pinnedRecord?: PinnedRecord) => {
    setDraft(formId, draft, pinnedRecord)
    if (!live || view.calls.get(bindingId)?.kind === "sending") return
    const { context, actionIdOf } = options.host()
    const binding = ux.actionBindings.find((candidate) => candidate.bindingId === bindingId)
    const actionId = binding === undefined ? undefined : actionIdOf(binding.action.id)
    if (binding === undefined || actionId === undefined) {
      setCall(bindingId, { kind: "not_served" })
      return
    }
    const keyOf = JSON.stringify([bindingId, draft])
    const key = formKeys.get(keyOf) ?? crypto.randomUUID()
    formKeys.set(keyOf, key)
    const sent = { input: JSON.stringify(draft), key }
    setCall(bindingId, { kind: "sending", sent })
    const controller = new AbortController()
    let timedOut = false
    const deadline = clock.setTimeout(() => {
      timedOut = true
      controller.abort(new Error("Submit deadline exceeded."))
    }, deadlineForBinding(bindingId))
    const flight = { controller, deadline }
    formInflight.add(flight)
    const settleFlight = () => {
      clock.clearTimeout(deadline)
      formInflight.delete(flight)
    }
    void (async () => {
      let payload: Record<string, unknown>
      try {
        const entries = Object.entries(binding.inputMapping)
        if (entries.length === 0) {
          payload = { ...draft }
        } else {
          const sources = actionSources(binding, context)
          payload = {}
          for (const [member, expression] of entries) {
            payload[member] = await abortable(engine.evaluateWith(expression, sources), controller.signal)
          }
        }
        controller.signal.throwIfAborted()
      } catch (error) {
        settleFlight()
        if (!live) return
        if (timedOut) {
          setCall(bindingId, callStateOf({ outcome: "timeout", payload: {} }, sent))
          return
        }
        change({ fault: errorOf(error) })
        setCall(bindingId, { kind: "unavailable", sent })
        return
      }
      try {
        const answer = await abortable(calls.call(actionId, payload, { signal: controller.signal, idempotencyKey: key }), controller.signal)
        settleFlight()
        if (!live) return
        const finalAnswer = timedOut ? ({ outcome: "timeout", payload: {} } as const) : answer
        if (DEFINITIVE_OUTCOMES.has(finalAnswer.outcome)) formKeys.delete(keyOf)
        setCall(bindingId, callStateOf(finalAnswer, { input: JSON.stringify(payload), key }))
      } catch {
        settleFlight()
        if (!live) return
        const outcome = timedOut ? "timeout" : "network_failure"
        setCall(bindingId, callStateOf({ outcome, payload: {} }, { input: JSON.stringify(payload), key }))
      }
    })()
  }

  return {
    view: () => view,
    // A person's next step clears the last refusal; a step the engine refuses sets it again.
    send: (msg) => {
      if (view.fault !== null) change({ fault: null })
      driver.dispatch(msg)
      if (driver.deferred() !== view.waiting) change({ waiting: driver.deferred() })
    },
    retryEngine: () => loadEngine(true),
    setDraft,
    setSelection: (bindingId, records, ids) => {
      const entries: { readonly id: string; readonly record: Readonly<Record<string, unknown>> }[] = []
      for (let index = 0; index < records.length; index += 1) {
        const explicit = ids[index]
        if (typeof explicit !== "string" || explicit === "") continue
        entries.push({ id: explicit, record: records[index]! })
      }
      selections.set(bindingId, entries)
    },
    submitForm,
    preload: () => {
      const state = engine.state()
      if (state === "idle") loadEngine(false)
      else if (state === "loading") observe(engine.load())
    },
    dispose: () => {
      live = false
      stopWaiting()
      for (const feed of feeds.values()) feed.stop()
      feeds.clear()
      for (const flight of formInflight) {
        clock.clearTimeout(flight.deadline)
        flight.controller.abort()
      }
      formInflight.clear()
      driver.dispose()
    },
  }
}

const needsEval = (expression: string | undefined): boolean =>
  expression !== undefined && literal(expression) === null

/**
 * Whether a page's machine evaluates any expression: a guard, an assignment or a call's input, an
 * action binding's input mapping it starts, a non-literal field condition (`visibleWhen`,
 * `requiredWhen`, `readOnlyWhen`), or a catalog prop on the page it runs. A page with none never
 * loads the engine.
 */
export function needsEngine(ux: WidgetUx, machine: Machine): boolean {
  const started = new Set<string>()
  for (const transition of machine.transitions) {
    if (transition.guard !== undefined || Object.keys(transition.assign ?? {}).length > 0) return true
    for (const effect of transition.effects ?? []) {
      if (effect.kind !== "start") continue
      if (Object.keys(effect.input ?? {}).length > 0) return true
      if (effect.call.kind === "action") started.add(effect.call.bindingId)
    }
  }
  const page = ux.pages.find((candidate) => candidate.machine === machine)
  const formIds = new Set<string>()
  const fieldIds = new Set<string>()
  if (page !== undefined) {
    for (const node of nodesOf(page.root)) {
      const component = node.component
      if (component.kind === "form") {
        formIds.add(component.formId)
        const form = ux.forms.find((candidate) => candidate.formId === component.formId)
        if (form !== undefined) started.add(form.submitBindingId)
      } else if (component.kind === "field") {
        fieldIds.add(component.fieldId)
      }
    }
  }
  if (ux.actionBindings.some((binding) => started.has(binding.bindingId) && Object.keys(binding.inputMapping).length > 0)) return true
  if (
    ux.fields.some((field) => {
      const used = fieldIds.has(field.fieldId) || ("formId" in field.target && formIds.has(field.target.formId))
      return used && (needsEval(field.visibleWhen?.expression) || needsEval(field.requiredWhen?.expression) || needsEval(field.readOnlyWhen?.expression))
    })
  ) {
    return true
  }
  return page === undefined || nodesOf(page.root).some((node) => node.component.kind === "catalog" && Object.keys(node.component.props ?? {}).length > 0)
}

/** Runs one page machine for as long as the component is mounted. */
export function useWidgetMachine(options: MachineOptions): MachineRuntime {
  const { ux, machine, reader, calls } = options
  const latest = useRef(options)
  latest.current = options
  const engine = useMemo(() => lazyEngine((attempt) => latest.current.engine(attempt)), [])
  const [view, setView] = useState<RuntimeView>(() => initialView(machine, engine))
  const runtimeRef = useRef<PageRuntime | null>(null)

  useEffect(() => {
    // The reader and calls this runtime is given are the ones it uses for its whole life.
    const { clock } = latest.current
    const runtime = createPageRuntime({ ux, machine, reader, calls, engine, host: () => latest.current, ...(clock === undefined ? {} : { clock }) }, setView)
    runtimeRef.current = runtime
    // A rebuilt runtime starts from nothing read and nothing called.
    setView(runtime.view())
    // After the first paint: the engine compiles while the person reads, never before the page draws;
    // and only for a page that has an expression to evaluate. A message that needs it loads it anyway.
    const idle = needsEngine(ux, machine) ? globalThis.setTimeout(runtime.preload, 0) : undefined
    return () => {
      if (idle !== undefined) globalThis.clearTimeout(idle)
      runtime.dispose()
      runtimeRef.current = null
    }
  }, [ux, machine, reader, calls, engine])

  const send = useCallback((msg: Msg) => runtimeRef.current?.send(msg), [])
  const retryEngine = useCallback(() => runtimeRef.current?.retryEngine(), [])
  const setDraft = useCallback((formId: string, draft: Readonly<Record<string, unknown>>, pinnedRecord?: PinnedRecord) => runtimeRef.current?.setDraft(formId, draft, pinnedRecord), [])
  const setSelection = useCallback((bindingId: string, records: readonly Readonly<Record<string, unknown>>[], ids: readonly string[]) => runtimeRef.current?.setSelection(bindingId, records, ids), [])
  const submitForm = useCallback((formId: string, bindingId: string, draft: Readonly<Record<string, unknown>>, pinnedRecord?: PinnedRecord) => runtimeRef.current?.submitForm(formId, bindingId, draft, pinnedRecord), [])

  return {
    ...view,
    evaluate: engine.evaluate,
    send,
    retryEngine,
    setDraft,
    setSelection,
    submitForm,
  }
}
