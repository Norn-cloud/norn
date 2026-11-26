import { client, type RequestPermissionRequest, type RequestPermissionResponse, type SessionNotification } from "@agentclientprotocol/sdk"
import { createWebSocketStream } from "@agentclientprotocol/sdk/experimental/ws-client"
import type { ToolResult } from "@/widget/bridge"
import { defaultFetch, getDisplayedScope, scopeHeaders, type StudioScope } from "../lib/definitions.ts"
import { toolLabels, type ToolLabels } from "./transcript.ts"

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
 *
 * The bridge decides which tool calls need the person's OK first (every Norn tool the cell does not
 * mark read-only) and asks with ACP's `session/request_permission`, offering only "allow once" and
 * "reject". Studio shows the question and returns the person's pick; the bridge checks the pick was
 * one it offered, and the cell still decides the call itself. The page holds no rule of its own.
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
  /** The server's label for each Norn tool, sent by the bridge when a session starts. */
  readonly tools: (labels: ToolLabels) => void
  /** The bridge asks the person before a tool call; resolve with their pick (or `cancelled`). */
  readonly permission: (request: RequestPermissionRequest) => Promise<RequestPermissionResponse>
  readonly closed: (reason: string) => void
}

interface Grant { readonly token: string; readonly expiresAt: number }

/** Renew this long before the grant expires. */
const RENEW_BEFORE_MS = 2 * 60_000

export { sidekickServed } from "./served.ts"

/** The Worker's grant for the signed-in person; the reason in words when it refuses. */
export async function fetchGrant(fetcher: (input: string, init?: RequestInit) => Promise<Response> = defaultFetch, boundScope?: StudioScope | null): Promise<Grant> {
  const scope = boundScope !== undefined ? boundScope : getDisplayedScope()
  const response = await fetcher("/api/sidekick/grant", {
    method: "POST",
    headers: { accept: "application/json", ...scopeHeaders(scope) },
    credentials: "same-origin",
  })
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

  /** Open a socket, initialize with a fresh grant and start one session pinned to the opening workspace. */
  static async open(events: SidekickEvents, options: { readonly url?: string; readonly getGrant?: () => Promise<Grant>; readonly scope?: StudioScope | null } = {}): Promise<SidekickSession> {
    const pinnedScope = options.scope !== undefined ? options.scope : getDisplayedScope()
    const getGrant = options.getGrant ?? (() => fetchGrant(defaultFetch, pinnedScope))
    const grant = await getGrant()
    // Once this session has ended, nothing the socket still delivers reaches the chat, and a question is `cancelled`.
    let session: SidekickSession | null = null
    // An open that failed never hands out a session: what its socket still delivers is dropped too.
    let failed = false
    const ended = () => failed || session?.closed === true
    const app = client()
      .onNotification("session/update", ({ params }) => { if (!ended()) events.update(params) })
      .onNotification("_norn/tool_result", (value: unknown) => value as ViewDelivery, ({ params }) => { if (!ended()) events.view(params) })
      .onNotification("_norn/tools", (value: unknown) => toolLabels(value), ({ params }) => { if (!ended()) events.tools(params) })
      .onRequest("session/request_permission", ({ params }) => ended() ? Promise.resolve<RequestPermissionResponse>({ outcome: { outcome: "cancelled" } }) : events.permission(params))
    const connection = app.connect(createWebSocketStream(options.url ?? socketUrl(window.location)))
    try {
      await connection.agent.request("initialize", { protocolVersion: 1, clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false }, _meta: { "norn.cloud/grant": grant.token } })
      const created = await connection.agent.request("session/new", { cwd: "/", mcpServers: [] })
      const opened = new SidekickSession(connection, created.sessionId, getGrant, events)
      session = opened
      opened.scheduleRenew(grant.expiresAt)
      void connection.closed.then(() => opened.ended("The Sidekick connection closed."))
      return opened
    } catch (error) {
      failed = true
      connection.close()
      throw error
    }
  }

  private scheduleRenew(expiresAt: number) {
    clearTimeout(this.renewTimer)
    if (this.closed) return
    this.renewTimer = setTimeout(() => {
      if (this.closed) return
      void this.getGrant()
        .then(async (grant) => {
          if (this.closed) return
          await this.connection.agent.request("_norn/grant", { grant: grant.token })
          if (!this.closed) this.scheduleRenew(grant.expiresAt)
        })
        .catch(() => { if (!this.closed) this.close("Sidekick's access ended; start a new chat to continue.") })
    }, Math.min(2_147_483_647, Math.max(10_000, expiresAt - Date.now() - RENEW_BEFORE_MS)))
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
