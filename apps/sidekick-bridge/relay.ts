/**
 * Sidekick bridge: Studio's in-app agent over the Agent Client Protocol (ACP).
 *
 * One Bun process on the agent host. Each browser WebSocket is one ACP connection: the relay
 * starts one Claude Code ACP adapter (`claude-agent-acp`, stdio, newline-delimited JSON-RPC) for
 * it and passes ACP messages both ways, rewriting the few that decide what the agent may do.
 *
 * Trust, in order:
 * - The browser proves who it is with a Sidekick grant: a short-lived access token Studio's
 *   Worker mints for the signed-in person (`POST /api/sidekick/grant`), audience `<studio>/mcp`,
 *   `azp` `norn-studio-sidekick`. No ACP message reaches the adapter before `initialize` carries
 *   a grant that verifies against the Worker's own signing keys, and `session/new` opens only
 *   after the Worker's /mcp answered `initialize` with that grant.
 * - The agent's only tools are the Norn MCP server. Built-in tools are off (`tools: []`), every
 *   built-in name is disallowed as well, no settings, skills, plugins or other MCP servers load,
 *   nothing is persisted, and the working directory is an empty scratch directory. Any permission
 *   request for a tool outside the Norn server is denied here; file system and terminal requests
 *   from the agent are refused.
 * - The grant never reaches the agent. The agent's MCP server is this relay
 *   (`http://127.0.0.1:<port>/mcp/<capability>`, a random per-connection path); the relay adds the
 *   person's current grant on the way to Studio's /mcp, so the cell's PDP decides every call as
 *   that person. The grant lives in this process's memory for the connection only, is never
 *   logged, and is scrubbed from every message the relay forwards.
 *
 * Tool results that carry an MCP Apps view (`_meta.ui.resourceUri`) are also sent to the browser
 * as `_norn/tool_result`, so Studio renders the same view inline; the view's own follow-up tool
 * calls come back as `_norn/call_tool` and go out with the same grant.
 *
 * This is a demo bridge on one subscription login; a production Sidekick needs an API-key provider.
 */
import { createRemoteJWKSet, jwtVerify, type JWTVerifyGetKey } from "jose"
import { mkdirSync } from "node:fs"
import { join } from "node:path"
import type { Server, ServerWebSocket, Subprocess } from "bun"

export const SIDEKICK_CLIENT = "norn-studio-sidekick"
export const MCP_SERVER_NAME = "norn"

/** Every built-in Claude Code tool, disallowed by name on top of `tools: []`. */
export const BUILT_IN_TOOLS = [
  "Agent", "AskUserQuestion", "Bash", "BashOutput", "Edit", "EnterPlanMode", "ExitPlanMode", "Glob", "Grep", "KillShell",
  "ListMcpResourcesTool", "LSP", "MultiEdit", "NotebookEdit", "NotebookRead", "Read", "ReadMcpResourceTool", "Skill",
  "SlashCommand", "Task", "TaskOutput", "TaskStop", "TodoRead", "TodoWrite", "ToolSearch", "WebFetch", "WebSearch", "Write",
  "Monitor", "CronCreate", "CronDelete", "CronList", "EnterWorktree", "ExitWorktree", "RemoteTrigger", "ScheduleWakeup",
]

export const SYSTEM_PROMPT = [
  "You are Sidekick, the assistant inside Norn Studio. You act for the signed-in person through the Norn tools only;",
  "Norn's policy decides what they may see and do, so never claim more than a tool returned.",
  "Answer from tool results. Prefer norn_answer for numbers, norn_trace_answer for where a number comes from,",
  "norn_inbox and norn_get_findings for what needs attention. Studio shows each tool's result as a card next to your words,",
  "so keep your text short: one or two sentences that say what the card shows and what to do next.",
  "Masked values stay masked: never guess or reconstruct them. If a tool refuses, say so in its words.",
].join(" ")

export interface Config {
  /** Studio's origin: the grant's issuer, whose /mcp the agent reaches and whose keys verify grants. */
  readonly studioOrigin: string
  /** Origins whose pages may open a socket (the private persona origins). */
  readonly allowedOrigins: ReadonlySet<string>
  /** The header the private-network proxy sets to the caller's login, and the logins allowed to connect. */
  readonly loginHeader: string
  readonly allowedLogins: ReadonlySet<string>
  readonly listenHost: string
  readonly listenPort: number
  /** The loopback port the agent's MCP client reaches this relay on. */
  readonly mcpPort: number
  /** The adapter command: the runtime and `claude-agent-acp`'s entry, space-separated in the environment. */
  readonly adapter: readonly string[]
  /** An empty directory sessions run in. */
  readonly scratch: string
  /** At most this many connections (one adapter process each) at once. */
  readonly maxConnections: number
  /** The model the adapter uses, if not its default. */
  readonly model?: string
}

