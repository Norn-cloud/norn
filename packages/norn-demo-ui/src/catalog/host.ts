// The host of a catalog component (UI-CATALOG-1, trust ladder rung "contract_checked").
//
// Custom UI code, in any framework, runs sandboxed and gets one typed `norn` handle: the props the
// page binds, the state its declared machine is in, and `emit` for its declared events. The host
// owns the component's state: it runs the entry's admitted machine itself, so a state the contract
// does not declare cannot exist, and an emit is admitted only when the entry declares the event,
// its payload has the declared types, and the current state takes it. Anything else is refused and
// reported as a violation (for telemetry), never forwarded. The component has no fetch, no storage
// and no DOM outside its frame; calls and navigation happen only through the page machine.

import type { Evaluate, Fields, Machine, MachineField, Snapshot } from "../machine/machine.ts"
import { init, update } from "../machine/machine.ts"

export type Trust = "native" | "contract_checked" | "opaque_island"

export interface CatalogEvent {
  readonly eventId: string
  readonly payload?: readonly MachineField[]
}

/** A catalog entry as the admitted contract declares it. */
export interface CatalogEntry {
  readonly entryId: string
  readonly trust: Trust
  readonly framework?: string
  readonly props?: readonly MachineField[]
  readonly events?: readonly CatalogEvent[]
  readonly machine?: Machine
}

export type Violation =
  | { readonly kind: "undeclared_event"; readonly eventId: string }
  | { readonly kind: "ill_typed_payload"; readonly eventId: string; readonly detail: string }
  | { readonly kind: "not_offered"; readonly eventId: string; readonly state: string }
  | { readonly kind: "malformed_message"; readonly detail: string }

/** What the component sees: its props, and where its machine is. */
export interface View {
  readonly props: Fields
  readonly state: string | null
  readonly fields: Fields
}

const DATE = /^\d{4}-\d{2}-\d{2}$/
const TIMESTAMP = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2}(\.\d+)?)?(Z|[+-]\d{2}:\d{2})$/
const DURATION = /^-?P(?!$)(\d+Y)?(\d+M)?(\d+W)?(\d+D)?(T(?=\d)(\d+H)?(\d+M)?(\d+(\.\d+)?S)?)?$/

/** Whether `value` has the declared type (null only where the member is optional). */
export function fits(field: MachineField, value: unknown): boolean {
  if (value === null || value === undefined) return !field.required
  switch (field.type) {
    case "text":
      return typeof value === "string"
    case "integer":
      return typeof value === "number" && Number.isInteger(value)
    case "number":
      return typeof value === "number" && Number.isFinite(value)
    case "boolean":
      return typeof value === "boolean"
    case "date":
      return typeof value === "string" && DATE.test(value)
    case "timestamp":
      return typeof value === "string" && TIMESTAMP.test(value)
    case "duration":
      return typeof value === "string" && DURATION.test(value)
    case "json":
      return true
  }
}

/** The payload, checked member by member against the declaration: no member more, none missing. */
export function checkPayload(declared: readonly MachineField[], payload: unknown): string | null {
  if (typeof payload !== "object" || payload === null || Array.isArray(payload)) return "the payload is not a record"
  const record = payload as Record<string, unknown>
  for (const name of Object.keys(record)) if (!declared.some((field) => field.fieldId === name)) return `undeclared member ${name}`
  for (const field of declared) if (!fits(field, record[field.fieldId])) return `${field.fieldId} is not ${field.required ? "" : "an optional "}${field.type}`
  return null
}

export interface HostOptions {
  readonly entry: CatalogEntry
  readonly evaluate: Evaluate
  /** An admitted emit, for the page machine (raised as its `user` event with `emits`). */
  readonly onEmit: (eventId: string, payload: Fields) => void
  /** A refused message, for telemetry. */
  readonly onViolation: (violation: Violation) => void
  /** The component should redraw. */
  readonly onView: (view: View) => void
}

