// Runs a page machine's commands against a transport (UI-MACHINE-1). The driver owns time and I/O
// only: it starts the declared call, races it against the state's deadline, turns a cancel into an
// abort, arms timers, and feeds every answer back as an outcome message. It holds no permission
// logic and never invents an outcome: a call answered before its abort reports what the server said.
//
// Idempotency: one key per (call, request body). A definitive answer (accepted, refused, pending
// approval, answer moved) retires the key; a timeout, network failure or cancel keeps it, so a
// retry of the same request replays instead of committing twice. An edited request gets a new key.

import { type Classified, classifyResponse } from "./outcome.ts"
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
  readonly navigate?: (itemId: string) => void
  readonly newKey?: () => string
  readonly clock?: Clock
}

export interface Driver {
  readonly snapshot: () => Snapshot
  readonly dispatch: (msg: Msg) => Step
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
  fetcher: typeof fetch = fetch,
): Transport {
  return {
    async call(call, input, options) {
      const { url, init } = request(call, input, options.idempotencyKey)
      const response = await fetcher(url, { ...init, signal: options.signal })
      const text = await response.text()
      let body: unknown
      if (text !== "") {
        try {
          body = JSON.parse(text)
        } catch {
          body = undefined
        }
      }
      return classifyResponse(response.status, body)
    },
  }
}

export function createDriver(options: DriverOptions): Driver {
  const { machine, evaluate, transport } = options
  const clock: Clock = options.clock ?? { setTimeout: (callback, ms) => globalThis.setTimeout(callback, ms), clearTimeout: (handle) => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>) }
  const newKey = options.newKey ?? (() => crypto.randomUUID())
  const keys = new Map<string, string>()
  const inflight = new Map<number, InFlight>()
  const timers = new Set<unknown>()
  let disposed = false
  const opened = init(machine)
  let current = opened.snapshot

  const dispatch = (msg: Msg): Step => {
    const step = update(machine, evaluate, current, msg)
    current = step.snapshot
    options.onStep?.(step, msg)
    run(step.cmds)
    return step
  }

  const deliver = (entry: number, classified: Classified) => {
    if (disposed) return
    dispatch({ kind: "outcome", entry, outcome: classified.outcome, ...(classified.category ? { category: classified.category } : {}), payload: classified.payload })
  }

  const start = (cmd: Extract<Cmd, { kind: "start" }>) => {
    const keyOf = JSON.stringify([cmd.call, cmd.input])
    const key = keys.get(keyOf) ?? newKey()
    keys.set(keyOf, key)
    const controller = new AbortController()
    const flight: InFlight = {
      controller,
      ended: null,
      deadline: clock.setTimeout(() => {
        flight.ended ??= "timeout"
        controller.abort()
      }, cmd.deadlineMs),
    }
    inflight.set(cmd.entry, flight)
    transport.call(cmd.call, cmd.input, { signal: controller.signal, idempotencyKey: key }).then(
      (classified) => {
        settle(cmd.entry, flight)
        if (DEFINITIVE.has(classified.outcome)) keys.delete(keyOf)
        deliver(cmd.entry, flight.ended === "timeout" ? { outcome: "timeout", payload: {} } : classified)
      },
      () => {
        settle(cmd.entry, flight)
        deliver(cmd.entry, { outcome: flight.ended ?? "network_failure", payload: {} })
      },
    )
  }

  const settle = (entry: number, flight: InFlight) => {
    clock.clearTimeout(flight.deadline)
    if (inflight.get(entry) === flight) inflight.delete(entry)
  }

  const run = (cmds: readonly Cmd[]) => {
    for (const cmd of cmds) {
      switch (cmd.kind) {
        case "start":
          start(cmd)
          break
        case "cancel": {
          const flight = inflight.get(cmd.entry)
          if (flight) {
            flight.ended ??= "cancelled"
            flight.controller.abort()
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
    dispose: () => {
      disposed = true
      for (const flight of inflight.values()) {
        clock.clearTimeout(flight.deadline)
        flight.controller.abort()
      }
      inflight.clear()
      for (const handle of timers) clock.clearTimeout(handle)
      timers.clear()
    },
  }
}