export function configFromEnv(env: Record<string, string | undefined>): Config {
  const need = (name: string) => {
    const value = env[name]?.trim()
    if (!value) throw new Error(`${name} is required`)
    return value
  }
  const list = (name: string) => new Set((env[name] ?? "").split(",").map((entry) => entry.trim()).filter(Boolean))
  const studioOrigin = new URL(need("SIDEKICK_STUDIO_ORIGIN")).origin
  return {
    studioOrigin,
    allowedOrigins: list("SIDEKICK_ALLOWED_ORIGINS"),
    // Fails closed: the relay does not start without the proxy's login header and at least one login.
    loginHeader: need("SIDEKICK_LOGIN_HEADER").toLowerCase(),
    allowedLogins: new Set([...list("SIDEKICK_ALLOWED_LOGINS")].map((login) => login.toLowerCase())),
    listenHost: env.SIDEKICK_LISTEN_HOST ?? "127.0.0.1",
    listenPort: Number(env.SIDEKICK_LISTEN_PORT ?? "17950"),
    mcpPort: Number(env.SIDEKICK_MCP_PORT ?? "17951"),
    adapter: need("SIDEKICK_ADAPTER").split(/\s+/),
    scratch: need("SIDEKICK_SCRATCH"),
    maxConnections: Number(env.SIDEKICK_MAX_CONNECTIONS ?? "3"),
    ...env.SIDEKICK_MODEL ? { model: env.SIDEKICK_MODEL } : {},
  }
}

// ---------------------------------------------------------------- grants

export interface Grant {
  readonly token: string
  readonly subject: string
  readonly expiresAt: number
}

/** Verify a Sidekick grant against Studio's keys: issuer, audience, client and expiry. Null when it fails any. */
export async function verifyGrant(token: unknown, keys: JWTVerifyGetKey, studioOrigin: string, now = Date.now()): Promise<Grant | null> {
  if (typeof token !== "string" || !/^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/.test(token)) return null
  try {
    const { payload } = await jwtVerify(token, keys, { issuer: studioOrigin, audience: `${studioOrigin}/mcp`, requiredClaims: ["sub", "exp", "azp"], currentDate: new Date(now) })
    if (payload.azp !== SIDEKICK_CLIENT || typeof payload.sub !== "string" || typeof payload.exp !== "number") return null
    return { token, subject: payload.sub, expiresAt: payload.exp * 1000 }
  } catch {
    return null
  }
}

/** Replace every occurrence of a secret in a serialized message; the relay never forwards a grant. */
export function scrub(text: string, secrets: readonly string[]): string {
  let out = text
  for (const secret of secrets) if (secret.length >= 16) out = out.split(secret).join("[redacted]")
  return out
}

// ---------------------------------------------------------------- ACP rewrites

type Json = Record<string, unknown>
export type Message = { jsonrpc: "2.0"; id?: number | string | null; method?: string; params?: Json; result?: unknown; error?: unknown }

/** Methods the browser may send the agent. Everything else is refused at the relay. */
export const BROWSER_METHODS = new Set(["initialize", "session/new", "session/prompt", "session/cancel", "_norn/grant", "_norn/call_tool"])

/** The `initialize` the adapter sees: the browser's protocol version, and no client file system or terminal. */
export function rewriteInitialize(params: Json | undefined): Json {
  return {
    protocolVersion: typeof params?.protocolVersion === "number" ? params.protocolVersion : 1,
    clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false },
    clientInfo: { name: "norn-studio-sidekick", version: "1" },
  }
}