export interface Host {
  readonly view: () => View
  /** The page's props changed (from its bindings and fields). */
  readonly setProps: (props: Fields) => void
  /** One message from the component. */
  readonly receive: (message: unknown) => void
}

export function createHost(options: HostOptions): Host {
  const { entry, evaluate } = options
  let props: Fields = {}
  let snapshot: Snapshot | null = entry.machine ? init(entry.machine).snapshot : null
  const view = (): View => ({ props, state: snapshot?.stateId ?? null, fields: snapshot?.fields ?? {} })
  const refuse = (violation: Violation) => options.onViolation(violation)
  return {
    view,
    setProps(next) {
      props = next
      options.onView(view())
    },
    receive(message) {
      const msg = message as { type?: unknown; eventId?: unknown; payload?: unknown }
      if (typeof msg !== "object" || msg === null || msg.type !== "emit" || typeof msg.eventId !== "string") {
        return refuse({ kind: "malformed_message", detail: "expected {type: \"emit\", eventId, payload}" })
      }
      const eventId = msg.eventId
      const declared = entry.events?.find((event) => event.eventId === eventId)
      const internal = entry.machine?.events.find((event) => event.eventId === eventId && event.source.kind === "user")
      if (!declared && !internal) return refuse({ kind: "undeclared_event", eventId })
      const payload = (msg.payload ?? {}) as Fields
      const problem = checkPayload(declared?.payload ?? internal?.payload ?? [], payload)
      if (problem !== null) return refuse({ kind: "ill_typed_payload", eventId, detail: problem })
      // The entry's machine decides whether the current state takes the event; it is the only state.
      if (entry.machine && snapshot && internal) {
        const step = update(entry.machine, evaluate, snapshot, { kind: "user", eventId, payload })
        if (step.fired === null) return refuse({ kind: "not_offered", eventId, state: snapshot.stateId })
        snapshot = step.snapshot
        options.onView(view())
      }
      if (declared) options.onEmit(eventId, payload)
    },
  }
}

/**
 * The sandboxed frame a component runs in: scripts only, no same-origin access (so no storage, no
 * cookies, no parent DOM), and a content security policy that allows no network at all; the one
 * channel is a `MessagePort`. `moduleSource` is the component's implementation, inlined.
 */
export function frameDocument(moduleSource: string): string {
  const csp = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:"
  const bridge = `
    const ready = new Promise((resolve) => addEventListener("message", (event) => { if (event.data === "norn.port") resolve(event.ports[0]) }, { once: true }));
    window.norn = {
      async connect(render) {
        const port = await ready;
        port.onmessage = (event) => render(event.data);
        return { emit: (eventId, payload = {}) => port.postMessage({ type: "emit", eventId, payload }) };
      },
    };`
  return `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${csp}"><script>${bridge}</script></head><body><script type="module">${moduleSource.replaceAll("</script", "<\\/script")}</script></body></html>`
}

/** A mounted frame: `show` sends the component a new view; `unmount` removes it. */
export interface Frame {
  readonly show: (view: View) => void
  readonly unmount: () => void
}

/** Mount a component's frame in `container`, wired to `host` (pass `frame.show` as the host's `onView`). */
export function mountFrame(container: HTMLElement, moduleSource: string, host: Host, title: string): Frame {
  const frame = container.ownerDocument.createElement("iframe")
  frame.setAttribute("sandbox", "allow-scripts")
  frame.setAttribute("title", title)
  frame.srcdoc = frameDocument(moduleSource)
  const channel = new MessageChannel()
  channel.port1.onmessage = (event) => host.receive(event.data)
  let connected = false
  frame.addEventListener("load", () => {
    // The sandboxed frame has an opaque origin, so the port is handed over with "*"; it carries no data.
    frame.contentWindow?.postMessage("norn.port", "*", [channel.port2])
    connected = true
    channel.port1.postMessage(host.view())
  }, { once: true })
  container.replaceChildren(frame)
  return {
    show: (view) => {
      if (connected) channel.port1.postMessage(view)
    },
    unmount: () => {
      channel.port1.close()
      frame.remove()
    },
  }
}
