import { expect, test } from "bun:test"
import { SignJWT, createLocalJWKSet, exportJWK, generateKeyPair } from "jose"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { BUILT_IN_TOOLS, ENV_CLAIM, MCP_SERVER_NAME, MEMBER_CLAIM, ORG_CLAIM, canonicalJson, configFromEnv, exactDecimal, exactNumbers, manualConfigOptions, PROXY_METHODS, forwardedUpdate, manualMode, nornToolName, SIDEKICK_CLIENT, permissionDecision, personAnswer, proxyAdmits, questionFor, readOnlyTools, rewriteInitialize, rewriteNewSession, rewritePrompt, sameGrantIdentity, scrub, startRelay, verifyGrant, viewOf, type Approval, type Config } from "./relay.ts"

const STUDIO = "https://studio.norn.test"

async function keys() {
  const { publicKey, privateKey } = await generateKeyPair("EdDSA", { crv: "Ed25519" })
  const jwk = { ...(await exportJWK(publicKey)), kid: "k1", alg: "EdDSA" }
  const sign = (claims: Record<string, unknown>, exp = Math.floor(Date.now() / 1000) + 600) =>
    new SignJWT(claims).setProtectedHeader({ alg: "EdDSA", kid: "k1" }).setIssuedAt().setExpirationTime(exp).sign(privateKey)
  return { set: createLocalJWKSet({ keys: [jwk] }), sign }
}

const GRANT = { iss: STUDIO, aud: `${STUDIO}/mcp`, sub: "user-1", azp: SIDEKICK_CLIENT, [ORG_CLAIM]: "org-1", [ENV_CLAIM]: "env-1", [MEMBER_CLAIM]: "mem-1" }

test("only a Sidekick grant from Studio's own keys, for its /mcp, unexpired, with full scope identity, is accepted", async () => {
  const { set, sign } = await keys()
  const good = await verifyGrant(await sign(GRANT), set, STUDIO)
  expect(good).toMatchObject({ subject: "user-1", org: "org-1", env: "env-1", member: "mem-1" })
  // Renewal accepts identical (subject, org, env, member) and rejects any workspace/env/membership/subject change.
  const renewed = await verifyGrant(await sign(GRANT, Math.floor(Date.now() / 1000) + 1200), set, STUDIO)
  expect(good && renewed && sameGrantIdentity(good, renewed)).toBe(true)
  for (const changed of [
    { ...GRANT, [ORG_CLAIM]: "org-2" },
    { ...GRANT, [ENV_CLAIM]: "env-2" },
    { ...GRANT, [MEMBER_CLAIM]: "mem-2" },
    { ...GRANT, sub: "user-2" },
  ]) {
    const next = await verifyGrant(await sign(changed), set, STUDIO)
    expect(next).not.toBeNull()
    expect(good && next && sameGrantIdentity(good, next)).toBe(false)
  }
  // Missing workspace, environment, or membership generation claims are refused.
  for (const omit of [ORG_CLAIM, ENV_CLAIM, MEMBER_CLAIM]) {
    const incomplete = { ...GRANT } as Record<string, unknown>
    delete incomplete[omit]
    expect(await verifyGrant(await sign(incomplete), set, STUDIO)).toBeNull()
  }
  // An assistant's OAuth token (another client), a host token, another issuer, an expired grant, a foreign key: refused.
  expect(await verifyGrant(await sign({ ...GRANT, azp: "claude-connector" }), set, STUDIO)).toBeNull()
  expect(await verifyGrant(await sign({ ...GRANT, aud: "norn-host" }), set, STUDIO)).toBeNull()
  expect(await verifyGrant(await sign({ ...GRANT, iss: "https://elsewhere.test" }), set, STUDIO)).toBeNull()
  expect(await verifyGrant(await sign(GRANT, Math.floor(Date.now() / 1000) - 5), set, STUDIO)).toBeNull()
  const other = await keys()
  expect(await verifyGrant(await other.sign(GRANT), set, STUDIO)).toBeNull()
  expect(await verifyGrant(undefined, set, STUDIO)).toBeNull()
  expect(await verifyGrant("not a token", set, STUDIO)).toBeNull()
})

