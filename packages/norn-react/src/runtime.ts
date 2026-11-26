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

/** What the page draws from its runtime: all of it from the machine and the server's answers. */
export interface RuntimeView {
  readonly snapshot: Snapshot
  readonly engine: EngineState
  readonly reads: ReadonlyMap<string, BindingReadState>
  readonly calls: ReadonlyMap<string, BindingCallState>
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
  /** Record a selectable list's selected records so `ActionBinding` (`input.selection`) maps from it. */
  readonly setSelection?: (bindingId: string, records: readonly Readonly<Record<string, unknown>>[]) => void
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
  readonly setSelection: (bindingId: string, records: readonly Readonly<Record<string, unknown>>[]) => void
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
  return { snapshot: init(machine).snapshot, engine: state, reads: new Map(), calls: new Map(), waiting: 0, fault: null }
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
  const selections = new Map<string, readonly Readonly<Record<string, unknown>>[]>()
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
    const selection = binding.inputs.selectionBindingId === undefined ? undefined : selections.get(binding.inputs.selectionBindingId)
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
  const setRead = (bindingId: string, state: BindingReadState) => change({ reads: new Map(view.reads).set(bindingId, state) })
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
    setSelection: (bindingId, records) => {
      selections.set(bindingId, records)
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
  const setSelection = useCallback((bindingId: string, records: readonly Readonly<Record<string, unknown>>[]) => runtimeRef.current?.setSelection(bindingId, records), [])
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
