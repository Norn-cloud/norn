// Runs a page machine's commands against a transport (UI-MACHINE-1). The driver owns time and I/O
// only: it starts the declared call, races it against the state's deadline, turns a cancel into an
// abort, arms timers, and feeds every answer back as an outcome message. It holds no permission
// logic and never invents an outcome: a call answered before its abort reports what the server said.
//
// Deferral: guard-free transitions run at once, in plain TypeScript. A message whose transition needs
// the expression engine before it is loaded (`EngineNotReady`) is deferred, with every message after
// it, and replayed in order by `resume` once the engine is here: nothing is dropped or reordered.
//
// A transition whose guard or assignment the engine refuses (`EvaluationError`, a guard that is not a
// Boolean) fires nothing, whether it ran at once or on replay: the page stays where it was, the
// message is dropped, `onError` reports it, and the messages after it still run.
//
// Idempotency: one key per (call, request body). A definitive answer (accepted, refused, pending
// approval, answer moved) retires the key; a timeout, network failure or cancel keeps it, so a
// retry of the same request replays instead of committing twice. An edited request gets a new key.

import { EngineNotReady } from "./evaluate.ts"
import { type Classified, classifyText } from "./outcome.ts"
import { type Cmd, type Evaluate, type Fields, type Machine, type MachineCall, type Msg, type Snapshot, type Step, init, update } from "./machine.ts"

export interface CallOptions {
  readonly signal: AbortSignal
  readonly idempotencyKey: string
}

/** Performs one call or read. Resolves with the server's classified answer; rejects only when no answer arrived. */
export interface Transport {
  call(call: MachineCall, input: Fields, options: CallOptions): Promise<Classified>
}

export interface Clock {
  setTimeout(callback: () => void, ms: number): unknown
  clearTimeout(handle: unknown): void
}

export interface DriverOptions {
  readonly machine: Machine
  readonly evaluate: Evaluate
  readonly transport: Transport
  readonly onStep?: (step: Step, msg: Msg | null) => void
  /** A call starts, under its idempotency key (a retry of the same request keeps it). */
  readonly onStart?: (cmd: Extract<Cmd, { kind: "start" }>, idempotencyKey: string) => void
  /** Every answer to a started call, before its outcome is dispatched: the body the page shows. */
  readonly onAnswer?: (cmd: Extract<Cmd, { kind: "start" }>, answer: Classified, idempotencyKey: string) => void
  /** A message waits for the expression engine (`resume` replays it): called for each message that waits. */
  readonly onDefer?: () => void
  /** A message whose transition the engine refused: it fired nothing and was dropped. */
  readonly onError?: (error: unknown, msg: Msg) => void
  readonly navigate?: (itemId: string) => void
  readonly newKey?: () => string
  readonly clock?: Clock
}

export interface Driver {
  readonly snapshot: () => Snapshot
  readonly dispatch: (msg: Msg) => Step
  /** Replay deferred messages in order; stops again at the first that still needs the engine. */
  readonly resume: () => void
  /** Messages waiting for the expression engine. */
  readonly deferred: () => number
  readonly dispose: () => void
}

const DEFINITIVE = new Set(["accepted", "refused", "pending_approval", "answer_moved"])

interface InFlight {
  readonly controller: AbortController
  readonly deadline: unknown
  ended: "timeout" | "cancelled" | null
}

/** A transport over `fetch` for host routes: the page supplies how a call maps to a request. */
export function fetchTransport(
  request: (call: MachineCall, input: Fields, idempotencyKey: string) => { readonly url: string; readonly init: RequestInit },
  fetcher: (input: string, init?: RequestInit) => Promise<Response> = (input, init) => fetch(input, init),
): Transport {
  return {
    async call(call, input, options) {
      const { url, init } = request(call, input, options.idempotencyKey)
      const response = await fetcher(url, { ...init, signal: options.signal })
      return classifyText(response.status, await response.text())
    },
  }
}

export const DEFAULT_CLOCK: Clock = {
  setTimeout: (callback, ms) => globalThis.setTimeout(callback, ms),
  clearTimeout: (handle) => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>),
}