test("the agent gets no client file system or terminal, whatever the browser declares", () => {
  expect(rewriteInitialize({ protocolVersion: 1, clientCapabilities: { fs: { readTextFile: true, writeTextFile: true }, terminal: true } })).toEqual({
    protocolVersion: 1, clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false }, clientInfo: { name: "norn-studio-sidekick", version: "1" },
  })
})

test("a session has only the relay's Norn server, no built-in tools, settings, skills or persistence, in the scratch cwd", () => {
  const session = rewriteNewSession("/scratch/c1", "http://127.0.0.1:17951/mcp/cap", "claude-sonnet-5-5", new Set(["norn_inbox", "norn_answer", "mcp__x"])) as { cwd: string; mcpServers: unknown[]; _meta: { claudeCode: { options: Record<string, unknown> } } }
  expect(session.cwd).toBe("/scratch/c1")
  expect(session.mcpServers).toEqual([{ type: "http", name: MCP_SERVER_NAME, url: "http://127.0.0.1:17951/mcp/cap", headers: [] }])
  const options = session._meta.claudeCode.options
  expect(options).toMatchObject({ tools: [], settingSources: [], strictMcpConfig: true, skills: [], plugins: [], persistSession: false, allowedTools: ["mcp__norn__norn_answer", "mcp__norn__norn_inbox"], model: "claude-sonnet-5-5" })
  // The session runs in the manual mode, never auto or bypass, whatever the host's settings say.
  expect(options.allowDangerouslySkipPermissions).toBe(false)
  expect((session as unknown as { configOptions: unknown }).configOptions).toEqual([{ configId: "mode", value: "default" }])
  for (const tool of ["Bash", "Edit", "Write", "Read", "WebFetch", "WebSearch", "Task", "Agent", "Glob", "Grep", "NotebookEdit", "Skill"]) expect(options.disallowedTools as string[]).toContain(tool)
  expect(BUILT_IN_TOOLS.length).toBeGreaterThan(20)
  // Without the cell's tool list, nothing is pre-allowed: every Norn tool asks the person.
  const unlisted = rewriteNewSession("/scratch/c1", "http://127.0.0.1:17951/mcp/cap") as { _meta: { claudeCode: { options: Record<string, unknown> } } }
  expect(unlisted._meta.claudeCode.options.allowedTools).toEqual([])
})

test("only text reaches the agent", () => {
  expect(rewritePrompt({ sessionId: "s", prompt: [{ type: "text", text: "what's our stock worth" }, { type: "resource_link", uri: "file:///etc/passwd", name: "x" }, { type: "image", data: "", mimeType: "image/png" }] })).toEqual({ sessionId: "s", prompt: [{ type: "text", text: "what's our stock worth" }] })
  expect(rewritePrompt({ sessionId: "s", prompt: [{ type: "resource_link", uri: "file:///etc/passwd", name: "x" }] })).toBeNull()
  expect(rewritePrompt({ prompt: [] })).toBeNull()
})

const OPTIONS = [{ optionId: "a", kind: "allow_always", name: "Always" }, { optionId: "o", kind: "allow_once", name: "Once" }, { optionId: "r", kind: "reject_once", name: "No" }]
const call = (toolName: string) => ({ sessionId: "s", toolCall: { toolCallId: "t", title: toolName, rawInput: { action: "x.close" }, _meta: { claudeCode: { toolName, secret: "kept back" } } }, options: OPTIONS })

test("only the tools the cell lists as read-only run without asking", () => {
  expect([...readOnlyTools([{ name: "norn_answer", annotations: { readOnlyHint: true } }, { name: "norn_call_action", annotations: { readOnlyHint: false, destructiveHint: true } }, { name: "norn_x" }])]).toEqual(["norn_answer"])
})

