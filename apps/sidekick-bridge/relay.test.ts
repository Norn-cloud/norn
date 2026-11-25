import { expect, test } from "bun:test"
import { SignJWT, createLocalJWKSet, exportJWK, generateKeyPair } from "jose"
import { BUILT_IN_TOOLS, ENV_CLAIM, MCP_SERVER_NAME, MEMBER_CLAIM, ORG_CLAIM, TOOL_PAGES, TOOL_PAGE_LIMIT, configFromEnv, forwardedUpdate, listAllTools, type ListedTool, SIDEKICK_CLIENT, permissionAnswer, rewriteInitialize, rewriteNewSession, rewritePrompt, sameGrantIdentity, scrub, toolLabels, verifyGrant, viewOf } from "./relay.ts"

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
  const session = rewriteNewSession("/scratch/c1", "http://127.0.0.1:17951/mcp/cap", "claude-sonnet-5-5") as { cwd: string; mcpServers: unknown[]; _meta: { claudeCode: { options: Record<string, unknown> } } }
  expect(session.cwd).toBe("/scratch/c1")
  expect(session.mcpServers).toEqual([{ type: "http", name: MCP_SERVER_NAME, url: "http://127.0.0.1:17951/mcp/cap", headers: [] }])
  const options = session._meta.claudeCode.options
  expect(options).toMatchObject({ tools: [], settingSources: [], strictMcpConfig: true, skills: [], plugins: [], persistSession: false, allowedTools: ["mcp__norn"], model: "claude-sonnet-5-5" })
  for (const tool of ["Bash", "Edit", "Write", "Read", "WebFetch", "WebSearch", "Task", "Agent", "Glob", "Grep", "NotebookEdit", "Skill"]) expect(options.disallowedTools as string[]).toContain(tool)
  expect(BUILT_IN_TOOLS.length).toBeGreaterThan(20)
})

test("only text reaches the agent", () => {
  expect(rewritePrompt({ sessionId: "s", prompt: [{ type: "text", text: "what's our stock worth" }, { type: "resource_link", uri: "file:///etc/passwd", name: "x" }, { type: "image", data: "", mimeType: "image/png" }] })).toEqual({ sessionId: "s", prompt: [{ type: "text", text: "what's our stock worth" }] })
  expect(rewritePrompt({ sessionId: "s", prompt: [{ type: "resource_link", uri: "file:///etc/passwd", name: "x" }] })).toBeNull()
  expect(rewritePrompt({ prompt: [] })).toBeNull()
})

test("a Norn tool is allowed once; any other tool's permission request is rejected", () => {
  const options = [{ optionId: "a", kind: "allow_always", name: "Always" }, { optionId: "o", kind: "allow_once", name: "Once" }, { optionId: "r", kind: "reject_once", name: "No" }]
  expect(permissionAnswer({ toolCall: { toolCallId: "t", _meta: { claudeCode: { toolName: "mcp__norn__norn_answer" } } }, options })).toEqual({ outcome: { outcome: "selected", optionId: "o" } })
  expect(permissionAnswer({ toolCall: { toolCallId: "t", _meta: { claudeCode: { toolName: "Bash" } } }, options })).toEqual({ outcome: { outcome: "selected", optionId: "r" } })
  expect(permissionAnswer({ toolCall: { toolCallId: "t", title: "mcp__other__norn_answer" }, options })).toEqual({ outcome: { outcome: "selected", optionId: "r" } })
  expect(permissionAnswer({ toolCall: { toolCallId: "t" }, options: [{ optionId: "a", kind: "allow_once" }] })).toEqual({ outcome: { outcome: "cancelled" } })
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

test("a tool's label is the title its listing gives; an untitled tool has none", () => {
  expect(toolLabels([
    { name: "norn_answer", title: "Answer a question" },
    { name: "act_x", title: "Correct a scanned document" },
    { name: "norn_untitled" },
    { name: "norn_blank", title: "  " },
    { name: "norn_odd", title: 7 },
  ])).toEqual([{ name: "norn_answer", title: "Answer a question" }, { name: "act_x", title: "Correct a scanned document" }])
})

/** A `tools/list` server as the cell pages it: sorted by name, `limit` 1..100 (32 when absent), `nextCursor` until the end. */
function catalogServer(tools: ListedTool[]) {
  const asked: { limit?: number; cursor?: string }[] = []
  const page = async (params: { limit?: number; cursor?: string }) => {
    asked.push(params)
    const limit = params.limit ?? 32
    if (limit < 1 || limit > 100) return null
    const offset = params.cursor === undefined ? 0 : Number(params.cursor.split(":")[1])
    const end = Math.min(offset + limit, tools.length)
    return { tools: tools.slice(offset, end), ...(end < tools.length ? { nextCursor: `digest:${end}` } : {}) }
  }
  return { asked, page }
}

test("the relay reads every page of tools/list, so a tool past the first page keeps its label and view", async () => {
  // A workspace with many actions: act_* sorts before norn_*, so the Norn tools fall on later pages.
  const actions: ListedTool[] = Array.from({ length: 230 }, (_, index) => ({ name: `act_${String(index).padStart(3, "0")}`, title: `Action ${index}` }))
  const norn: ListedTool[] = [
    { name: "norn_answer", title: "Answer a question", _meta: { ui: { resourceUri: "ui://norn/answer.html" } } },
    { name: "norn_trace_answer", title: "Trace an answer to its source", _meta: { ui: { resourceUri: "ui://norn/lineage.html" } } },
    { name: "norn_trace_object", title: "Trace a record to its source", _meta: { ui: { resourceUri: "ui://norn/lineage.html" } } },
  ]
  const server = catalogServer([...actions, ...norn])
  const tools = await listAllTools(server.page)
  expect(server.asked).toEqual([{ limit: TOOL_PAGE_LIMIT }, { limit: TOOL_PAGE_LIMIT, cursor: "digest:100" }, { limit: TOOL_PAGE_LIMIT, cursor: "digest:200" }])
  expect(tools?.length).toBe(233)
  const labels = toolLabels(tools ?? [])
  for (const { name, title } of norn) expect(labels).toContainEqual({ name, title: title as string })
  const views = new Map((tools ?? []).flatMap((tool) => { const view = viewOf(tool); return view === null ? [] : [[tool.name, view] as const] }))
  expect(Object.fromEntries(views)).toEqual({ norn_answer: "answer", norn_trace_answer: "lineage", norn_trace_object: "lineage" })
})

test("a refused tools/list page ends the listing; an endless cursor stops at the page cap", async () => {
  expect(await listAllTools(async () => null)).toBeNull()
  let served = 0
  const second = await listAllTools(async () => served++ === 0 ? { tools: [{ name: "norn_answer", title: "Answer a question" }], nextCursor: "d:1" } : null)
  expect(second).toEqual([{ name: "norn_answer", title: "Answer a question" }])
  let pages = 0
  const endless = await listAllTools(async () => { pages++; return { tools: [{ name: `norn_${pages}` }], nextCursor: "d:1" } })
  expect(pages).toBe(TOOL_PAGES)
  expect(endless?.length).toBe(TOOL_PAGES)
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
