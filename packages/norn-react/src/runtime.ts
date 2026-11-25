/**
 * The React runtime of one page's `WidgetMachine` (UI-MACHINE-1, #1762 milestone C).
 *
 * `useWidgetMachine` runs the admitted machine with the framework-free driver (`machine/driver.ts`):
 * guard-free transitions fire at once, in plain TypeScript; a transition that needs a ZEN guard,
 * assignment or input waits for the checker's engine, which loads lazily after the page's first
 * paint (`lazyEngine`), so the WASM never sits on the initial load path (DESIGN.md: WASM loaded on
 * intent, LCP ≤ 2.5s). Reads go through the decided workspace's reader and calls through the
 * host's calls; each answer becomes a closed `ReadState` / `ActionCallState` the page draws.
 */
import { useEffect, useMemo, useRef, useState } from "react"
import { createDriver, type Clock, type Driver, type Transport } from "./machine/driver.ts"
import { type CheckerEvaluator, lazyEngine, type LazyEngine } from "./machine/evaluate.ts"
import { init, type Machine, type Msg, type Snapshot } from "./machine/machine.ts"
import { bindingRead, callStateOf, followed, readStateOf, type BindingCallState, type BindingReadState, type WidgetCalls, type WidgetLive, type WidgetReader } from "./reads.ts"
import type { WidgetUx } from "./ux.ts"

export type EngineState = "idle" | "loading" | "ready" | "failed"

export interface MachineRuntime {
  readonly snapshot: Snapshot
  readonly engine: EngineState
  readonly evaluate: LazyEngine["evaluate"]
  readonly send: (msg: Msg) => void
  readonly reads: ReadonlyMap<string, BindingReadState>
  readonly calls: ReadonlyMap<string, BindingCallState>
}

export interface MachineOptions {
  readonly ux: WidgetUx
  readonly machine: Machine
  readonly reader: WidgetReader
  readonly calls: WidgetCalls
  readonly live?: WidgetLive
  readonly engine: () => Promise<CheckerEvaluator>
  readonly context: Readonly<Record<string, unknown>>
  readonly objectTypeOf: (artifactId: string) => string | undefined
  readonly actionIdOf: (artifactId: string) => string | undefined
  readonly navigate?: (itemId: string) => void
  readonly clock?: Clock
}

/** A read document's values (an object record's `values`), or the document itself. */
function valuesOf(document: unknown): unknown {
  const values = typeof document === "object" && document !== null ? (document as { readonly values?: unknown }).values : undefined
  return values ?? document
}