test("permissions: a read-only Norn tool is allowed once, another tool is rejected, any other Norn tool asks the person", () => {
  const readOnly = new Set(["norn_answer"])
  expect(permissionDecision(call("mcp__norn__norn_answer"), readOnly)).toEqual({ answer: { outcome: { outcome: "selected", optionId: "o" } } })
  expect(permissionDecision(call("Bash"), readOnly)).toEqual({ answer: { outcome: { outcome: "selected", optionId: "r" } } })
  expect(permissionDecision(call("mcp__other__norn_answer"), readOnly)).toEqual({ answer: { outcome: { outcome: "selected", optionId: "r" } } })
  // "Always allow" never reaches the person: each call is asked on its own.
  const asked = permissionDecision(call("mcp__norn__norn_call_action"), readOnly)
  expect(asked).toEqual({ ask: { options: [{ optionId: "o", kind: "allow_once", name: "Once" }, { optionId: "r", kind: "reject_once", name: "No" }], reject: "r" } })
  // The tool's identity is `_meta.claudeCode.toolName` only: a title is display text, and without the
  // identity the request is refused (fails closed), whatever the title says.
  const titled = { sessionId: "s", toolCall: { toolCallId: "t", title: "mcp__norn__norn_answer", rawInput: {} }, options: OPTIONS }
  expect(nornToolName(titled)).toBeNull()
  expect(permissionDecision(titled, readOnly)).toEqual({ answer: { outcome: { outcome: "selected", optionId: "r" } } })
  const spoofed = { ...titled, toolCall: { ...titled.toolCall, title: "mcp__norn__norn_call_action", _meta: { claudeCode: { toolName: 7 } } } }
  expect(permissionDecision(spoofed, readOnly)).toEqual({ answer: { outcome: { outcome: "selected", optionId: "r" } } })
  expect(nornToolName({ toolCall: { title: "Bash", _meta: { claudeCode: { toolName: "mcp__norn__norn_answer" } } } })).toBe("norn_answer")
  expect(nornToolName({ toolCall: { _meta: { claudeCode: { toolName: "mcp__norn__" } } } })).toBeNull()
  // Without both a one-time allow and a reject to offer, the relay cancels rather than guess.
  expect(permissionDecision({ ...call("mcp__norn__norn_call_action"), options: [{ optionId: "a", kind: "allow_always" }, { optionId: "r", kind: "reject_once" }] }, readOnly)).toEqual({ answer: { outcome: { outcome: "cancelled" } } })
})

test("permissions: the person is asked about the call's id, title and input only, and only an offered option goes back", () => {
  const { ask } = permissionDecision(call("mcp__norn__norn_call_action"), new Set()) as { ask: Parameters<typeof personAnswer>[0] }
  expect(questionFor(call("mcp__norn__norn_call_action"), ask)).toEqual({
    sessionId: "s",
    toolCall: { toolCallId: "t", title: "mcp__norn__norn_call_action", rawInput: { action: "x.close" }, _meta: { claudeCode: { toolName: "mcp__norn__norn_call_action" } } },
    options: ask.options,
  })
  const reply = (result: unknown, error?: unknown) => ({ jsonrpc: "2.0" as const, id: "p1", result, ...error === undefined ? {} : { error } })
  expect(personAnswer(ask, reply({ outcome: { outcome: "selected", optionId: "o" } }))).toEqual({ outcome: { outcome: "selected", optionId: "o" } })
  expect(personAnswer(ask, reply({ outcome: { outcome: "selected", optionId: "r" } }))).toEqual({ outcome: { outcome: "selected", optionId: "r" } })
  expect(personAnswer(ask, reply({ outcome: { outcome: "cancelled" } }))).toEqual({ outcome: { outcome: "cancelled" } })
  // An option the relay never offered (always-allow), a malformed answer or an error is the reject.
  expect(personAnswer(ask, reply({ outcome: { outcome: "selected", optionId: "a" } }))).toEqual({ outcome: { outcome: "selected", optionId: "r" } })
  expect(personAnswer(ask, reply("yes"))).toEqual({ outcome: { outcome: "selected", optionId: "r" } })
  expect(personAnswer(ask, reply(undefined, { code: -1, message: "no" }))).toEqual({ outcome: { outcome: "selected", optionId: "r" } })
})

test("a forwarded message never carries the grant", () => {
  const grant = "eyJhbGciOiJFZERTQSJ9.eyJzdWIiOiJ1In0.c2lnbmF0dXJl"
  expect(scrub(JSON.stringify({ text: `token ${grant} again ${grant}` }), [grant])).toBe(JSON.stringify({ text: "token [redacted] again [redacted]" }))
})