/** The `session/new` the adapter sees: whatever the browser asked, only the Norn server, locked options, the scratch cwd. */
export function rewriteNewSession(cwd: string, mcpUrl: string, model?: string): Json {
  return {
    cwd,
    mcpServers: [{ type: "http", name: MCP_SERVER_NAME, url: mcpUrl, headers: [] }],
    _meta: {
      claudeCode: {
        options: {
          tools: [],
          disallowedTools: BUILT_IN_TOOLS,
          allowedTools: [`mcp__${MCP_SERVER_NAME}`],
          settingSources: [],
          strictMcpConfig: true,
          skills: [],
          plugins: [],
          agents: {},
          persistSession: false,
          promptSuggestions: false,
          systemPrompt: { type: "preset", preset: "claude_code", append: SYSTEM_PROMPT },
          ...model === undefined ? {} : { model },
        },
      },
    },
  }
}

/** Only text from the browser reaches the agent: no resource links, files or images. */
export function rewritePrompt(params: Json | undefined): Json | null {
  const sessionId = params?.sessionId
  const prompt = params?.prompt
  if (typeof sessionId !== "string" || !Array.isArray(prompt)) return null
  const text = prompt.filter((block): block is { type: "text"; text: string } => block?.type === "text" && typeof block.text === "string").map(({ text }) => ({ type: "text", text: text.slice(0, 8000) }))
  return text.length === 0 ? null : { sessionId, prompt: text }
}

/** Whether a permission request is for one of the Norn server's tools. */
export function isNornTool(params: Json | undefined): boolean {
  const call = params?.toolCall as Json | undefined
  const meta = call?._meta as { claudeCode?: { toolName?: unknown } } | undefined
  const name = meta?.claudeCode?.toolName ?? call?.title
  return typeof name === "string" && name.startsWith(`mcp__${MCP_SERVER_NAME}__`)
}

/** The relay's own answer to a permission request: allow a Norn tool once, reject anything else. */
export function permissionAnswer(params: Json | undefined): Json {
  const options = Array.isArray(params?.options) ? params.options as { optionId: string; kind: string }[] : []
  const pick = (kinds: string[]) => kinds.map((kind) => options.find((option) => option.kind === kind)).find((option) => option !== undefined)
  const choice = isNornTool(params) ? pick(["allow_once", "allow_always"]) : pick(["reject_once", "reject_always"])
  return choice === undefined ? { outcome: { outcome: "cancelled" } } : { outcome: { outcome: "selected", optionId: choice.optionId } }
}

/** Whether an adapter notification is a session update the browser may see (not usage or quota). */
export function forwardedUpdate(message: Message): boolean {
  if (message.method !== "session/update") return false
  const update = message.params?.update as { sessionUpdate?: unknown } | undefined
  return update?.sessionUpdate !== "usage_update"
}

// ---------------------------------------------------------------- MCP views

/** The MCP Apps view a tool names, from its `tools/list` entry. */
export function viewOf(tool: { _meta?: Json } | undefined): string | null {
  const meta = tool?._meta
  const ui = meta?.ui as { resourceUri?: unknown } | undefined
  const uri = ui?.resourceUri ?? meta?.["ui/resourceUri"] ?? meta?.["openai/outputTemplate"]
  const match = typeof uri === "string" ? /^ui:\/\/norn\/([a-z]+)\.html$/.exec(uri) : null
  return match?.[1] ?? null
}

// ---------------------------------------------------------------- connections

interface Connection {
  readonly id: string
  readonly capability: string
  readonly ws: ServerWebSocket<Data>
  grant: Grant | null
  adapter: Subprocess<"pipe", "pipe", "pipe"> | null
  /** The MCP Apps view of each Norn tool that has one, from tools/list. */
  views: Map<string, string>
  /** Ids the relay used for its own requests to the adapter, mapped back to the browser's id. */
  readonly pendingFromBrowser: Map<string, { id: Message["id"]; method: string }>
  nextId: number
  closed: boolean
}

interface Data { connection?: Connection }

const send = (ws: ServerWebSocket<Data>, message: Message, secrets: readonly string[]) => {
  if (ws.readyState === 1) ws.send(scrub(JSON.stringify(message), secrets))
}

const rpcError = (id: Message["id"], code: number, message: string): Message => ({ jsonrpc: "2.0", id: id ?? null, error: { code, message } })

