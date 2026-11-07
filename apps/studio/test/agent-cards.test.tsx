import { expect, jest, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import { DecisionCard, VerdictCard, reasons } from "../src/agent/cards.tsx"
import { sessionId, verdictOf } from "../src/agent/verdict.ts"
import { Chat } from "@ai-sdk/react"
import type { UIMessage } from "ai"
import { AGENT_UNSERVED, AgentPanel, agentServedBy, PROBE_DEADLINE_MS, canSend, probeAgentRoute, sendsOnEnter, type AgentState } from "../src/agent/panel.tsx"

const proposal = { artifact: "stress.purchase_positive.law", pointer: "/payload/response/expression", value: "self.unseen > 0" }
const refusal = {
  proposal, admitted: false, stage: "model", receiptSha256: null,
  refusals: [{ code: "law.model.predicate_type", message: "`unseen` is not a member of stress.Purchase", artifactId: proposal.artifact,
    law: { artifactId: proposal.artifact, name: "Order quantity must be positive", rationale: "A purchase promises a positive quantity.", expression: "self.ordered > 0" } }],
}

test("a refusal shows the law in its own words and the checker's reason", () => {
  const verdict = verdictOf(refusal)
  if (verdict === undefined) throw new Error("a verdict")
  const html = renderToStaticMarkup(<VerdictCard verdict={verdict} />)
  expect(html).toContain("Refused by the checker")
  expect(html).toContain("Order quantity must be positive")
  expect(html).toContain("A purchase promises a positive quantity.")
  expect(html).toContain("law.model.predicate_type")
})

test("only the Worker's verdict shapes are read as verdicts", () => {
  expect(verdictOf({ proposal, error: "No artifact x." })).toBeDefined()
  expect(verdictOf({ admitted: true })).toBeUndefined()
  expect(verdictOf("refused")).toBeUndefined()
})

test("after a refusal the user can stop or ask for an alternative", () => {
  const html = renderToStaticMarkup(<DecisionCard disabled={false} onStop={() => {}} onAlternative={() => {}} />)
  expect(html).toContain("Stop")
  expect(html).toContain("Try an alternative")
})

test("a tab keeps one session id for the server's budget", () => {
  const store = new Map<string, string>()
  const storage = { getItem: (key: string) => store.get(key) ?? null, setItem: (key: string, value: string) => void store.set(key, value) }
  const first = sessionId(storage)
  expect(first).toMatch(/^[0-9a-f-]{36}$/)
  expect(sessionId(storage)).toBe(first)
})

test("a nested checker finding reads as its own message, with the raw text kept under Details", () => {
  const raw = '[Diagnostic { code: "expr.member.unknown", message: "`unseen` is not a member of `stress.Purchase`", span: (0, 11) }]'
  expect(reasons(raw)).toEqual(["`unseen` is not a member of `stress.Purchase`"])
  expect(reasons("plain reason")).toEqual(["plain reason"])
  const verdict = { ...refusal, refusals: [{ ...refusal.refusals[0], message: raw }] }
  const html = renderToStaticMarkup(<VerdictCard verdict={verdict as never} />)
  expect(html).toContain("Details")
  expect(html).toContain("is not a member of")
})

test("a proposal or refusal without the fields the card reads is not a verdict", () => {
  expect(verdictOf({ ...refusal, proposal: null })).toBeUndefined()
  expect(verdictOf({ proposal: "x", error: "No artifact x." })).toBeUndefined()
  expect(verdictOf({ ...refusal, refusals: [null] })).toBeUndefined()
  expect(verdictOf({ ...refusal, refusals: [{ code: "x" }] })).toBeUndefined()
})

test("a tab keeps one session id when its storage throws", () => {
  const broken = { getItem: (): string | null => { throw new Error("SecurityError") }, setItem: () => { throw new Error("QuotaExceededError") } }
  const first = sessionId(broken)
  expect(first).toMatch(/^[0-9a-f-]{36}$/)
  expect(sessionId(broken)).toBe(first)
  expect(sessionId(undefined)).toBe(first)
})

test("the panel renders the conversation and draft it is given, so reopening the sheet keeps them", () => {
  const chat = new Chat<UIMessage>({ messages: [{ id: "m1", role: "user", parts: [{ type: "text", text: "Let Receive start from draft" }] }] })
  const state: AgentState = { chat, input: "half-typed draft", setInput: () => {}, stopped: new Set(), setStopped: () => {}, agentServed: true }
  const html = renderToStaticMarkup(<AgentPanel state={state} />)
  expect(html).toContain("Let Receive start from draft")
  expect(html).toContain("half-typed draft")
  expect(html).not.toContain("Propose a change")
})

test("a verdict is read only when every field the cards render has the Worker's shape", () => {
  const admitted = { proposal, admitted: true, stage: "admitted", receiptSha256: "eef45040c171aa", refusals: [] }
  expect(verdictOf(admitted)).toBeDefined()
  expect(verdictOf({ ...admitted, receiptSha256: null })).toBeDefined()
  for (const broken of [
    { proposal: {}, admitted: true, stage: "admitted", refusals: [], receiptSha256: "r" },
    { ...admitted, receiptSha256: 42 },
    { ...admitted, stage: { name: "admitted" } },
    { ...admitted, stage: "unheard-of" },
    { ...refusal, refusals: [{ ...refusal.refusals[0], code: 7 }] },
    { ...refusal, refusals: [{ ...refusal.refusals[0], law: { ...refusal.refusals[0]?.law, name: { text: "x" } } }] },
    { ...refusal, refusals: [{ ...refusal.refusals[0], law: "law" }] },
    { proposal, error: 3 },
  ]) expect(verdictOf(broken)).toBeUndefined()
  const html = renderToStaticMarkup(<VerdictCard verdict={verdictOf(refusal) ?? (() => { throw new Error("a verdict") })()} />)
  expect(html).toContain("Refused by the checker")
})

test("Enter sends, but not with Shift and not while an IME composition is being confirmed", () => {
  expect(sendsOnEnter({ key: "Enter", shiftKey: false, nativeEvent: { isComposing: false } })).toBe(true)
  expect(sendsOnEnter({ key: "Enter", shiftKey: true, nativeEvent: { isComposing: false } })).toBe(false)
  expect(sendsOnEnter({ key: "Enter", shiftKey: false, nativeEvent: { isComposing: true } })).toBe(false)
  expect(sendsOnEnter({ key: "a", shiftKey: false, nativeEvent: {} })).toBe(false)
})

test("the page keeps one session id even when storage recovers holding another, and writes its id back", () => {
  const broken = { getItem: (): string | null => { throw new Error("SecurityError") }, setItem: () => { throw new Error("SecurityError") } }
  const first = sessionId(broken)
  const store = new Map([["norn-agent-session", "00000000-0000-4000-8000-000000000000"]])
  const recovered = { getItem: (key: string) => store.get(key) ?? null, setItem: (key: string, value: string) => void store.set(key, value) }
  expect(sessionId(recovered)).toBe(first)
  expect(store.get("norn-agent-session")).toBe(first)
  expect(sessionId(undefined)).toBe(first)
})

test("where the agent route is not served, the assistant says so and its composer is disabled", () => {
  expect(agentServedBy(404)).toBe(false)
  expect(agentServedBy(503)).toBe(false)
  expect(agentServedBy("unreachable")).toBe(false)
  expect(agentServedBy(405)).toBe(true)
  const chat = new Chat<UIMessage>({})
  const base: AgentState = { chat, input: "", setInput: () => {}, stopped: new Set(), setStopped: () => {}, agentServed: false }
  const off = renderToStaticMarkup(<AgentPanel state={base} />)
  expect(off).toContain(AGENT_UNSERVED)
  expect(off).toMatch(/<fieldset disabled=""/)
  const on = renderToStaticMarkup(<AgentPanel state={{ ...base, agentServed: true }} />)
  expect(on).not.toContain(AGENT_UNSERVED)
  expect(on).not.toMatch(/<fieldset disabled=""/)
})

test("sending stays closed while the agent probe is pending, and after it fails or is refused", async () => {
  expect(canSend(undefined, "hello", false)).toBe(false)
  expect(canSend(false, "hello", false)).toBe(false)
  expect(canSend(true, "hello", false)).toBe(true)
  expect(canSend(true, " ", false)).toBe(false)
  expect(canSend(true, "hello", true)).toBe(false)
  expect(await probeAgentRoute(async () => { throw new TypeError("network") })).toBe(false)
  expect(await probeAgentRoute(async () => new Response(null, { status: 404 }))).toBe(false)
  expect(await probeAgentRoute(async () => new Response(null, { status: 503 }))).toBe(false)
  expect(await probeAgentRoute(async () => new Response(null, { status: 405 }))).toBe(true)
  // A probe that never settles leaves the panel pending: the composer is disabled and says it is checking.
  let settled = false
  void probeAgentRoute(() => new Promise<Response>(() => {})).then(() => { settled = true })
  await new Promise((resolve) => setTimeout(resolve, 10))
  expect(settled).toBe(false)
  const pending: AgentState = { chat: new Chat<UIMessage>({}), input: "draft", setInput: () => {}, stopped: new Set(), setStopped: () => {} }
  const html = renderToStaticMarkup(<AgentPanel state={pending} />)
  expect(html).toMatch(/<fieldset disabled=""/)
  expect(html).toContain("Checking the agent service…")
})

// Lets pending promise callbacks run (the probe's .finally adds hops) without advancing fake time.
const settle = async () => { for (let hop = 0; hop < 10; hop += 1) await Promise.resolve() }

test("a probe whose request never answers settles as not served at the deadline, and aborts the request", async () => {
  jest.useFakeTimers()
  try {
    let request: AbortSignal | undefined
    let answer: boolean | undefined
    void probeAgentRoute((_input, init) => { request = init?.signal ?? undefined; return new Promise<Response>(() => {}) }).then((served) => { answer = served })
    jest.advanceTimersByTime(PROBE_DEADLINE_MS - 1)
    await settle()
    expect(answer).toBeUndefined()
    jest.advanceTimersByTime(1)
    await settle()
    expect(answer).toBe(false)
    expect(request?.aborted).toBe(true)
  } finally {
    jest.useRealTimers()
  }
})

test("unmounting aborts the probe's request, and the probe settles as not served", async () => {
  const unmounted = new AbortController()
  let request: AbortSignal | undefined
  const probe = probeAgentRoute((_input, init) => {
    request = init?.signal ?? undefined
    return new Promise<Response>((_resolve, reject) => request?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError"))))
  }, { signal: unmounted.signal })
  unmounted.abort()
  expect(request?.aborted).toBe(true)
  expect(await probe).toBe(false)
})