test("a tool's MCP Apps view is read from its listing", () => {
  expect(viewOf({ _meta: { ui: { resourceUri: "ui://norn/answer.html" } } })).toBe("answer")
  expect(viewOf({ _meta: { "openai/outputTemplate": "ui://norn/lineage.html" } })).toBe("lineage")
  expect(viewOf({ _meta: { ui: { resourceUri: "ui://other/answer.html" } } })).toBeNull()
  expect(viewOf(undefined)).toBeNull()
})

test("the browser sees the session's updates, never the host login's status, quota or raw SDK messages", () => {
  expect(forwardedUpdate({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "s", update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "hi" } } } })).toBe(true)
  expect(forwardedUpdate({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "s", update: { sessionUpdate: "usage_update" } } })).toBe(false)
  for (const method of ["_auth/status_update", "auth/status_update", "_claude/sdkMessage"]) expect(forwardedUpdate({ jsonrpc: "2.0", method, params: {} })).toBe(false)
})

test("the relay does not start without the proxy's login header", () => {
  const env = { SIDEKICK_STUDIO_ORIGIN: STUDIO, SIDEKICK_ADAPTER: "/bin/node adapter.js", SIDEKICK_SCRATCH: "/scratch", SIDEKICK_ALLOWED_LOGINS: "me@example" }
  expect(() => configFromEnv(env)).toThrow("SIDEKICK_LOGIN_HEADER")
  expect(configFromEnv({ ...env, SIDEKICK_LOGIN_HEADER: "X-User-Login" })).toMatchObject({ loginHeader: "x-user-login", adapter: ["/bin/node", "adapter.js"], listenHost: "127.0.0.1" })
})

test("the session must be in the manual permission mode", () => {
  expect(manualMode({ sessionId: "s", modes: { currentModeId: "default" } })).toBe(true)
  for (const mode of ["auto", "bypassPermissions", "acceptEdits", "dontAsk", "plan", undefined]) expect(manualMode({ sessionId: "s", modes: { currentModeId: mode } })).toBe(false)
  expect(manualMode({ sessionId: "s" })).toBe(false)
  // A mode config option must say manual too; options without a mode do not matter.
  const option = (currentValue: string, key: Record<string, string> = { id: "mode" }) => ({ ...key, name: "Mode", type: "select", currentValue, options: [] })
  expect(manualMode({ sessionId: "s", modes: { currentModeId: "default" }, configOptions: [option("default"), option("opus", { id: "model", category: "model" })] })).toBe(true)
  expect(manualMode({ sessionId: "s", modes: { currentModeId: "default" }, configOptions: [option("auto")] })).toBe(false)
  expect(manualConfigOptions([option("bypassPermissions", { id: "permissions", category: "mode" })])).toBe(false)
  expect(manualConfigOptions("mode=auto")).toBe(false)
})

test("the proxy lets a call through only if it is read-only or spends the person's one-time approval for exactly those arguments", () => {
  const readOnly = new Set(["norn_answer"])
  const approvals: Approval[] = [{ tool: "norn_call_action", input: canonicalJson({ action: "x.close", input: { id: "R-1", note: "n" } }) }]
  const tools = (name: string, args: unknown) => ({ jsonrpc: "2.0", id: 3, method: "tools/call", params: { name, arguments: args } })
  expect(proxyAdmits({ jsonrpc: "2.0", id: 1, method: "tools/list" }, readOnly, approvals)).toBe(true)
  expect(proxyAdmits(tools("norn_answer", { q: 1 }), readOnly, approvals)).toBe(true)
  expect(proxyAdmits(tools("norn_call_action", { action: "x.delete", input: { id: "R-1", note: "n" } }), readOnly, approvals)).toBe(false)
  expect(proxyAdmits(tools("norn_apply_definitions", { action: "x.close", input: { id: "R-1", note: "n" } }), readOnly, approvals)).toBe(false)
  expect(approvals).toHaveLength(1)
  // Same arguments in another key order: the approval is spent, once.
  expect(proxyAdmits(tools("norn_call_action", { input: { note: "n", id: "R-1" }, action: "x.close" }), readOnly, approvals)).toBe(true)
  expect(approvals).toHaveLength(0)
  expect(proxyAdmits(tools("norn_call_action", { action: "x.close", input: { id: "R-1", note: "n" } }), readOnly, approvals)).toBe(false)
  // A batch or a call without a name is refused.
  expect(proxyAdmits([tools("norn_answer", {})], readOnly, [])).toBe(false)
  expect(proxyAdmits({ jsonrpc: "2.0", id: 4, method: "tools/call", params: {} }, readOnly, [])).toBe(false)
})

