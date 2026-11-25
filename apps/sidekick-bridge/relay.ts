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
 * - The person approves every call that can change something. Only the Norn tools the cell's own
 *   `tools/list` marks `readOnlyHint: true` are pre-allowed; for any other Norn tool the agent must
 *   ask, and the relay forwards that `session/request_permission` to the browser with only the
 *   "allow once" and "reject" options. It forwards back only an option it offered (anything else
 *   is the reject), answers `cancelled` for every open question when the person stops the turn, and
 *   never lets "always allow" through. If `tools/list` fails, nothing is pre-allowed. The session
 *   runs in the manual permission mode (never auto or bypass, whatever the host's settings say;
 *   the relay closes the connection if the adapter reports any other mode), and the relay's own MCP
 *   proxy forwards a non-read-only `tools/call` only when it spends a one-time approval the person
 *   gave for that tool with those arguments. The cell still decides each call as the person.
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
    // Fails closed: the relay does not start without the proxy's login header; with no allowed
    // login it starts and refuses every request with 403.
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

export const ORG_CLAIM = "norn.cloud/org"
export const ENV_CLAIM = "norn.cloud/env"
export const MEMBER_CLAIM = "norn.cloud/member"

export interface Grant {
  readonly token: string
  readonly subject: string
  readonly org: string
  readonly env: string
  readonly member: string
  readonly expiresAt: number
}

const nonEmptyClaim = (value: unknown): string | null =>
  typeof value === "string" && value.trim() !== "" ? value : null

/** True only when two grants belong to the exact same person, workspace, environment, and membership generation. */
export const sameGrantIdentity = (a: Grant, b: Grant): boolean =>
  a.subject === b.subject && a.org === b.org && a.env === b.env && a.member === b.member

