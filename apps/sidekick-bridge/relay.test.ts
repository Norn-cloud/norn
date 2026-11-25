import { expect, test } from "bun:test"
import { SignJWT, createLocalJWKSet, exportJWK, generateKeyPair } from "jose"
import { BUILT_IN_TOOLS, ENV_CLAIM, MCP_SERVER_NAME, MEMBER_CLAIM, ORG_CLAIM, configFromEnv, forwardedUpdate, SIDEKICK_CLIENT, permissionDecision, personAnswer, questionFor, readOnlyTools, rewriteInitialize, rewriteNewSession, rewritePrompt, sameGrantIdentity, scrub, verifyGrant, viewOf } from "./relay.ts"

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