test("the proxy fails closed: only a plain JSON-RPC object, and only tools/call or a method that runs nothing", () => {
  const readOnly = new Set(["norn_answer"])
  for (const method of PROXY_METHODS) expect(proxyAdmits({ jsonrpc: "2.0", id: 1, method }, readOnly, [])).toBe(true)
  for (const body of [
    null, "tools/list", 7, true,
    { jsonrpc: "2.0", id: 1 },
    { jsonrpc: "2.0", id: 1, result: {} },
    { jsonrpc: "2.0", id: 1, method: 5 },
    { jsonrpc: "2.0", id: 1, method: "tools/delete" },
    { jsonrpc: "2.0", id: 1, method: "sampling/createMessage" },
    { jsonrpc: "2.0", id: 1, method: "prompts/get", params: { name: "norn_answer" } },
    { jsonrpc: "2.0", id: 1, method: "TOOLS/CALL", params: { name: "norn_call_action", arguments: {} } },
    { jsonrpc: "2.0", id: 1, method: "tools/call", params: [ "norn_answer" ] },
    { jsonrpc: "2.0", id: 1, method: "tools/call", params: null },
    Object.assign(Object.create({ method: "tools/list" }), { jsonrpc: "2.0", id: 1 }),
    Object.assign(Object.create(null), { jsonrpc: "2.0", id: 1, method: "tools/list" }),
  ]) expect(proxyAdmits(body, readOnly, [])).toBe(false)
})

test("a number passes the proxy only when a double keeps it exactly, so Studio reads the decimal that was checked and shown", () => {
  expect(exactDecimal("1.50e2")).toBe(exactDecimal("150"))
  expect(exactDecimal("-0.0")).toBe("0")
  expect(exactDecimal("1e-400")).not.toBe(exactDecimal("0"))
  for (const text of ['{"a":9007199254740992}', '{"a":0.1,"b":-12.5e-3,"c":1E21,"d":-0}', '{"s":"9007199254740993 and 1e-400"}', '{"s":"\\"9007199254740993"}', '[1,2.0,3e0]']) expect(exactNumbers(text)).toBe(true)
  for (const text of ['{"a":9007199254740993}', '{"a":1e-400}', '{"a":1e400}', '{"a":0.1000000000000000000001}', '{"s":"\\"","a":12345678901234567890}']) expect(exactNumbers(text)).toBe(false)
})

/**
 * A stand-in adapter (newline JSON-RPC on stdio): it answers `session/new` with the mode it is given
 * and echoes the params it saw; a prompt asks for two `norn_call_action` calls and ends on cancel.
 */
const ADAPTER = (mode: string, rawInput: string) => `
const out = (m) => process.stdout.write(JSON.stringify(m) + "\\n")
let prompt = null
let buffer = ""
process.stdin.on("data", (chunk) => {
  buffer += chunk
  let at
  while ((at = buffer.indexOf("\\n")) >= 0) {
    const line = buffer.slice(0, at); buffer = buffer.slice(at + 1)
    if (!line.trim()) continue
    const m = JSON.parse(line)
    if (m.method === "initialize") out({ jsonrpc: "2.0", id: m.id, result: { protocolVersion: 1 } })
    else if (m.method === "session/new") out({ jsonrpc: "2.0", id: m.id, result: { sessionId: "s1", modes: { currentModeId: ${JSON.stringify(mode)} }, _echo: m.params } })
    else if (m.method === "session/prompt") {
      prompt = m.id
      if (m.params.prompt[0].text === "leave manual") out({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "s1", update: { sessionUpdate: "config_option_update", configOptions: [{ id: "mode", name: "Mode", type: "select", category: "mode", currentValue: "auto", options: [] }] } } })
      else for (const id of [900, 901]) out({ jsonrpc: "2.0", id, method: "session/request_permission", params: { sessionId: "s1", toolCall: { toolCallId: "t" + id, title: "Close", rawInput: ${rawInput}, _meta: { claudeCode: { toolName: "mcp__norn__norn_call_action" } } }, options: [{ optionId: "allow", kind: "allow_once", name: "Allow" }, { optionId: "reject", kind: "reject_once", name: "Reject" }] } })
    } else if (m.method === "session/cancel") out({ jsonrpc: "2.0", id: prompt, result: { stopReason: "cancelled" } })
    else if (m.method === undefined) out({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "s1", update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "answer " + m.id + " " + JSON.stringify(m.result) } } } })
  }
})
`

