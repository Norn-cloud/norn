/**
 * The widget's side of the MCP Apps bridge (io.modelcontextprotocol/ui, 2026-01-26): JSON-RPC
 * over postMessage with the chat host (Claude, ChatGPT). The host hands the widget its tool's
 * input and result; the widget asks for more only by calling the same server tools through the
 * host, as the person, never by fetching anything itself (the sandbox allows no connections).
 */

export interface ToolResult {
  readonly content?: readonly { readonly type: string; readonly text?: string }[]
  readonly structuredContent?: unknown
  readonly isError?: boolean
}

export interface HostContext {
  readonly theme?: "light" | "dark"
  readonly locale?: string
  readonly displayMode?: string
}

type Message = { readonly jsonrpc: "2.0"; readonly id?: number | string; readonly method?: string; readonly params?: unknown; readonly result?: unknown; readonly error?: { readonly message?: string } }

/** A request the host did not answer in time; the widget says so instead of waiting forever. */
export class HostTimeout extends Error {}

export const UI_PROTOCOL = "2026-01-26"

export class Bridge {
  private next = 1
  private readonly pending = new Map<number, { resolve: (value: unknown) => void; reject: (error: Error) => void }>()
  private readonly listeners = new Map<string, ((params: unknown) => void)[]>()

  constructor(private readonly host: Window, listen: (handler: (event: MessageEvent) => void) => void = (handler) => window.addEventListener("message", handler)) {
    listen((event) => this.receive(event))
  }

  private receive(event: MessageEvent) {
    // Only the frame that embeds the widget speaks for the host.
    if (event.source !== this.host) return
    const message = event.data as Message | null
    if (message === null || typeof message !== "object" || message.jsonrpc !== "2.0") return
    if (typeof message.id === "number" && message.method === undefined) {
      const waiting = this.pending.get(message.id)
      if (waiting === undefined) return
      this.pending.delete(message.id)
      if (message.error !== undefined) waiting.reject(new Error(message.error.message ?? "The host refused the request."))
      else waiting.resolve(message.result)
      return
    }
    if (typeof message.method !== "string") return
    // A host request (for example ui/resource-teardown) is acknowledged; the widget holds nothing to release.
    if (message.id !== undefined) this.host.postMessage({ jsonrpc: "2.0", id: message.id, result: {} }, "*")
    for (const listener of this.listeners.get(message.method) ?? []) listener(message.params)
  }

  request<T>(method: string, params: unknown, timeoutMs = 30_000): Promise<T> {
    const id = this.next++
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new HostTimeout(`${method} got no answer from the host.`)) }, timeoutMs)
      this.pending.set(id, { resolve: (value) => { clearTimeout(timer); resolve(value as T) }, reject: (error) => { clearTimeout(timer); reject(error) } })
      this.host.postMessage({ jsonrpc: "2.0", id, method, params }, "*")
    })
  }

  notify(method: string, params: unknown = {}) {
    this.host.postMessage({ jsonrpc: "2.0", method, params }, "*")
  }

  on(method: string, listener: (params: unknown) => void): () => void {
    const list = [...this.listeners.get(method) ?? [], listener]
    this.listeners.set(method, list)
    return () => this.listeners.set(method, (this.listeners.get(method) ?? []).filter((entry) => entry !== listener))
  }

  /** Handshake: the host's context (theme, locale), then the initialized notification. */
  async connect(): Promise<HostContext> {
    const answer = await this.request<{ hostContext?: HostContext }>("ui/initialize", { protocolVersion: UI_PROTOCOL, appInfo: { name: "norn", version: "1" }, appCapabilities: {} })
    this.notify("ui/notifications/initialized")
    return answer?.hostContext ?? {}
  }

  /** Call one of this server's tools through the host, as the person who granted the assistant. */
  callTool(name: string, args: Record<string, unknown>): Promise<ToolResult> {
    return this.request<ToolResult>("tools/call", { name, arguments: args }, 60_000)
  }

  /** Ask the host to open a link (the sandbox opens nothing itself). */
  openLink(url: string) {
    void this.request("ui/open-link", { url }).catch(() => undefined)
  }

  /** Tell the host how tall the widget is, so it sizes the frame without scrolling. */
  reportSize(width: number, height: number) {
    this.notify("ui/notifications/size-changed", { width: Math.ceil(width), height: Math.ceil(height) })
  }
}

/** The exact JSON text of a tool's document: its first text block, which carries every decimal as the server wrote it. */
export function documentText(result: ToolResult): string | null {
  const text = result.content?.find((block) => block.type === "text" && typeof block.text === "string")?.text
  return typeof text === "string" ? text : null
}
