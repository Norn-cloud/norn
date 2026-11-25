import { expect, test } from "bun:test"
import { defaultFetch, setDisplayedScope } from "../src/lib/definitions.ts"
import { fetchGrant, sidekickServed, type SidekickEvents, type SidekickSession } from "../src/sidekick/client.ts"
import { toolWords, Turn, type SidekickChunk, type SidekickMessage } from "../src/sidekick/transcript.ts"
import { SidekickTransport } from "../src/sidekick/transport.ts"

const update = (value: unknown) => ({ sessionId: "s", update: value }) as Parameters<Turn["update"]>[0]
const user = (text: string): SidekickMessage => ({ id: "u1", role: "user", parts: [{ type: "text", text }] })
const send = (messages: SidekickMessage[], abortSignal?: AbortSignal) => ({ trigger: "submit-message" as const, chatId: "c", messageId: undefined, messages, abortSignal })
const PERMISSION = {
  sessionId: "s",
  toolCall: { toolCallId: "t2", title: "mcp__norn__norn_call_action", rawInput: { action: "demo.record.close", input: { id: "R-1" } }, _meta: { claudeCode: { toolName: "mcp__norn__norn_call_action" } } },
  options: [{ optionId: "allow", name: "Allow", kind: "allow_once" }, { optionId: "reject", name: "Reject", kind: "reject_once" }],
}

async function drain(stream: ReadableStream<SidekickChunk>): Promise<SidekickChunk[]> {
  const chunks: SidekickChunk[] = []
  const reader = stream.getReader()
  for (;;) {
    const next = await reader.read()
    if (next.done) return chunks
    chunks.push(next.value)
  }
}

/** A stand-in session: the test plays the bridge through the events the transport hands it. */
function fakeSession(script: (events: SidekickEvents) => Promise<string>) {
  const state = { cancelled: 0, prompts: [] as string[] }
  const open = async (events: SidekickEvents) => ({
    closed: false,
    prompt: (text: string) => { state.prompts.push(text); return script(events) },
    cancel: () => { state.cancelled += 1 },
    close: () => undefined,
    callTool: async () => ({ content: [] }),
  }) as unknown as SidekickSession
  return { open, state }
}

test("Sidekick is on only where a bridge answers its config as JSON; the public SPA fallback page is not one", async () => {
  const answer = (body: string, type: string, status = 200) => async () => new Response(body, { status, headers: { "content-type": type } })
  expect(await sidekickServed(answer(JSON.stringify({ sidekick: 1, socket: "/sidekick/ws" }), "application/json") as unknown as typeof fetch)).toBe(true)
  expect(await sidekickServed(answer("<!doctype html><div id=root>", "text/html") as unknown as typeof fetch)).toBe(false)
  expect(await sidekickServed(answer("{}", "application/json", 404) as unknown as typeof fetch)).toBe(false)
  expect(await sidekickServed(answer(JSON.stringify({ sidekick: 1, socket: "wss://elsewhere/ws" }), "application/json") as unknown as typeof fetch)).toBe(false)
  expect(await sidekickServed((async () => { throw new TypeError("offline") }) as unknown as typeof fetch)).toBe(false)
})

test("a turn becomes the chat's chunks: the agent's words, each Norn tool compactly, and its view, in order", () => {
  const chunks: SidekickChunk[] = []
  const turn = new Turn((chunk) => chunks.push(chunk))
  turn.start()
  turn.update(update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "Asking " } }))
  turn.update(update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "Norn." } }))
  turn.update(update({ sessionUpdate: "tool_call", toolCallId: "t1", title: "mcp__norn__norn_answer", status: "pending", _meta: { claudeCode: { toolName: "mcp__norn__norn_answer" } } }))
  turn.update(update({ sessionUpdate: "tool_call", toolCallId: "t1", title: "again" }))
  turn.view({ tool: "norn_answer", view: "answer", input: {}, result: { content: [{ type: "text", text: "{}" }] } })
  turn.update(update({ sessionUpdate: "tool_call_update", toolCallId: "t1", status: "completed" }))
  turn.update(update({ sessionUpdate: "tool_call_update", toolCallId: "unknown", status: "completed" }))
  turn.update(update({ sessionUpdate: "usage_update", used: 1, size: 2 }))
  turn.finish()
  turn.update(update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "late" } }))
  expect(chunks.map((chunk) => chunk.type)).toEqual(["start", "text-start", "text-delta", "text-delta", "text-end", "tool-input-available", "data-view", "tool-output-available", "finish"])
  expect(chunks[5]).toMatchObject({ toolCallId: "t1", toolName: "norn_answer", title: "Answered the question", dynamic: true })
  expect(toolWords("norn_trace_answer")).toBe("Traced where the number comes from")
  expect(toolWords("norn_new_thing")).toBe("new thing")
})