export function createDriver(options: DriverOptions): Driver {
  const { machine, evaluate, transport } = options
  const clock: Clock = options.clock ?? DEFAULT_CLOCK
  const newKey = options.newKey ?? (() => crypto.randomUUID())
  const keys = new Map<string, string>()
  const inflight = new Map<number, Set<InFlight>>()
  const timers = new Set<unknown>()
  let disposed = false
  let resuming = false
  const opened = init(machine)
  let current = opened.snapshot

  const waiting: Msg[] = []
  const idle = (): Step => ({ snapshot: current, cmds: [], fired: null })

  /**
   * Fire `msg`; null when its transition needs the engine (it waits). A transition the engine refuses
   * fires nothing: the same step whether the message runs at once or is replayed.
   */
  const fire = (msg: Msg): Step | null => {
    let step: Step
    try {
      step = update(machine, evaluate, current, msg)
    } catch (error) {
      if (error instanceof EngineNotReady) return null
      if (options.onError === undefined) console.error(error)
      else options.onError(error, msg)
      return idle()
    }
    current = step.snapshot
    options.onStep?.(step, msg)
    run(step.cmds)
    return step
  }

  const dispatch = (msg: Msg): Step => {
    if (disposed) return idle()
    const step = waiting.length > 0 ? null : fire(msg)
    if (step !== null) return step
    waiting.push(msg)
    options.onDefer?.()
    return idle()
  }

  const resume = () => {
    if (resuming) return
    resuming = true
    try {
      while (!disposed && waiting.length > 0) {
        // Still at the head while it fires, so a message it dispatches lands behind it.
        if (fire(waiting[0]!) === null) return
        waiting.shift()
      }
    } finally {
      resuming = false
    }
  }

  const deliver = (entry: number, classified: Classified) => {
    if (disposed) return
    const code = typeof classified.payload.code === "string" ? classified.payload.code : undefined
    dispatch({ kind: "outcome", entry, outcome: classified.outcome, ...(classified.category ? { category: classified.category } : {}), ...(code === undefined ? {} : { code }), payload: classified.payload })
  }

  const DEFAULT_DEADLINE_MS = 10_000

  const start = (cmd: Extract<Cmd, { kind: "start" }>) => {
    const keyOf = JSON.stringify([cmd.call, cmd.input])
    const key = keys.get(keyOf) ?? newKey()
    keys.set(keyOf, key)
    const controller = new AbortController()
    const timeoutMs = cmd.deadlineMs > 0 ? cmd.deadlineMs : DEFAULT_DEADLINE_MS
    const flight: InFlight = {
      controller,
      ended: null,
      deadline: clock.setTimeout(() => {
        flight.ended ??= "timeout"
        controller.abort()
      }, timeoutMs),
    }
    let flights = inflight.get(cmd.entry)
    if (flights === undefined) {
      flights = new Set()
      inflight.set(cmd.entry, flights)
    }
    flights.add(flight)
    options.onStart?.(cmd, key)
    transport.call(cmd.call, cmd.input, { signal: controller.signal, idempotencyKey: key }).then(
      (classified) => {
        settle(cmd.entry, flight)
        if (DEFINITIVE.has(classified.outcome)) keys.delete(keyOf)
        const answer: Classified = flight.ended === "timeout" ? { outcome: "timeout", payload: {} } : classified
        if (!disposed) options.onAnswer?.(cmd, answer, key)
        deliver(cmd.entry, answer)
      },
      () => {
        settle(cmd.entry, flight)
        const answer: Classified = { outcome: flight.ended ?? "network_failure", payload: {} }
        if (!disposed) options.onAnswer?.(cmd, answer, key)
        deliver(cmd.entry, answer)
      },
    )
  }

  const settle = (entry: number, flight: InFlight) => {
    if (flight.deadline !== null) clock.clearTimeout(flight.deadline)
    const flights = inflight.get(entry)
    if (flights !== undefined) {
      flights.delete(flight)
      if (flights.size === 0) inflight.delete(entry)
    }
  }

  const run = (cmds: readonly Cmd[]) => {
    for (const cmd of cmds) {
      switch (cmd.kind) {
        case "start":
          start(cmd)
          break
        case "cancel": {
          const flights = inflight.get(cmd.entry)
          if (flights !== undefined) {
            for (const flight of flights) {
              flight.ended ??= "cancelled"
              flight.controller.abort()
            }
          }
          break
        }
        case "arm": {
          const handle = clock.setTimeout(() => {
            timers.delete(handle)
            if (!disposed) dispatch({ kind: "timer", entry: cmd.entry, eventId: cmd.eventId })
          }, cmd.afterMs)
          timers.add(handle)
          break
        }
        case "navigate":
          options.navigate?.(cmd.itemId)
          break
      }
    }
  }

  options.onStep?.(opened, null)
  run(opened.cmds)
  return {
    snapshot: () => current,
    dispatch,
    resume,
    deferred: () => waiting.length,
    dispose: () => {
      disposed = true
      for (const flights of inflight.values()) {
        for (const flight of flights) {
          if (flight.deadline !== null) clock.clearTimeout(flight.deadline)
          flight.controller.abort()
        }
      }
      inflight.clear()
      for (const handle of timers) clock.clearTimeout(handle)
      timers.clear()
    },
  }
}
