import { expect, test } from "bun:test"
import { defaultFetch, setDisplayedScope } from "../src/lib/definitions.ts"
import { fetchGrant, sidekickServed } from "../src/sidekick/client.ts"
import { applyUpdate, promptItem, toolLabel, toolLabels, viewItem, type Transcript } from "../src/sidekick/transcript.ts"

const update = (value: unknown) => ({ sessionId: "s", update: value }) as Parameters<typeof applyUpdate>[1]

test("Sidekick is on only where a bridge answers its config as JSON; the public SPA fallback page is not one", async () => {
  const answer = (body: string, type: string, status = 200) => async () => new Response(body, { status, headers: { "content-type": type } })
  expect(await sidekickServed(answer(JSON.stringify({ sidekick: 1, socket: "/sidekick/ws" }), "application/json") as unknown as typeof fetch)).toBe(true)
  expect(await sidekickServed(answer("<!doctype html><div id=root>", "text/html") as unknown as typeof fetch)).toBe(false)
  expect(await sidekickServed(answer("{}", "application/json", 404) as unknown as typeof fetch)).toBe(false)
  expect(await sidekickServed(answer(JSON.stringify({ sidekick: 1, socket: "wss://elsewhere/ws" }), "application/json") as unknown as typeof fetch)).toBe(false)
  expect(await sidekickServed((async () => { throw new TypeError("offline") }) as unknown as typeof fetch)).toBe(false)
})

test("a turn folds into the person's prompt, the agent's words, each Norn tool compactly, and its view", () => {
  let transcript: Transcript = promptItem([], "u0", "what's our stock worth")
  transcript = applyUpdate(transcript, update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "Asking " } }))
  transcript = applyUpdate(transcript, update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "Norn." } }))
  transcript = applyUpdate(transcript, update({ sessionUpdate: "tool_call", toolCallId: "t1", title: "mcp__norn__norn_answer", status: "pending", _meta: { claudeCode: { toolName: "mcp__norn__norn_answer" } } }))
  transcript = applyUpdate(transcript, update({ sessionUpdate: "tool_call", toolCallId: "t1", title: "again" }))
  transcript = viewItem(transcript, { tool: "norn_answer", view: "answer", input: {}, result: { content: [{ type: "text", text: "{}" }] } })
  transcript = applyUpdate(transcript, update({ sessionUpdate: "tool_call_update", toolCallId: "t1", status: "completed" }))
  transcript = applyUpdate(transcript, update({ sessionUpdate: "usage_update", used: 1, size: 2 }))
  expect(transcript.map((item) => item.kind)).toEqual(["user", "text", "tool", "view"])
  expect(transcript[1]).toMatchObject({ text: "Asking Norn." })
  expect(transcript[2]).toMatchObject({ tool: "norn_answer", status: "completed" })
})

test("a tool row says the server's label for the tool, or the tool's own name, never Studio's words", () => {
  const labels = toolLabels({
    tools: [
      { name: "norn_trace_answer", title: "Trace an answer to its source" },
      { name: "act_docs_scanned_document_correct", title: "Correct a scanned document" },
      { name: "norn_untitled", title: " " },
      { name: 7, title: "not a name" },
      "malformed",
      null,
    ],
  })
  expect([...labels.keys()]).toEqual(["norn_trace_answer", "act_docs_scanned_document_correct"])
  expect(toolLabel(labels, "norn_trace_answer")).toBe("Trace an answer to its source")
  expect(toolLabel(labels, "act_docs_scanned_document_correct")).toBe("Correct a scanned document")
  // No label from the server: the name as it is, not prettified.
  expect(toolLabel(labels, "norn_untitled")).toBe("norn_untitled")
  expect(toolLabel(toolLabels(null), "norn_new_thing")).toBe("norn_new_thing")
  expect(toolLabels({ tools: "nope" }).size).toBe(0)
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