/** Verify a Sidekick grant against Studio's keys: issuer, audience, client, scope identity, and expiry. Null when it fails any. */
export async function verifyGrant(token: unknown, keys: JWTVerifyGetKey, studioOrigin: string, now = Date.now()): Promise<Grant | null> {
  if (typeof token !== "string" || !/^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/.test(token)) return null
  try {
    const { payload } = await jwtVerify(token, keys, { issuer: studioOrigin, audience: `${studioOrigin}/mcp`, requiredClaims: ["sub", "exp", "azp"], currentDate: new Date(now) })
    const subject = nonEmptyClaim(payload.sub)
    const org = nonEmptyClaim(payload[ORG_CLAIM])
    const env = nonEmptyClaim(payload[ENV_CLAIM])
    const member = nonEmptyClaim(payload[MEMBER_CLAIM])
    if (payload.azp !== SIDEKICK_CLIENT || subject === null || org === null || env === null || member === null || typeof payload.exp !== "number") return null
    return { token, subject, org, env, member, expiresAt: payload.exp * 1000 }
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

/** The only permission mode a Sidekick session runs in: every non-pre-allowed tool asks (ACP mode id). */
export const MANUAL_MODE = "default"

/** The `session/new` the adapter sees: whatever the browser asked, only the Norn server, locked options, the scratch cwd. */
export function rewriteNewSession(cwd: string, mcpUrl: string, model?: string, readOnly: Iterable<string> = []): Json {
  return {
    cwd,
    mcpServers: [{ type: "http", name: MCP_SERVER_NAME, url: mcpUrl, headers: [] }],
    // The adapter picks the starting mode from the host's settings (auto when none is set); the
    // session is switched to manual before it answers, and the relay checks the answer says so.
    configOptions: [{ configId: "mode", value: MANUAL_MODE }],
    _meta: {
      claudeCode: {
        options: {
          tools: [],
          disallowedTools: BUILT_IN_TOOLS,
          // Only the cell's read-only tools run without asking; every other Norn tool asks the person.
          allowedTools: [...readOnly].filter((name) => /^norn_[a-z0-9_]+$/.test(name)).sort().map((name) => `mcp__${MCP_SERVER_NAME}__${name}`),
          settingSources: [],
          // No bypass mode, whatever the host's settings allow.
          allowDangerouslySkipPermissions: false,
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

/**
 * The Norn tool a permission request names, without the server prefix; null when it is not a Norn
 * tool. The name is read only from `_meta.claudeCode.toolName` (the adapter's tool identity), never
 * from `title`, which is display text; without it the request is not a Norn tool and is refused.
 */
export function nornToolName(params: Json | undefined): string | null {
  const call = params?.toolCall as Json | undefined
  const meta = call?._meta as { claudeCode?: { toolName?: unknown } } | undefined
  const name = meta?.claudeCode?.toolName
  const prefix = `mcp__${MCP_SERVER_NAME}__`
  return typeof name === "string" && name.startsWith(prefix) && name.length > prefix.length ? name.slice(prefix.length) : null
}

/** Whether a permission request is for one of the Norn server's tools. */
export const isNornTool = (params: Json | undefined): boolean => nornToolName(params) !== null

/** A JSON value with object keys sorted, so the same arguments always read the same. */
export function canonicalJson(value: unknown): string {
  const sort = (item: unknown): unknown => Array.isArray(item)
    ? item.map(sort)
    : item !== null && typeof item === "object"
      ? Object.fromEntries(Object.keys(item).sort().map((key) => [key, sort((item as Json)[key])]))
      : item
  return JSON.stringify(sort(value ?? {}))
}

/** One call the person allowed once: the Norn tool and its exact arguments. */
export interface Approval { readonly tool: string; readonly input: string }

/** The MCP methods the proxy forwards without an approval: none of them runs a tool. Anything else but `tools/call` is refused. */
export const PROXY_METHODS: ReadonlySet<string> = new Set(["initialize", "notifications/initialized", "ping", "tools/list", "resources/list", "resources/read", "resources/templates/list"])

const plainObject = (value: unknown): value is Json =>
  value !== null && typeof value === "object" && !Array.isArray(value) && Object.getPrototypeOf(value) === Object.prototype

/**
 * Whether the agent's MCP request may reach Studio. Fails closed: only one plain JSON-RPC object
 * passes (a batch is refused); a method that runs nothing (`PROXY_METHODS`) passes, a read-only tool
 * passes, any other tool passes only by spending one approval the person gave for that tool with
 * those arguments, and every other method is refused.
 */
export function proxyAdmits(body: unknown, readOnly: ReadonlySet<string>, approvals: Approval[]): boolean {
  if (!plainObject(body)) return false
  if (body.method !== "tools/call") return typeof body.method === "string" && PROXY_METHODS.has(body.method)
  const call = body as { params?: { name?: unknown; arguments?: unknown } }
  if (!plainObject(call.params)) return false
  const name = call.params.name
  if (typeof name !== "string") return false
  if (readOnly.has(name)) return true
  const input = canonicalJson(call.params?.arguments)
  const at = approvals.findIndex((approval) => approval.tool === name && approval.input === input)
  if (at < 0) return false
  approvals.splice(at, 1)
  return true
}

/** A JSON number literal as an exact decimal: sign, significant digits and exponent; "0" for zero, "NaN" for anything else. */
export function exactDecimal(literal: string): string {
  const parts = /^(-?)(\d+)(?:\.(\d+))?(?:[eE]([+-]?\d+))?$/.exec(literal)
  if (parts === null) return "NaN"
  const fraction = parts[3] ?? ""
  const digits = `${parts[2]}${fraction}`.replace(/^0+/, "")
  if (digits === "") return "0"
  const significant = digits.replace(/0+$/, "")
  const exponent = BigInt(parts[4] ?? "0") - BigInt(fraction.length) + BigInt(digits.length - significant.length)
  return `${parts[1]}${significant}e${exponent}`
}

const NUMBER = /-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/y

/**
 * Whether every number in a JSON text survives JSON.parse exactly: re-serialised, it is the same
 * decimal. Studio reads numbers as exact decimals, so a number a double rounds (2^53 + 1, 1e-400,
 * 0.1000000000000000000001) would reach Studio as something other than what the person was shown.
 * `text` must already be valid JSON.
 */
export function exactNumbers(text: string): boolean {
  for (let at = 0; at < text.length; at += 1) {
    const char = text[at] as string
    if (char === '"') {
      for (at += 1; at < text.length && text[at] !== '"'; at += text[at] === "\\" ? 2 : 1) { /* inside a string */ }
      continue
    }
    if (char !== "-" && (char < "0" || char > "9")) continue
    NUMBER.lastIndex = at
    const literal = NUMBER.exec(text)?.[0]
    if (literal === undefined) return false
    // Past a double's range the literal parses to ±Infinity or 0: decided here, before exactDecimal reads
    // a possibly enormous exponent as a big integer. A literal that parses to 0 is exact only as a written zero.
    const value = Number(literal)
    if (!Number.isFinite(value)) return false
    if (value === 0 ? /[1-9]/.test(literal.split(/[eE]/)[0] as string) : exactDecimal(literal) !== exactDecimal(String(value))) return false
    at += literal.length - 1
  }
  return true
}

/** The tools the cell's own `tools/list` marks read-only: the only ones that run without asking. */
export function readOnlyTools(tools: readonly { name: string; annotations?: Json }[]): Set<string> {
  return new Set(tools.filter((tool) => tool.annotations?.readOnlyHint === true).map((tool) => tool.name))
}

type Option = { optionId: string; kind: string; name?: string }
const CANCELLED: Json = { outcome: { outcome: "cancelled" } }
const selected = (optionId: string): Json => ({ outcome: { outcome: "selected", optionId } })

/** A question for the person: the two options the relay lets through, and the reject it falls back to. */
export interface Question { readonly options: readonly Option[]; readonly reject: string }

/**
 * What the relay does with the agent's permission request: answer it itself (reject a tool outside
 * the Norn server, allow a tool the cell marks read-only, cancel when the options it needs are
 * missing), or ask the person, offering only "allow once" and "reject".
 */
export function permissionDecision(params: Json | undefined, readOnly: ReadonlySet<string>): { readonly answer: Json } | { readonly ask: Question } {
  const options = Array.isArray(params?.options) ? params.options as Option[] : []
  const find = (kind: string) => options.find((option) => option.kind === kind)
  const name = nornToolName(params)
  if (name === null) {
    const reject = find("reject_once") ?? find("reject_always")
    return { answer: reject === undefined ? CANCELLED : selected(reject.optionId) }
  }
  const allow = find("allow_once")
  if (readOnly.has(name)) return { answer: allow === undefined ? CANCELLED : selected(allow.optionId) }
  const reject = find("reject_once")
  if (allow === undefined || reject === undefined) return { answer: CANCELLED }
  return { ask: { options: [allow, reject].map(({ optionId, kind, name: label }) => ({ optionId, kind, name: label ?? (kind === "allow_once" ? "Allow" : "Reject") })), reject: reject.optionId } }
}

/** The person's answer as the agent gets it: an option the relay offered, `cancelled`, or else the reject. */
export function personAnswer(question: Question, reply: Message): Json {
  const outcome = (reply.result as { outcome?: { outcome?: unknown; optionId?: unknown } } | undefined)?.outcome
  if (reply.error === undefined && outcome?.outcome === "selected" && question.options.some((option) => option.optionId === outcome.optionId)) return selected(outcome.optionId as string)
  if (reply.error === undefined && outcome?.outcome === "cancelled") return CANCELLED
  return selected(question.reject)
}

/** The tool call the person is asked about: its id, title, kind and input, nothing else of the agent's. */
export function questionFor(params: Json | undefined, question: Question): Json {
  const call = (params?.toolCall ?? {}) as Json
  const name = nornToolName(params)
  return {
    sessionId: params?.sessionId,
    toolCall: {
      toolCallId: call.toolCallId,
      ...typeof call.title === "string" ? { title: call.title } : {},
      ...typeof call.kind === "string" ? { kind: call.kind } : {},
      ...call.rawInput !== undefined ? { rawInput: call.rawInput } : {},
      _meta: { claudeCode: { toolName: `mcp__${MCP_SERVER_NAME}__${name}` } },
    },
    options: question.options,
  }
}

/** Whether an adapter notification is a session update the browser may see (not usage or quota). */
export function forwardedUpdate(message: Message): boolean {
  if (message.method !== "session/update") return false
  const update = message.params?.update as { sessionUpdate?: unknown } | undefined
  return update?.sessionUpdate !== "usage_update"
}

/** Whether every mode config option (id or category `mode`) in a list is set to the manual mode; true when there is none. */
export function manualConfigOptions(options: unknown): boolean {
  if (options === undefined || options === null) return true
  if (!Array.isArray(options)) return false
  return options.every((option: { id?: unknown; category?: unknown; currentValue?: unknown } | null) =>
    option === null || typeof option !== "object" || (option.id !== "mode" && option.category !== "mode") || option.currentValue === MANUAL_MODE)
}

/** Whether the adapter's `session/new` answer puts the session in the manual permission mode (its mode and any mode config option). */
export function manualMode(result: unknown): boolean {
  const answer = result as { modes?: { currentModeId?: unknown }; configOptions?: unknown } | undefined
  return answer?.modes?.currentModeId === MANUAL_MODE && manualConfigOptions(answer.configOptions)
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
  /** The Norn tools the cell marks read-only, from tools/list: the only ones pre-allowed. */
  readOnly: Set<string>
  /** Permission questions forwarded to the person, by the relay's id, with the agent's request id and the call asked about. */
  readonly questions: Map<string, { adapterId: Message["id"]; question: Question; approval: Approval }>
  /** Calls the person allowed once and the agent has not made yet; cleared when the turn ends or stops. */
  readonly approvals: Approval[]
  /** Messages from the browser, handled one at a time in the order they came. */
  inbox: Promise<void>
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

  /** The cell's tools, every page of them; the views and the read-only set change only when all pages arrived. */
  async function listTools(connection: Connection) {
    const tools: { name: string; _meta?: Json; annotations?: Json }[] = []
    let cursor: string | undefined
    for (let page = 0; page < 64; page += 1) {
      const response = await studioMcp(connection, JSON.stringify({ jsonrpc: "2.0", id: "relay-tools", method: "tools/list", params: cursor === undefined ? {} : { cursor } }), { "mcp-protocol-version": "2025-06-18" })
      if (!response.ok) throw new Error(`tools/list answered ${response.status}`)
      const body = await response.json() as { result?: { tools?: { name: string; _meta?: Json; annotations?: Json }[]; nextCursor?: unknown } }
      if (!Array.isArray(body.result?.tools)) throw new Error("tools/list answered no tools")
      tools.push(...body.result.tools)
      if (typeof body.result.nextCursor !== "string" || body.result.nextCursor === "") {
        connection.views = new Map(tools.flatMap((tool) => { const view = viewOf(tool); return view === null ? [] : [[tool.name, view] as const] }))
        connection.readOnly = readOnlyTools(tools)
        return
      }
      cursor = body.result.nextCursor
    }
    throw new Error("tools/list never ended")
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

  /** The turn ended or stopped: open questions answer `cancelled`, unspent approvals lapse. */
  function endTurn(connection: Connection) {
    for (const asked of connection.questions.values()) toAdapter(connection, { jsonrpc: "2.0", id: asked.adapterId, result: CANCELLED })
    connection.questions.clear()
    connection.approvals.length = 0
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
      if (pending.method === "session/new" && message.error === undefined && !manualMode(message.result)) {
        // Fail closed: a session in any mode but manual could run a tool without asking.
        log(`conn ${connection.id} session refused: not in the manual permission mode`)
        send(connection.ws, rpcError(pending.id, -32003, "Sidekick could not start a session that asks before every change."), secrets(connection))
        connection.ws.close(1011, "permission mode")
        return
      }
      if (pending.method === "session/prompt") endTurn(connection)
      send(connection.ws, { ...message, id: pending.id }, secrets(connection))
      return
    }
    if (message.method === undefined) return
    // A request from the agent to the client.
    if (message.id !== undefined) {
      if (message.method === "session/request_permission") {
        const decision = permissionDecision(message.params, connection.readOnly)
        if ("answer" in decision) {
          log(`conn ${connection.id} permission ${(decision.answer.outcome as Json).outcome === "selected" && isNornTool(message.params) ? "allowed (read-only)" : "refused"}`)
          toAdapter(connection, { jsonrpc: "2.0", id: message.id, result: decision.answer })
          return
        }
        // Ask the person: the browser answers with one of the two options, or the reject stands.
        const id = `p${connection.nextId++}`
        const tool = nornToolName(message.params) as string
        connection.questions.set(id, { adapterId: message.id, question: decision.ask, approval: { tool, input: canonicalJson((message.params?.toolCall as Json | undefined)?.rawInput) } })
        log(`conn ${connection.id} permission asked ${nornToolName(message.params)}`)
        send(connection.ws, { jsonrpc: "2.0", id, method: "session/request_permission", params: questionFor(message.params, decision.ask) }, secrets(connection))
        return
      }
      // Every other client method (file system, terminal, elicitation) is refused: the browser is never asked.
      toAdapter(connection, rpcError(message.id, -32601, "Sidekick's client offers no file system, terminal or other client methods."))
      return
    }
    // Only the session's own updates go to the browser. The adapter's extensions (account and
    // login status, rate limits, raw SDK messages) describe the host's login, never the person's work.
    if (!forwardedUpdate(message)) return
    const update = message.params?.update as { sessionUpdate?: unknown; currentModeId?: unknown; configOptions?: unknown } | undefined
    if ((update?.sessionUpdate === "current_mode_update" && update.currentModeId !== MANUAL_MODE) || (update?.sessionUpdate === "config_option_update" && !manualConfigOptions(update.configOptions))) {
      log(`conn ${connection.id} closed: the session left the manual permission mode`)
      connection.ws.close(1011, "permission mode")
      return
    }
    send(connection.ws, message, secrets(connection))
  }

  async function fromBrowser(connection: Connection, raw: string) {
    let message: Message
    try { message = JSON.parse(raw) as Message } catch { return send(connection.ws, rpcError(null, -32700, "Parse error"), []) }
    // Responses from the browser: only the person's answers to the questions the relay forwarded.
    if (message.method === undefined) {
      const asked = typeof message.id === "string" ? connection.questions.get(message.id) : undefined
      if (asked === undefined) return
      connection.questions.delete(message.id as string)
      const answer = personAnswer(asked.question, message)
      const outcome = answer.outcome as Json
      const allowed = outcome.outcome === "selected" && outcome.optionId !== asked.question.reject
      log(`conn ${connection.id} permission ${outcome.outcome === "cancelled" ? "cancelled" : allowed ? "allowed" : "declined"} by the person`)
      // The proxy lets this one call through, once, with exactly these arguments.
      if (allowed) connection.approvals.push(asked.approval)
      toAdapter(connection, { jsonrpc: "2.0", id: asked.adapterId, result: answer })
      return
    }
    const method = message.method
    if (!BROWSER_METHODS.has(method)) return message.id === undefined ? undefined : send(connection.ws, rpcError(message.id, -32601, `Sidekick does not offer ${method}.`), secrets(connection))
    if (method === "initialize" || method === "_norn/grant") {
      const grant = await verifyGrant((message.params?._meta as Json | undefined)?.["norn.cloud/grant"] ?? message.params?.grant, keys, config.studioOrigin)
      if (grant === null || (connection.grant !== null && !sameGrantIdentity(connection.grant, grant))) {
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
      // Only a request: the relay checks the adapter's answer for the manual mode, and a notification has none.
      if (message.id === undefined || message.id === null) { log(`conn ${connection.id} session/new without an id refused`); return }
      // A new session ends the last one's turn: its open questions answer `cancelled` and its approvals lapse.
      endTurn(connection)
      if (!(await workerAccepts(connection))) {
        send(connection.ws, rpcError(message.id, -32001, "Studio did not accept the Sidekick grant."), secrets(connection))
        connection.ws.close(4403, "worker refused grant")
        return
      }
      // A new session starts from nothing pre-allowed; if the listing fails, nothing is.
      connection.readOnly = new Set()
      connection.views = new Map()
      await listTools(connection).catch(() => { connection.readOnly = new Set(); connection.views = new Map() })
      params = rewriteNewSession(join(config.scratch, connection.id), `http://127.0.0.1:${config.mcpPort}/mcp/${connection.capability}`, config.model, connection.readOnly)
    }
    if (method === "session/prompt") params = rewritePrompt(message.params)
    if (method === "session/cancel") {
      // Stopping the turn cancels every question still open (ACP: pending permissions answer `cancelled`)
      // and every approval the agent has not spent.
      endTurn(connection)
    }
    if (params === null) return send(connection.ws, rpcError(message.id, -32602, "Send text only."), secrets(connection))
    if (method === "_norn/call_tool") {
      // A view's follow-up read: one Norn tool, as the person.
      const name = message.params?.name
      if (typeof name !== "string" || !name.startsWith("norn_")) return send(connection.ws, rpcError(message.id, -32602, "Only Norn tools."), secrets(connection))
      // Runs beside the inbox: a slow read never holds up the person's answers or Stop.
      void (async () => {
        const response = await studioMcp(connection, JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: message.params?.arguments ?? {} } }), { "mcp-protocol-version": "2025-06-18" })
        const body = await response.json().catch(() => null) as { result?: unknown; error?: { message?: string } } | null
        send(connection.ws, body?.result !== undefined ? { jsonrpc: "2.0", id: message.id ?? null, result: body.result } : rpcError(message.id, -32000, body?.error?.message ?? `Norn answered ${response.status}.`), secrets(connection))
      })().catch(() => send(connection.ws, rpcError(message.id, -32000, "Norn did not answer."), secrets(connection)))
      return
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
      const raw = await request.text()
      if (raw.length > 262_144) return new Response("Too large", { status: 413 })
      let call: { id?: Message["id"]; method?: string; params?: { name?: unknown; arguments?: unknown } } | null = null
      try { call = JSON.parse(raw) } catch { return Response.json(rpcError(null, -32700, "Parse error"), { status: 400 }) }
      const id = plainObject(call) ? call.id : null
      // A number a double would round reaches Studio as another number than the one checked and shown: refused.
      if (!exactNumbers(raw)) {
        log(`conn ${connection.id} tool call refused: a number does not survive exactly`)
        return Response.json(rpcError(id, -32602, "A number in this call cannot be passed on exactly; send it as a string."), { headers: { "cache-control": "no-store" } })
      }
      if (!proxyAdmits(call, connection.readOnly, connection.approvals)) {
        // Defence in depth: whatever the agent's own permission flow did, a call the person did not
        // allow never reaches Studio with their grant.
        log(`conn ${connection.id} tool call refused: not approved by the person`)
        return Response.json(rpcError(id, -32001, "The person did not approve this call; ask them first."), { headers: { "cache-control": "no-store" } })
      }
      const forward: Record<string, string> = {}
      for (const name of ["mcp-protocol-version", "mcp-session-id"]) { const value = request.headers.get(name); if (value !== null) forward[name] = value }
      // Studio gets the object that was checked, re-serialised, never the agent's own bytes: a duplicate
      // key or a number spelled differently cannot reach Studio as anything but what was approved.
      const response = await studioMcp(connection, JSON.stringify(call), forward)
      const text = await response.text()
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
        const connection: Connection = { id, capability, ws, grant: null, adapter: null, views: new Map(), readOnly: new Set(), questions: new Map(), approvals: [], inbox: Promise.resolve(), pendingFromBrowser: new Map(), nextId: 1, closed: false }
        ws.data.connection = connection
        connections.set(id, connection)
        byCapability.set(capability, connection)
        log(`conn ${id} open (${connections.size})`)
        // A socket that never initializes is closed.
        setTimeout(() => { if (connection.grant === null && !connection.closed) ws.close(4401, "no grant") }, 15_000)
      },
      message(ws, data) {
        const connection = ws.data.connection
        if (connection === undefined) return
        // One at a time, in order: a prompt never overtakes the session/new or grant check before it.
        const raw = typeof data === "string" ? data : new TextDecoder().decode(data)
        connection.inbox = connection.inbox.then(() => fromBrowser(connection, raw)).catch((error: unknown) => log(`conn ${connection.id} message failed: ${error instanceof Error ? error.message : "unknown"}`))
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