test("send: one prompt turn with the person's text; a failure becomes the chat's error, in words", async () => {
  const ok = fakeSession(async (events) => {
    events.update(update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "Hi." } }))
    return "end_turn"
  })
  const transport = new SidekickTransport({ open: ok.open })
  const chunks = await drain(await transport.sendMessages(send([user(" What's due? ")])))
  expect(ok.state.prompts).toEqual(["What's due?"])
  expect(chunks.map((chunk) => chunk.type)).toEqual(["start", "text-start", "text-delta", "text-end", "finish"])
  const refused = new SidekickTransport({ open: async () => { throw new Error("Studio did not grant Sidekick access.") } })
  const failed = await drain(await refused.sendMessages(send([user("hi")])))
  expect(failed.at(-1)).toEqual({ type: "error", errorText: "Studio did not grant Sidekick access." })
})

test("approval: the bridge's question waits for the person; Allow and Decline answer only with the options it offered", async () => {
  for (const approved of [true, false]) {
    let answer: unknown
    const bridge = fakeSession(async (events) => {
      events.update(update({ sessionUpdate: "tool_call", toolCallId: "t2", title: "mcp__norn__norn_call_action", status: "pending", rawInput: PERMISSION.toolCall.rawInput, _meta: PERMISSION.toolCall._meta }))
      answer = await events.permission(PERMISSION as never)
      if (approved) events.update(update({ sessionUpdate: "tool_call_update", toolCallId: "t2", status: "completed" }))
      return "end_turn"
    })
    const transport = new SidekickTransport({ open: bridge.open })
    const reader = (await transport.sendMessages(send([user("close R-1")]))).getReader()
    const seen: SidekickChunk[] = []
    while (!seen.some((chunk) => chunk.type === "tool-approval-request")) seen.push((await reader.read()).value!)
    const request = seen.at(-1) as Extract<SidekickChunk, { type: "tool-approval-request" }>
    expect(request.toolCallId).toBe("t2")
    expect(transport.respond("approval-unknown", true)).toBe(false)
    expect(transport.respond(request.approvalId, approved)).toBe(true)
    expect(transport.respond(request.approvalId, approved)).toBe(false)
    for (;;) {
      const next = await reader.read()
      if (next.done) break
      seen.push(next.value)
    }
    expect(answer).toEqual({ outcome: { outcome: "selected", optionId: approved ? "allow" : "reject" } })
    expect(seen).toContainEqual({ type: "tool-approval-response", approvalId: request.approvalId, approved })
    expect(seen.map((chunk) => chunk.type)).toContain(approved ? "tool-output-available" : "tool-output-denied")
  }
})

test("approval: a question without both offered answers is cancelled, never allowed", async () => {
  let answer: unknown
  const bridge = fakeSession(async (events) => {
    answer = await events.permission({ ...PERMISSION, options: [{ optionId: "always", name: "Always", kind: "allow_always" }] } as never)
    return "end_turn"
  })
  const transport = new SidekickTransport({ open: bridge.open })
  await drain(await transport.sendMessages(send([user("go")])))
  expect(answer).toEqual({ outcome: { outcome: "cancelled" } })
})