export function startRelay(config: Config, deps: { keys?: JWTVerifyGetKey; fetcher?: typeof fetch; log?: (line: string) => void } = {}) {
  const keys = deps.keys ?? createRemoteJWKSet(new URL("/api/auth/jwks", config.studioOrigin), { cooldownDuration: 30_000 })
  const fetcher = deps.fetcher ?? fetch
  const log = deps.log ?? ((line: string) => console.log(`${new Date().toISOString()} ${line}`))
  const connections = new Map<string, Connection>()
  const byCapability = new Map<string, Connection>()
  const mcpUrl = `${config.studioOrigin}/mcp`
  const secrets = (connection: Connection) => connection.grant === null ? [] : [connection.grant.token]

  /** One JSON-RPC message to Studio's /mcp as the connection's person. */
  async function studioMcp(connection: Connection, body: string, headers: Record<string, string> = {}): Promise<Response> {
    if (connection.grant === null || connection.grant.expiresAt <= Date.now()) {
      return Response.json({ jsonrpc: "2.0", id: null, error: { code: -32001, message: "The Sidekick grant expired; Studio renews it while the panel is open." } }, { status: 401 })
    }
    return fetcher(mcpUrl, {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json, text/event-stream", ...headers, authorization: `Bearer ${connection.grant.token}` },
      body,
      signal: AbortSignal.timeout(120_000),
    })
  }

  async function listTools(connection: Connection) {
    const response = await studioMcp(connection, JSON.stringify({ jsonrpc: "2.0", id: "relay-tools", method: "tools/list", params: {} }), { "mcp-protocol-version": "2025-06-18" })
    if (!response.ok) return
    const body = await response.json().catch(() => null) as { result?: { tools?: { name: string; _meta?: Json }[] } } | null
    for (const tool of body?.result?.tools ?? []) {
      const view = viewOf(tool)
      if (view !== null) connection.views.set(tool.name, view)
    }
  }

  /** Whether Studio's /mcp accepts this grant right now: the Worker's own answer, not just the signature. */
  async function workerAccepts(connection: Connection): Promise<boolean> {
    const response = await studioMcp(connection, JSON.stringify({ jsonrpc: "2.0", id: "relay-init", method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "norn-sidekick-relay", version: "1" } } }), { "mcp-protocol-version": "2025-06-18" })
    await response.body?.cancel()
    return response.ok
  }

  function toAdapter(connection: Connection, message: Message) {
    connection.adapter?.stdin.write(`${JSON.stringify(message)}\n`)
  }

  function spawnAdapter(connection: Connection) {
    const cwd = join(config.scratch, connection.id)
    mkdirSync(cwd, { recursive: true, mode: 0o700 })
    const child = Bun.spawn([...config.adapter], {
      cwd,
      stdin: "pipe", stdout: "pipe", stderr: "pipe",
      // The adapter needs the login (CLAUDE_CONFIG_DIR) and a PATH; nothing else of the relay's environment.
      env: { PATH: process.env.PATH ?? "/usr/bin:/bin", HOME: process.env.HOME ?? "", CLAUDE_CONFIG_DIR: process.env.CLAUDE_CONFIG_DIR ?? "", DISABLE_AUTOUPDATER: "1", CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION: "false" },
    })
    connection.adapter = child
    void (async () => {
      const decoder = new TextDecoder()
      let buffer = ""
      for await (const chunk of child.stdout) {
        buffer += decoder.decode(chunk, { stream: true })
        let newline: number
        while ((newline = buffer.indexOf("\n")) >= 0) {
          const line = buffer.slice(0, newline).trim()
          buffer = buffer.slice(newline + 1)
          if (line !== "") fromAdapter(connection, line)
        }
      }
    })()
    // The adapter's stderr is drained and counted, never echoed: it can hold prompt text.
    void (async () => { for await (const _ of child.stderr) { /* drained */ } })()
    void child.exited.then((code) => {
      log(`conn ${connection.id} adapter exited ${code}`)
      if (!connection.closed) connection.ws.close(1011, "agent stopped")
    })
  }

  function fromAdapter(connection: Connection, line: string) {
    let message: Message
    try { message = JSON.parse(line) as Message } catch { return }
    // A response to a request the browser sent: restore the browser's id.
    if (message.method === undefined && message.id !== undefined && message.id !== null) {
      const key = String(message.id)
      const pending = connection.pendingFromBrowser.get(key)
      if (pending === undefined) return
      connection.pendingFromBrowser.delete(key)
      send(connection.ws, { ...message, id: pending.id }, secrets(connection))
      return
    }
    if (message.method === undefined) return
    // A request from the agent to the client.
    if (message.id !== undefined) {
      if (message.method === "session/request_permission") {
        const answer = permissionAnswer(message.params)
        log(`conn ${connection.id} permission ${isNornTool(message.params) ? "allowed" : "denied"}`)
        toAdapter(connection, { jsonrpc: "2.0", id: message.id, result: answer })
        return
      }
      // Every other client method (file system, terminal, elicitation) is refused: the browser is never asked.
      toAdapter(connection, rpcError(message.id, -32601, "Sidekick's client offers no file system, terminal or other client methods."))
      return
    }
    // Only the session's own updates go to the browser. The adapter's extensions (account and
    // login status, rate limits, raw SDK messages) describe the host's login, never the person's work.
    if (!forwardedUpdate(message)) return
    send(connection.ws, message, secrets(connection))
  }

  async function fromBrowser(connection: Connection, raw: string) {
    let message: Message
    try { message = JSON.parse(raw) as Message } catch { return send(connection.ws, rpcError(null, -32700, "Parse error"), []) }
    // Responses from the browser: the relay sends it no requests, so there is nothing to route.
    if (message.method === undefined) return
    const method = message.method
    if (!BROWSER_METHODS.has(method)) return message.id === undefined ? undefined : send(connection.ws, rpcError(message.id, -32601, `Sidekick does not offer ${method}.`), secrets(connection))
    if (method === "initialize" || method === "_norn/grant") {
      const grant = await verifyGrant((message.params?._meta as Json | undefined)?.["norn.cloud/grant"] ?? message.params?.grant, keys, config.studioOrigin)
      if (grant === null || (connection.grant !== null && grant.subject !== connection.grant.subject)) {
        send(connection.ws, rpcError(message.id, -32001, "Sign in to Studio again: the Sidekick grant was not accepted."), [])
        log(`conn ${connection.id} grant refused`)
        connection.ws.close(4401, "grant refused")
        return
      }
      connection.grant = grant
      if (method === "_norn/grant") return send(connection.ws, { jsonrpc: "2.0", id: message.id ?? null, result: { expiresAt: grant.expiresAt } }, secrets(connection))
      if (connection.adapter === null) spawnAdapter(connection)
    }
    if (connection.grant === null) {
      send(connection.ws, rpcError(message.id, -32001, "Initialize with a Sidekick grant first."), [])
      connection.ws.close(4401, "no grant")
      return
    }
    let params: Json | null | undefined = message.params
    if (method === "initialize") params = rewriteInitialize(message.params)
    if (method === "session/new") {
      if (!(await workerAccepts(connection))) {
        send(connection.ws, rpcError(message.id, -32001, "Studio did not accept the Sidekick grant."), secrets(connection))
        connection.ws.close(4403, "worker refused grant")
        return
      }
      await listTools(connection).catch(() => undefined)
      params = rewriteNewSession(join(config.scratch, connection.id), `http://127.0.0.1:${config.mcpPort}/mcp/${connection.capability}`, config.model)
    }
    if (method === "session/prompt") params = rewritePrompt(message.params)
    if (params === null) return send(connection.ws, rpcError(message.id, -32602, "Send text only."), secrets(connection))
    if (method === "_norn/call_tool") {
      // A view's follow-up read: one Norn tool, as the person.
      const name = message.params?.name
      if (typeof name !== "string" || !name.startsWith("norn_")) return send(connection.ws, rpcError(message.id, -32602, "Only Norn tools."), secrets(connection))
      const response = await studioMcp(connection, JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: message.params?.arguments ?? {} } }), { "mcp-protocol-version": "2025-06-18" })
      const body = await response.json().catch(() => null) as { result?: unknown; error?: { message?: string } } | null
      return send(connection.ws, body?.result !== undefined ? { jsonrpc: "2.0", id: message.id ?? null, result: body.result } : rpcError(message.id, -32000, body?.error?.message ?? `Norn answered ${response.status}.`), secrets(connection))
    }
    const outgoing: Message = { jsonrpc: "2.0", method, ...params === undefined ? {} : { params } }
    if (message.id !== undefined && message.id !== null) {
      const id = `b${connection.nextId++}`
      connection.pendingFromBrowser.set(id, { id: message.id, method })
      outgoing.id = id
    }
    toAdapter(connection, outgoing)
  }

  /** The agent's MCP client reaches Studio through here, so the grant never enters the agent. */
  const mcp = Bun.serve({
    hostname: "127.0.0.1",
    port: config.mcpPort,
    async fetch(request) {
      const match = /^\/mcp\/([A-Za-z0-9_-]{32,})$/.exec(new URL(request.url).pathname)
      const connection = match === null ? undefined : byCapability.get(match[1] as string)
      if (connection === undefined || connection.closed) return new Response("Not found", { status: 404 })
      if (request.method !== "POST") return new Response(null, { status: 405, headers: { allow: "POST" } })
      const body = await request.text()
      if (body.length > 262_144) return new Response("Too large", { status: 413 })
      const forward: Record<string, string> = {}
      for (const name of ["mcp-protocol-version", "mcp-session-id"]) { const value = request.headers.get(name); if (value !== null) forward[name] = value }
      const response = await studioMcp(connection, body, forward)
      const text = await response.text()
      let call: { method?: string; params?: { name?: unknown; arguments?: unknown } } | null = null
      try { call = JSON.parse(body) } catch { /* not one message */ }
      if (call?.method === "tools/call" && typeof call.params?.name === "string") {
        const view = connection.views.get(call.params.name)
        let result: unknown
        try { result = (JSON.parse(text) as { result?: unknown }).result } catch { /* streamed or not JSON */ }
        if (view !== undefined && result !== undefined) send(connection.ws, { jsonrpc: "2.0", method: "_norn/tool_result", params: { tool: call.params.name, view, input: call.params.arguments ?? {}, result } }, secrets(connection))
        log(`conn ${connection.id} tool ${call.params.name} ${response.status}`)
      }
      const headers = new Headers({ "content-type": response.headers.get("content-type") ?? "application/json", "cache-control": "no-store" })
      const session = response.headers.get("mcp-session-id")
      if (session !== null) headers.set("mcp-session-id", session)
      return new Response(text, { status: response.status, headers })
    },
  })

  const server: Server<Data> = Bun.serve<Data, never>({
    hostname: config.listenHost,
    port: config.listenPort,
    idleTimeout: 120,
    fetch(request, server) {
      const url = new URL(request.url)
      const path = url.pathname.replace(/^\/sidekick/, "") || "/"
      const login = request.headers.get(config.loginHeader)?.toLowerCase() ?? ""
      if (!config.allowedLogins.has(login)) return new Response("Forbidden\n", { status: 403 })
      if (path === "/config.json") return Response.json({ sidekick: 1, socket: "/sidekick/ws" }, { headers: { "cache-control": "no-store" } })
      if (path !== "/ws") return new Response("Not found\n", { status: 404 })
      const origin = request.headers.get("origin") ?? ""
      if (!config.allowedOrigins.has(origin)) return new Response("Forbidden origin\n", { status: 403 })
      if (connections.size >= config.maxConnections) return new Response("Sidekick is busy; try again shortly.\n", { status: 503 })
      return server.upgrade(request, { data: {} }) ? undefined : new Response("Upgrade required\n", { status: 426 })
    },
    websocket: {
      maxPayloadLength: 65_536,
      open(ws) {
        const id = crypto.randomUUID()
        const capability = Buffer.from(crypto.getRandomValues(new Uint8Array(32))).toString("base64url")
        const connection: Connection = { id, capability, ws, grant: null, adapter: null, views: new Map(), pendingFromBrowser: new Map(), nextId: 1, closed: false }
        ws.data.connection = connection
        connections.set(id, connection)
        byCapability.set(capability, connection)
        log(`conn ${id} open (${connections.size})`)
        // A socket that never initializes is closed.
        setTimeout(() => { if (connection.grant === null && !connection.closed) ws.close(4401, "no grant") }, 15_000)
      },
      message(ws, data) {
        const connection = ws.data.connection
        if (connection !== undefined) void fromBrowser(connection, typeof data === "string" ? data : new TextDecoder().decode(data))
      },
      close(ws) {
        const connection = ws.data.connection
        if (connection === undefined) return
        connection.closed = true
        connection.grant = null
        connection.adapter?.kill()
        connections.delete(connection.id)
        byCapability.delete(connection.capability)
        log(`conn ${connection.id} closed (${connections.size})`)
      },
    },
  })
  log(`sidekick relay on ${config.listenHost}:${server.port}, agent MCP on 127.0.0.1:${mcp.port}, studio ${config.studioOrigin}`)
  return { server, mcp, connections, stop: () => { server.stop(true); mcp.stop(true); for (const connection of connections.values()) connection.adapter?.kill() } }
}

if (import.meta.main) startRelay(configFromEnv(process.env))
