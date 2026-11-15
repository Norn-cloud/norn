import { client, type SessionNotification } from "@agentclientprotocol/sdk"
import { createWebSocketStream } from "@agentclientprotocol/sdk/experimental/ws-client"
import type { ToolResult } from "@/widget/bridge"

/**
 * Sidekick's connection: Studio as an Agent Client Protocol client.
 *
 * The bridge (apps/sidekick-bridge) runs the agent on a private network next to Studio and
 * serves `/sidekick/config.json` and the `/sidekick/ws` socket on the same origin as the page.
 * Where nothing answers there (the public deployment), Sidekick is off.
 *
 * The page proves who it is with a Sidekick grant from Studio's Worker (`POST
 * /api/sidekick/grant`, the person's own /mcp access, minutes long). The grant travels once in
 * `initialize` and again whenever it is renewed; it lives in this object's memory only.
 */

/** A tool result with an MCP Apps view: the bridge forwards what the agent's Norn tool returned. */
export interface ViewDelivery {
  readonly tool: string
  readonly view: string
  readonly input: Record<string, unknown>
  readonly result: ToolResult
}

export interface SidekickEvents {
  readonly update: (notification: SessionNotification) => void
  readonly view: (delivery: ViewDelivery) => void
  readonly closed: (reason: string) => void
}

interface Grant { readonly token: string; readonly expiresAt: number }

/** Renew this long before the grant expires. */
const RENEW_BEFORE_MS = 2 * 60_000

/** Whether this origin serves a Sidekick bridge: its config answers as JSON naming the socket. */
export async function sidekickServed(fetcher: typeof fetch = fetch, signal?: AbortSignal): Promise<boolean> {
  try {
    const response = await fetcher("/sidekick/config.json", { headers: { accept: "application/json" }, cache: "no-store", ...signal === undefined ? {} : { signal } })
    if (!response.ok || !(response.headers.get("content-type") ?? "").includes("application/json")) return false
    const body = (await response.json()) as { sidekick?: unknown; socket?: unknown }
    return body.sidekick === 1 && body.socket === "/sidekick/ws"
  } catch {
    return false
  }
}

/** The Worker's grant for the signed-in person; the reason in words when it refuses. */
export async function fetchGrant(fetcher: typeof fetch = fetch): Promise<Grant> {
  const response = await fetcher("/api/sidekick/grant", { method: "POST", headers: { accept: "application/json" }, credentials: "same-origin" })
  const body = (await response.json().catch(() => null)) as { token?: unknown; expiresAt?: unknown; error?: unknown } | null
  if (!response.ok || typeof body?.token !== "string" || typeof body.expiresAt !== "number") {
    throw new Error(typeof body?.error === "string" ? body.error : "Studio did not grant Sidekick access.")
  }
  return { token: body.token, expiresAt: body.expiresAt }
}

export const socketUrl = (location: Pick<Location, "protocol" | "host">) => `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/sidekick/ws`

export class SidekickSession {
  private renewTimer: ReturnType<typeof setTimeout> | undefined
  private closedReason: string | null = null

  private constructor(
    private readonly connection: ReturnType<ReturnType<typeof client>["connect"]>,
    readonly sessionId: string,
    private readonly getGrant: () => Promise<Grant>,
    private readonly events: SidekickEvents,
  ) {}

  /** Open a socket, initialize with a fresh grant and start one session. */
  static async open(events: SidekickEvents, options: { readonly url?: string; readonly getGrant?: () => Promise<Grant> } = {}): Promise<SidekickSession> {
    const getGrant = options.getGrant ?? (() => fetchGrant())
    const grant = await getGrant()
    const app = client()
      .onNotification("session/update", ({ params }) => events.update(params))
      .onNotification("_norn/tool_result", (value: unknown) => value as ViewDelivery, ({ params }) => events.view(params))
    const connection = app.connect(createWebSocketStream(options.url ?? socketUrl(window.location)))
    try {
      await connection.agent.request("initialize", { protocolVersion: 1, clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false }, _meta: { "norn.cloud/grant": grant.token } })
      const created = await connection.agent.request("session/new", { cwd: "/", mcpServers: [] })
      const session = new SidekickSession(connection, created.sessionId, getGrant, events)
      session.scheduleRenew(grant.expiresAt)
      void connection.closed.then(() => session.ended("The Sidekick connection closed."))
      return session
    } catch (error) {
      connection.close()
      throw error
    }
  }

  private scheduleRenew(expiresAt: number) {
    clearTimeout(this.renewTimer)
    this.renewTimer = setTimeout(() => {
      void this.getGrant()
        .then(async (grant) => { await this.connection.agent.request("_norn/grant", { grant: grant.token }); this.scheduleRenew(grant.expiresAt) })
        .catch(() => this.close("Sidekick's access ended; start a new chat to continue."))
    }, Math.max(10_000, expiresAt - Date.now() - RENEW_BEFORE_MS))
  }

  private ended(reason: string) {
    if (this.closedReason !== null) return
    this.closedReason = reason
    clearTimeout(this.renewTimer)
    this.events.closed(reason)
  }

  get closed(): boolean { return this.closedReason !== null }

  /** One turn: resolves with the agent's stop reason once it is done. */
  async prompt(text: string): Promise<string> {
    const response = await this.connection.agent.request("session/prompt", { sessionId: this.sessionId, prompt: [{ type: "text", text }] })
    return response.stopReason
  }

  /** Stop the current turn. */
  cancel() {
    void this.connection.agent.notify("session/cancel", { sessionId: this.sessionId }).catch(() => undefined)
  }

  /** A view's own follow-up read: one Norn tool, as the person, through the bridge. */
  callTool(name: string, args: Record<string, unknown>): Promise<ToolResult> {
    return this.connection.agent.request<ToolResult>("_norn/call_tool", { name, arguments: args })
  }

  close(reason = "Chat closed.") {
    this.ended(reason)
    this.connection.close()
  }
}