test("stop: aborting the turn cancels it at the bridge and cancels an open question", async () => {
  let answer: unknown
  let release: (() => void) | undefined
  const bridge = fakeSession(async (events) => {
    events.update(update({ sessionUpdate: "tool_call", toolCallId: "t2", title: "mcp__norn__norn_call_action", status: "pending" }))
    answer = await events.permission(PERMISSION as never)
    await new Promise<void>((resolve) => { release = resolve })
    return "cancelled"
  })
  const transport = new SidekickTransport({ open: bridge.open })
  const abort = new AbortController()
  const reader = (await transport.sendMessages(send([user("go")], abort.signal))).getReader()
  for (;;) {
    const next = await reader.read()
    if (next.done || next.value.type === "tool-approval-request") break
  }
  abort.abort()
  await Bun.sleep(0)
  expect(bridge.state.cancelled).toBe(1)
  expect(answer).toEqual({ outcome: { outcome: "cancelled" } })
  release?.()
})

test("fetchGrant sends pinned scope headers via defaultFetch even after displayed scope changes", async () => {
  const original = globalThis.fetch
  const seen: Headers[] = []
  globalThis.fetch = (async (_input: string | URL | Request, init?: RequestInit) => {
    seen.push(new Headers(init?.headers))
    return Response.json({ token: "tok", expiresAt: Date.now() + 60_000 })
  }) as typeof fetch
  try {
    const pinned = { orgId: "org_a", environmentId: "env_a" }
    setDisplayedScope({ orgId: "org_b", environmentId: "env_b" })
    await fetchGrant(defaultFetch, pinned)
    expect(seen[0]?.get("x-norn-expected-org")).toBe("org_a")
    expect(seen[0]?.get("x-norn-expected-env")).toBe("env_a")
  } finally {
    setDisplayedScope(undefined)
    globalThis.fetch = original
  }
})

test("close ends the session; a later send opens a new one (a remount or a new chat never strands the chat)", async () => {
  let opened = 0
  const bridge = fakeSession(async () => "end_turn")
  const transport = new SidekickTransport({ open: async (events) => { opened += 1; return bridge.open(events) } })
  await drain(await transport.sendMessages(send([user("one")])))
  transport.close()
  const chunks = await drain(await transport.sendMessages(send([user("two")])))
  expect(opened).toBe(2)
  expect(chunks.at(-1)).toEqual({ type: "finish" })
})

test("approval: the agent announces the call before its input; the input the person is asked about is drawn before the question", () => {
  const chunks: SidekickChunk[] = []
  const turn = new Turn((chunk) => chunks.push(chunk))
  turn.start()
  // The adapter's order: tool_call with no input yet, then the full input in an update and in the question.
  turn.update(update({ sessionUpdate: "tool_call", toolCallId: "t2", title: "mcp__norn__norn_call_action", status: "pending", _meta: PERMISSION.toolCall._meta }))
  turn.update(update({ sessionUpdate: "tool_call_update", toolCallId: "t2", rawInput: { action: "demo.record.close" } }))
  turn.update(update({ sessionUpdate: "tool_call_update", toolCallId: "t2", rawInput: { action: "demo.record.close" } }))
  turn.approval("approval-1", PERMISSION.toolCall)
  // After the question, an update never redraws the part over it.
  turn.update(update({ sessionUpdate: "tool_call_update", toolCallId: "t2", rawInput: { action: "other" } }))
  const inputs = chunks.filter((chunk): chunk is Extract<SidekickChunk, { type: "tool-input-available" }> => chunk.type === "tool-input-available")
  expect(inputs.map((chunk) => chunk.input)).toEqual([{}, { action: "demo.record.close" }, PERMISSION.toolCall.rawInput])
  // The name stays the one the call opened with, even when an update does not repeat it.
  expect(inputs.every((chunk) => chunk.toolName === "norn_call_action")).toBe(true)
  expect(chunks.map((chunk) => chunk.type).slice(-2)).toEqual(["tool-input-available", "tool-approval-request"])
})

