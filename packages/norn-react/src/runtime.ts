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
import { useEffect, useMemo, useRef, useState } from "react"
import { createDriver, DEFAULT_CLOCK, type Clock, type Driver, type Transport } from "./machine/driver.ts"
import { type EngineLoader, lazyEngine, type LazyEngine } from "./machine/evaluate.ts"
import { init, type Machine, type Msg, type Snapshot } from "./machine/machine.ts"
import { bindingRead, callStateOf, followed, readStateOf, type BindingCallState, type BindingReadState, type WidgetCalls, type WidgetLive, type WidgetReader } from "./reads.ts"
import type { WidgetUx } from "./ux.ts"

export type EngineState = "idle" | "loading" | "ready" | "failed"

/** How long a message in a state with no deadline of its own waits for the engine before the page says it is unavailable. */
export const ENGINE_WAIT_MS = 15_000

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
  /** Start the engine loading, as the page does after its first paint. */
  readonly preload: () => void
  readonly dispose: () => void
}

/** A read document's values (an object record's `values`), or the document itself. */
function valuesOf(document: unknown): unknown {
  const values = typeof document === "object" && document !== null ? (document as { readonly values?: unknown }).values : undefined
  return values ?? document
}

const GLOBAL_CLOCK: Clock = DEFAULT_CLOCK

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
  const clock = options.clock ?? GLOBAL_CLOCK
  let live = true
  let view = initialView(machine, engine)
  const feeds = new Map<string, () => void>()
  let wait: unknown = null

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
    if (feed === undefined || feeds.has(bindingId)) return
    feeds.set(bindingId, feed.subscribe(read, {
      snapshot: (document) => {
        setRead(bindingId, { kind: "ok", document, freshness: "live" })
        driver.dispatch({ kind: "subscription", bindingId })
      },
      lapsed: () => {
        const before = view.reads.get(bindingId)
        if (before?.kind === "ok") setRead(bindingId, { ...before, freshness: "stale" })
      },
      refused: () => {
        const before = view.reads.get(bindingId)
        if (before?.kind === "ok") setRead(bindingId, { ...before, freshness: "snapshot" })
      },
    }))
  }

  const transport: Transport = {
    async call(call, input, callOptions) {
      const { context, objectTypeOf, actionIdOf } = options.host()
      if (call.kind === "read") {
        const binding = ux.dataBindings.find((candidate) => candidate.bindingId === call.bindingId)
        const read = binding === undefined ? null : bindingRead(binding, context, objectTypeOf)
        if (read === null) return { outcome: "not_served", payload: {} }
        const answer = await reader.widget(read).queryFn({ signal: callOptions.signal })
        if (live && answer.outcome === "accepted" && followed(machine.events, call.bindingId)) follow(call.bindingId, read)
        return answer
      }
      const binding = call.kind === "action" ? ux.actionBindings.find((candidate) => candidate.bindingId === call.bindingId) : undefined
      const actionId = binding === undefined ? undefined : actionIdOf(binding.action.id)
      if (binding === undefined || actionId === undefined) return { outcome: "not_served", payload: {} }
      // The binding maps its own inputs (`inputMapping`), each a ZEN expression over its declared sources.
      // The record is this runtime's own read, in this workspace: never one another workspace answered.
      const record = binding.inputs.recordBindingId === undefined ? undefined : view.reads.get(binding.inputs.recordBindingId)
      // Only the slots that hold a record: the engine refuses a present slot that is not one.
      const recordValues = record?.kind === "ok" ? valuesOf(record.document) : undefined
      const sources = { context, ...(typeof recordValues === "object" && recordValues !== null && !Array.isArray(recordValues) ? { record: recordValues } : {}) }
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

  return {
    view: () => view,
    // A person's next step clears the last refusal; a step the engine refuses sets it again.
    send: (msg) => {
      if (view.fault !== null) change({ fault: null })
      driver.dispatch(msg)
      if (driver.deferred() !== view.waiting) change({ waiting: driver.deferred() })
    },
    retryEngine: () => loadEngine(true),
    preload: () => {
      const state = engine.state()
      if (state === "idle") loadEngine(false)
      else if (state === "loading") observe(engine.load())
    },
    dispose: () => {
      live = false
      stopWaiting()
      for (const stop of feeds.values()) stop()
      driver.dispose()
    },
  }
}

/** Runs one page machine for as long as the component is mounted. */
export function useWidgetMachine(options: MachineOptions): MachineRuntime {
  const { ux, machine, reader } = options
  const latest = useRef(options)
  latest.current = options
  const engine = useMemo(() => lazyEngine((attempt) => latest.current.engine(attempt)), [])
  const [view, setView] = useState<RuntimeView>(() => initialView(machine, engine))
  const runtimeRef = useRef<PageRuntime | null>(null)

  useEffect(() => {
    // The reader and calls this runtime is given are the ones it uses for its whole life.
    const { calls, clock } = latest.current
    const runtime = createPageRuntime({ ux, machine, reader, calls, engine, host: () => latest.current, ...(clock === undefined ? {} : { clock }) }, setView)
    runtimeRef.current = runtime
    // A rebuilt runtime starts from nothing read and nothing called.
    setView(runtime.view())
    // After the first paint: the engine compiles while the person reads, never before the page draws.
    const idle = globalThis.setTimeout(runtime.preload, 0)
    return () => {
      globalThis.clearTimeout(idle)
      runtime.dispose()
      runtimeRef.current = null
    }
  }, [ux, machine, reader, engine])

  return {
    ...view,
    evaluate: engine.evaluate,
    send: (msg) => runtimeRef.current?.send(msg),
    retryEngine: () => runtimeRef.current?.retryEngine(),
  }
}
