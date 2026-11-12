// The client side of live reads (UI-LIVE-1, `norn.live/1`): one socket per page, one wire
// subscription per path however many views listen, and a cache that holds only what the cell last
// sent for each path. It never polls and never computes data: a view shows the cell's answer or
// says it has none yet. An expired token closes the socket with 4401 and the client reconnects at
// once (the host mints a fresh token per upgrade); any other loss reconnects with backoff, and
// after `maxFailures` failed opens the client reports `unavailable` so views can say so.

export const LIVE_PROTOCOL = "norn.live/1"

export interface LiveSnapshot {
  readonly status: number
  readonly digest: string
  readonly body: unknown
}

export type LiveState = "idle" | "connecting" | "open" | "unavailable"

export type LiveListener = (snapshot: LiveSnapshot) => void

/** The slice of the browser WebSocket the client uses (tests pass their own). */
export interface SocketLike {
  readonly readyState: number
  send(data: string): void
  close(code?: number, reason?: string): void
  addEventListener(type: "open" | "close" | "message" | "error", listener: (event: { readonly code?: number; readonly data?: unknown }) => void): void
}

export interface LiveOptions {
  /** The socket URL, read at every (re)connect. */
  readonly url: () => string | Promise<string>
  readonly connect?: (url: string) => SocketLike
  readonly maxFailures?: number
  readonly delay?: (attempt: number) => number
  readonly schedule?: (callback: () => void, ms: number) => unknown
  readonly onState?: (state: LiveState) => void
}

interface Entry {
  readonly id: string
  readonly path: string
  readonly listeners: Set<LiveListener>
  last: LiveSnapshot | undefined
}

const OPEN = 1

export class LiveClient {
  readonly #options: LiveOptions
  readonly #entries = new Map<string, Entry>()
  readonly #byId = new Map<string, Entry>()
  #socket: SocketLike | null = null
  #state: LiveState = "idle"
  #failures = 0
  #next = 0
  #pending = false

  constructor(options: LiveOptions) {
    this.#options = options
  }

  get state(): LiveState {
    return this.#state
  }

  /** The cell's last answer for `path`, if one arrived. */
  snapshot(path: string): LiveSnapshot | undefined {
    return this.#entries.get(path)?.last
  }

  /** Listen to `path`; the last answer, if any, is delivered at once. Returns the unsubscribe. */
  subscribe(path: string, listener: LiveListener): () => void {
    let entry = this.#entries.get(path)
    if (!entry) {
      entry = { id: `s${++this.#next}`, path, listeners: new Set(), last: undefined }
      this.#entries.set(path, entry)
      this.#byId.set(entry.id, entry)
      this.#send({ type: "subscribe", id: entry.id, path })
    }
    entry.listeners.add(listener)
    if (entry.last) listener(entry.last)
    this.#ensure()
    const held = entry
    return () => {
      held.listeners.delete(listener)
      if (held.listeners.size > 0) return
      this.#entries.delete(held.path)
      this.#byId.delete(held.id)
      this.#send({ type: "unsubscribe", id: held.id })
      if (this.#entries.size === 0) this.close()
    }
  }

  close(): void {
    const socket = this.#socket
    this.#socket = null
    socket?.close(1000, "done")
    this.#setState("idle")
  }

  #setState(state: LiveState) {
    if (this.#state === state) return
    this.#state = state
    this.#options.onState?.(state)
  }

  #send(message: unknown) {
    if (this.#socket?.readyState === OPEN) this.#socket.send(JSON.stringify(message))
  }

  #ensure() {
    if (this.#socket !== null || this.#pending || this.#entries.size === 0 || this.#state === "unavailable") return
    this.#pending = true
    this.#setState("connecting")
    void Promise.resolve(this.#options.url()).then(
      (url) => {
        this.#pending = false
        if (this.#entries.size === 0) return this.#setState("idle")
        this.#open(url)
      },
      () => {
        this.#pending = false
        this.#lost(1006)
      },
    )
  }

  #open(url: string) {
    const connect = this.#options.connect ?? ((target: string) => new WebSocket(target, LIVE_PROTOCOL) as unknown as SocketLike)
    let socket: SocketLike
    try {
      socket = connect(url)
    } catch {
      this.#lost(1006)
      return
    }
    this.#socket = socket
    socket.addEventListener("open", () => {
      if (this.#socket !== socket) return
      this.#failures = 0
      this.#setState("open")
      for (const entry of this.#entries.values()) socket.send(JSON.stringify({ type: "subscribe", id: entry.id, path: entry.path }))
    })
    socket.addEventListener("message", (event) => {
      if (this.#socket !== socket || typeof event.data !== "string") return
      let message: { type?: unknown; id?: unknown; status?: unknown; digest?: unknown; body?: unknown }
      try {
        message = JSON.parse(event.data) as typeof message
      } catch {
        return
      }
      const entry = typeof message.id === "string" ? this.#byId.get(message.id) : undefined
      if (!entry || message.type !== "snapshot" || typeof message.status !== "number" || typeof message.digest !== "string") return
      entry.last = { status: message.status, digest: message.digest, body: message.body }
      for (const listener of entry.listeners) listener(entry.last)
    })
    socket.addEventListener("close", (event) => {
      if (this.#socket !== socket) return
      this.#socket = null
      this.#lost(event.code ?? 1006)
    })
  }

  #lost(code: number) {
    if (this.#entries.size === 0) return this.#setState("idle")
    // An expired token is routine: reconnect at once with a fresh one.
    if (code === 4401) {
      this.#setState("connecting")
      this.#ensure()
      return
    }
    this.#failures += 1
    if (this.#failures >= (this.#options.maxFailures ?? 5)) return this.#setState("unavailable")
    this.#setState("connecting")
    const wait = (this.#options.delay ?? ((attempt: number) => Math.min(30_000, 1000 * 2 ** (attempt - 1))))(this.#failures)
    const schedule = this.#options.schedule ?? ((callback: () => void, ms: number) => setTimeout(callback, ms))
    schedule(() => this.#ensure(), wait)
  }
}