/** A stand-in session whose prompts are played one by one by the test. */
function scriptedSession() {
  const state = { cancelled: 0, prompts: [] as string[], opened: 0 }
  const turns: { readonly text: string; readonly events: SidekickEvents; readonly settle: (reason: string) => void }[] = []
  const waiters: (() => void)[] = []
  const open = async (events: SidekickEvents) => {
    state.opened += 1
    return {
      closed: false,
      prompt: (text: string) => new Promise<string>((settle) => { state.prompts.push(text); turns.push({ text, events, settle }); for (const wake of waiters.splice(0)) wake() }),
      cancel: () => { state.cancelled += 1 },
      close: () => events.closed("closed"),
      callTool: async () => ({ content: [] }),
    } as unknown as SidekickSession
  }
  const prompted = async (count: number) => { while (turns.length < count) await new Promise<void>((resolve) => waiters.push(resolve)) }
  return { open, state, turns, prompted }
}

async function readUntil(reader: ReadableStreamDefaultReader<SidekickChunk>, seen: SidekickChunk[], type: SidekickChunk["type"]) {
  while (!seen.some((chunk) => chunk.type === type)) {
    const next = await reader.read()
    if (next.done) return
    seen.push(next.value)
  }
}

test("stop: the stopped turn is detached at once; a fast follow-up waits for it to settle and never gets its traffic or its cancels", async () => {
  const bridge = scriptedSession()
  const transport = new SidekickTransport({ open: bridge.open })
  const first = new AbortController()
  const one = (await transport.sendMessages(send([user("close R-1")], first.signal))).getReader()
  const oneSeen: SidekickChunk[] = []
  await bridge.prompted(1)
  const turn1 = bridge.turns[0]!
  const asked1 = turn1.events.permission(PERMISSION as never)
  await readUntil(one, oneSeen, "tool-approval-request")
  first.abort()
  expect(await asked1).toEqual({ outcome: { outcome: "cancelled" } })
  expect(bridge.state.cancelled).toBe(1)
  // A question the bridge asks after Stop is answered `cancelled`, never left open.
  expect(await turn1.events.permission(PERMISSION as never)).toEqual({ outcome: { outcome: "cancelled" } })

  // The follow-up is sent before the stopped prompt settles: it waits, so it is not prompted yet.
  const two = (await transport.sendMessages(send([user("and now?")]))).getReader()
  const twoSeen: SidekickChunk[] = []
  await Bun.sleep(5)
  expect(bridge.state.prompts).toEqual(["close R-1"])
  // The stopped turn's late traffic reaches no turn.
  turn1.events.update(update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "late from turn one" } }))
  turn1.settle("cancelled")
  await bridge.prompted(2)
  const turn2 = bridge.turns[1]!
  const asked2 = turn2.events.permission({ ...PERMISSION, toolCall: { ...PERMISSION.toolCall, toolCallId: "t3" } } as never)
  await readUntil(two, twoSeen, "tool-approval-request")
  // Turn one's ending did not cancel turn two's question: the person can still answer it.
  const request = twoSeen.find((chunk) => chunk.type === "tool-approval-request") as Extract<SidekickChunk, { type: "tool-approval-request" }>
  expect(transport.respond(request.approvalId, true)).toBe(true)
  expect(await asked2).toEqual({ outcome: { outcome: "selected", optionId: "allow" } })
  turn2.settle("end_turn")
  await readUntil(two, twoSeen, "finish")
  expect(JSON.stringify(twoSeen)).not.toContain("late from turn one")
  expect(bridge.state.opened).toBe(1)
})

test("stop: a stopped prompt that never settles does not hold the chat: the follow-up starts on a fresh session", async () => {
  const bridge = scriptedSession()
  const transport = new SidekickTransport({ open: bridge.open, settleMs: 20 })
  const first = new AbortController()
  await transport.sendMessages(send([user("one")], first.signal))
  await bridge.prompted(1)
  first.abort()
  const two = (await transport.sendMessages(send([user("two")]))).getReader()
  await bridge.prompted(2)
  expect(bridge.state.opened).toBe(2)
  expect(bridge.turns[1]!.events).not.toBe(bridge.turns[0]!.events)
  bridge.turns[1]!.settle("end_turn")
  const seen: SidekickChunk[] = []
  await readUntil(two, seen, "finish")
  expect(seen.at(-1)).toEqual({ type: "finish" })
})