/** Runs one page machine for as long as the component is mounted. */
export function useWidgetMachine(options: MachineOptions): MachineRuntime {
  const { ux, machine, reader } = options
  const latest = useRef(options)
  latest.current = options
  const engine = useMemo(() => lazyEngine(() => latest.current.engine()), [])
  const [snapshot, setSnapshot] = useState<Snapshot>(() => init(machine).snapshot)
  const [engineState, setEngineState] = useState<EngineState>(engine.state())
  const [reads, setReads] = useState<ReadonlyMap<string, BindingReadState>>(new Map())
  const [calls, setCalls] = useState<ReadonlyMap<string, BindingCallState>>(new Map())
  const driverRef = useRef<Driver | null>(null)
  const latestReads = useRef(reads)
  latestReads.current = reads

  useEffect(() => {
    let live = true
    const feeds = new Map<string, () => void>()
    const set = <T,>(setter: (update: (map: ReadonlyMap<string, T>) => ReadonlyMap<string, T>) => void, key: string, value: T) =>
      setter((map) => new Map(map).set(key, value))
    const loadEngine = () => {
      if (engine.state() === "loading" || engine.state() === "ready") return
      setEngineState("loading")
      engine.load().then(
        () => {
          if (!live) return
          setEngineState("ready")
          driverRef.current?.resume()
        },
        () => {
          if (live) setEngineState("failed")
        },
      )
    }
    const follow = (bindingId: string, read: NonNullable<ReturnType<typeof bindingRead>>) => {
      const { live: feed } = latest.current
      if (feed === undefined || feeds.has(bindingId)) return
      feeds.set(bindingId, feed.subscribe(read, {
        snapshot: (document) => {
          set(setReads, bindingId, { kind: "ok", document, freshness: "live" } as BindingReadState)
          driverRef.current?.dispatch({ kind: "subscription", bindingId })
        },
        lapsed: () => setReads((map) => {
          const before = map.get(bindingId)
          return before?.kind === "ok" ? new Map(map).set(bindingId, { ...before, freshness: "stale" }) : map
        }),
        refused: () => setReads((map) => {
          const before = map.get(bindingId)
          return before?.kind === "ok" ? new Map(map).set(bindingId, { ...before, freshness: "snapshot" }) : map
        }),
      }))
    }
    const transport: Transport = {
      async call(call, input, callOptions) {
        const { reader: current, calls: host, context, objectTypeOf } = latest.current
        if (call.kind === "read") {
          const binding = ux.dataBindings.find((candidate) => candidate.bindingId === call.bindingId)
          const read = binding === undefined ? null : bindingRead(binding, context, objectTypeOf)
          if (read === null) return { outcome: "not_served", payload: {} }
          const answer = await current.widget(read).queryFn({ signal: callOptions.signal })
          if (answer.outcome === "accepted" && followed(machine.events, call.bindingId)) follow(call.bindingId, read)
          return answer
        }
        const binding = call.kind === "action" ? ux.actionBindings.find((candidate) => candidate.bindingId === call.bindingId) : undefined
        const actionId = binding === undefined ? undefined : latest.current.actionIdOf(binding.action.id)
        if (binding === undefined || actionId === undefined) return { outcome: "not_served", payload: {} }
        // The binding maps its own inputs (`inputMapping`), each a ZEN expression over its declared sources.
        const record = binding.inputs.recordBindingId === undefined ? undefined : latestReads.current.get(binding.inputs.recordBindingId)
        // Only the slots that hold a record: the engine refuses a present slot that is not one.
        const recordValues = record?.kind === "ok" ? valuesOf(record.document) : undefined
        const sources = { context, ...(typeof recordValues === "object" && recordValues !== null && !Array.isArray(recordValues) ? { record: recordValues } : {}) }
        const mapped: Record<string, unknown> = {}
        for (const [member, expression] of Object.entries(binding.inputMapping)) mapped[member] = await engine.evaluateWith(expression, sources)
        return host.call(actionId, { ...mapped, ...input }, callOptions)
      },
    }
    const driver = createDriver({
      machine,
      evaluate: engine.evaluate,
      transport,
      ...(options.clock === undefined ? {} : { clock: options.clock }),
      onStep: (step) => {
        if (live) setSnapshot(step.snapshot)
      },
      onStart: (cmd, key) => {
        if (!live) return
        if (cmd.call.kind === "read") set(setReads, cmd.call.bindingId, { kind: "pending" } as BindingReadState)
        else if (cmd.call.kind === "action") set(setCalls, cmd.call.bindingId, { kind: "sending", sent: { input: JSON.stringify(cmd.input), key } } as BindingCallState)
      },
      onAnswer: (cmd, answer, key) => {
        if (!live) return
        if (cmd.call.kind === "read") {
          const bindingId = cmd.call.bindingId
          setReads((map) => new Map(map).set(bindingId, readStateOf(answer, map.get(bindingId), followed(machine.events, bindingId))))
        } else if (cmd.call.kind === "action") {
          set(setCalls, cmd.call.bindingId, callStateOf(answer, { input: JSON.stringify(cmd.input), key }))
        }
      },
      onDefer: loadEngine,
      navigate: (itemId) => latest.current.navigate?.(itemId),
    })
    driverRef.current = driver
    setSnapshot(driver.snapshot())
    driver.dispatch({ kind: "page_opened" })
    // After the first paint: the engine compiles while the person reads, never before the page draws.
    const idle = globalThis.setTimeout(loadEngine, 0)
    return () => {
      live = false
      globalThis.clearTimeout(idle)
      for (const stop of feeds.values()) stop()
      driver.dispose()
      driverRef.current = null
    }
  }, [ux, machine, reader, engine])

  return {
    snapshot,
    engine: engineState,
    evaluate: engine.evaluate,
    send: (msg) => {
      driverRef.current?.dispatch(msg)
    },
    reads,
    calls,
  }
}