async function harness(mode: string, listing: "paged" | "fails" = "paged", rawInput = `{ action: "x.close", input: { id: "R-1" } }`) {
  const { set, sign } = await keys()
  const scratch = mkdtempSync(join(tmpdir(), "sidekick-relay-"))
  const studio: { method: string; params?: Record<string, unknown> }[] = []
  /** The exact bytes each request to Studio carried. */
  const studioRaw: string[] = []
  const fetcher = (async (_url: string, init: RequestInit) => {
    const body = JSON.parse(String(init.body)) as { id: unknown; method: string; params?: Record<string, unknown> }
    studio.push(body)
    studioRaw.push(String(init.body))
    if (body.method === "tools/list") {
      if (listing === "fails") return new Response("down", { status: 502 })
      return Response.json(body.params?.cursor === undefined
        ? { jsonrpc: "2.0", id: body.id, result: { tools: [{ name: "norn_answer", annotations: { readOnlyHint: true } }, { name: "norn_call_action", annotations: { readOnlyHint: false } }], nextCursor: "page-2" } }
        : { jsonrpc: "2.0", id: body.id, result: { tools: [{ name: "norn_inbox", annotations: { readOnlyHint: true } }] } })
    }
    return Response.json({ jsonrpc: "2.0", id: body.id, result: { content: [{ type: "text", text: "ok" }] } })
  }) as unknown as typeof fetch
  const config: Config = { studioOrigin: STUDIO, allowedOrigins: new Set(["https://studio.norn.test"]), loginHeader: "x-login", allowedLogins: new Set(["me"]), listenHost: "127.0.0.1", listenPort: 0, mcpPort: 0, adapter: [process.execPath, "-e", ADAPTER(mode, rawInput)], scratch, maxConnections: 2 }
  const relay = startRelay(config, { keys: set, fetcher, log: () => undefined })
  const ws = new WebSocket(`ws://127.0.0.1:${relay.server.port}/ws`, { headers: { origin: "https://studio.norn.test", "x-login": "me" } } as unknown as string[])
  const inbox: Record<string, unknown>[] = []
  const waiters: (() => void)[] = []
  ws.onmessage = (event) => { inbox.push(JSON.parse(String(event.data))); for (const wake of waiters.splice(0)) wake() }
  const closed = new Promise<number>((resolve) => { ws.onclose = (event) => resolve(event.code) })
  await new Promise((resolve) => { ws.onopen = resolve })
  const next = async (match: (message: Record<string, unknown>) => boolean) => {
    for (;;) {
      const found = inbox.find(match)
      if (found !== undefined) return found
      await new Promise<void>((resolve) => waiters.push(resolve))
    }
  }
  const proxyRaw = (text: string) => fetch(`http://127.0.0.1:${relay.mcp.port}/mcp/${[...relay.connections.values()][0]!.capability}`, { method: "POST", headers: { "content-type": "application/json" }, body: text }).then((response) => response.json() as Promise<{ result?: unknown; error?: { code: number } }>)
  const proxy = (body: unknown) => proxyRaw(JSON.stringify(body))
  const grant = await sign(GRANT)
  const done = () => { relay.stop(); rmSync(scratch, { recursive: true, force: true }) }
  return { ws, inbox, next, proxy, proxyRaw, studio, studioRaw, grant, closed, done, send: (message: unknown) => ws.send(JSON.stringify(message)) }
}

const callAction = (args: unknown) => ({ jsonrpc: "2.0", id: 7, method: "tools/call", params: { name: "norn_call_action", arguments: args } })

test("relay: a call reaches Studio only with the person's approval for those arguments; Stop drops the unspent ones", async () => {
  const h = await harness("default")
  try {
    // Sent back to back: the relay handles them in order, so the prompt never overtakes the grant check or session/new.
    h.send({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: 1, _meta: { "norn.cloud/grant": h.grant } } })
    h.send({ jsonrpc: "2.0", id: 2, method: "session/new", params: { cwd: "/", mcpServers: [] } })
    h.send({ jsonrpc: "2.0", id: 3, method: "session/prompt", params: { sessionId: "s1", prompt: [{ type: "text", text: "close R-1" }] } })
    const created = await h.next((message) => message.id === 2) as { result: { _echo: { configOptions: unknown; _meta: { claudeCode: { options: Record<string, unknown> } } } } }
    // Both pages of tools/list count: norn_inbox came on the second page.
    expect(created.result._echo._meta.claudeCode.options.allowedTools).toEqual(["mcp__norn__norn_answer", "mcp__norn__norn_inbox"])
    expect(created.result._echo._meta.claudeCode.options.allowDangerouslySkipPermissions).toBe(false)
    expect(created.result._echo.configOptions).toEqual([{ configId: "mode", value: "default" }])
    const first = await h.next((message) => message.method === "session/request_permission") as { id: string }
    const second = await h.next((message) => message.method === "session/request_permission" && message.id !== first.id) as { id: string }
    const calls = () => h.studio.filter((body) => body.method === "tools/call").length
    // Before the person answers, the agent's call is refused at the proxy and never reaches Studio.
    expect((await h.proxy(callAction({ action: "x.close", input: { id: "R-1" } }))).error?.code).toBe(-32001)
    expect(calls()).toBe(0)
    // A read-only tool needs no approval.
    expect((await h.proxy({ jsonrpc: "2.0", id: 8, method: "tools/call", params: { name: "norn_answer", arguments: {} } })).result).toBeDefined()
    expect(calls()).toBe(1)
    h.send({ jsonrpc: "2.0", id: first.id, result: { outcome: { outcome: "selected", optionId: "allow" } } })
    h.send({ jsonrpc: "2.0", id: second.id, result: { outcome: { outcome: "selected", optionId: "allow" } } })
    await h.next((message) => JSON.stringify(message).includes("answer 901"))
    // Other arguments are refused; the approved ones go through once each approval.
    expect((await h.proxy(callAction({ action: "x.delete", input: { id: "R-1" } }))).error?.code).toBe(-32001)
    expect((await h.proxy(callAction({ input: { id: "R-1" }, action: "x.close" }))).result).toBeDefined()
    expect(calls()).toBe(2)
    // Stop: the second approval was never spent and lapses.
    h.send({ jsonrpc: "2.0", method: "session/cancel", params: { sessionId: "s1" } })
    await h.next((message) => message.id === 3)
    expect((await h.proxy(callAction({ action: "x.close", input: { id: "R-1" } }))).error?.code).toBe(-32001)
    expect(calls()).toBe(2)
  } finally { h.done() }
})

test("relay: a session the adapter starts in any mode but manual is refused and the connection closes", async () => {
  const h = await harness("auto", "fails")
  try {
    h.send({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: 1, _meta: { "norn.cloud/grant": h.grant } } })
    h.send({ jsonrpc: "2.0", id: 2, method: "session/new", params: { cwd: "/", mcpServers: [] } })
    const refused = await h.next((message) => message.id === 2) as { error?: { code: number }; result?: unknown }
    expect(refused.error?.code).toBe(-32003)
    expect(refused.result).toBeUndefined()
    expect(await h.closed).toBe(1011)
  } finally { h.done() }
})

test("relay: a failed tools/list pre-allows nothing", async () => {
  const h = await harness("default", "fails")
  try {
    h.send({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: 1, _meta: { "norn.cloud/grant": h.grant } } })
    h.send({ jsonrpc: "2.0", id: 2, method: "session/new", params: { cwd: "/", mcpServers: [] } })
    const created = await h.next((message) => message.id === 2) as { result: { _echo: { _meta: { claudeCode: { options: Record<string, unknown> } } } } }
    expect(created.result._echo._meta.claudeCode.options.allowedTools).toEqual([])
    expect((await h.proxy({ jsonrpc: "2.0", id: 8, method: "tools/call", params: { name: "norn_answer", arguments: {} } })).error?.code).toBe(-32001)
  } finally { h.done() }
})

/** Initialize, open a session and prompt; resolves with the two questions the stand-in agent asks. */
async function prompted(h: Awaited<ReturnType<typeof harness>>, text = "close R-1") {
  h.send({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: 1, _meta: { "norn.cloud/grant": h.grant } } })
  h.send({ jsonrpc: "2.0", id: 2, method: "session/new", params: { cwd: "/", mcpServers: [] } })
  h.send({ jsonrpc: "2.0", id: 3, method: "session/prompt", params: { sessionId: "s1", prompt: [{ type: "text", text }] } })
  await h.next((message) => message.id === 2)
  if (text === "leave manual") return []
  const first = await h.next((message) => message.method === "session/request_permission") as { id: string }
  const second = await h.next((message) => message.method === "session/request_permission" && message.id !== first.id) as { id: string }
  return [first, second]
}

test("relay: Studio gets exactly the call that was approved: a number a double rounds is refused, and the checked object is what is forwarded", async () => {
  const h = await harness("default", "paged", `{ n: 9007199254740992, z: 0 }`)
  try {
    const [first] = await prompted(h)
    h.send({ jsonrpc: "2.0", id: first!.id, result: { outcome: { outcome: "selected", optionId: "allow" } } })
    await h.next((message) => JSON.stringify(message).includes("answer 900"))
    const calls = () => h.studio.filter((body) => body.method === "tools/call").length
    const raw = (args: string) => `{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"norn_call_action","arguments":${args}}}`
    // 2^53 + 1 and 1e-400 parse to the approved 2^53 and 0, but Studio would read them exactly.
    expect((await h.proxyRaw(raw(`{"n":9007199254740993,"z":0}`))).error?.code).toBe(-32602)
    expect((await h.proxyRaw(raw(`{"n":9007199254740992,"z":1e-400}`))).error?.code).toBe(-32602)
    expect(calls()).toBe(0)
    // A duplicate key: whichever one Studio would keep, it gets only the checked object.
    expect((await h.proxyRaw(raw(`{"z":1,"z":0,"n":9007199254740992.0}`))).result).toBeDefined()
    expect(calls()).toBe(1)
    expect(h.studioRaw.at(-1)).toBe(raw(`{"z":0,"n":9007199254740992}`))
    // The approval was spent.
    expect((await h.proxyRaw(raw(`{"n":9007199254740992,"z":0}`))).error?.code).toBe(-32001)
  } finally { h.done() }
})

test("relay: a second session/new ends the turn before it: open questions are cancelled and unspent approvals lapse", async () => {
  const h = await harness("default")
  try {
    const [first] = await prompted(h)
    h.send({ jsonrpc: "2.0", id: first!.id, result: { outcome: { outcome: "selected", optionId: "allow" } } })
    await h.next((message) => JSON.stringify(message).includes("answer 900"))
    h.send({ jsonrpc: "2.0", id: 4, method: "session/new", params: { cwd: "/", mcpServers: [] } })
    await h.next((message) => message.id === 4)
    expect(JSON.stringify(await h.next((message) => JSON.stringify(message).includes("answer 901")))).toContain("cancelled")
    expect((await h.proxy(callAction({ action: "x.close", input: { id: "R-1" } }))).error?.code).toBe(-32001)
    expect(h.studio.filter((body) => body.method === "tools/call")).toHaveLength(0)
  } finally { h.done() }
})

test("relay: a session/new sent as a notification never opens a session", async () => {
  const h = await harness("default")
  try {
    h.send({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: 1, _meta: { "norn.cloud/grant": h.grant } } })
    h.send({ jsonrpc: "2.0", method: "session/new", params: { cwd: "/", mcpServers: [] } })
    h.send({ jsonrpc: "2.0", id: 2, method: "session/new", params: { cwd: "/", mcpServers: [] } })
    await h.next((message) => message.id === 2)
    // Only the request was checked with Studio and reached the agent.
    expect(h.studio.filter((body) => body.method === "initialize")).toHaveLength(1)
  } finally { h.done() }
})

test("relay: a config option update that moves the session out of the manual mode closes the connection", async () => {
  const h = await harness("default")
  try {
    await prompted(h, "leave manual")
    expect(await h.closed).toBe(1011)
  } finally { h.done() }
})
