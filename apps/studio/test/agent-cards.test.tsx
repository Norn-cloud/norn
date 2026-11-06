import { expect, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import { DecisionCard, VerdictCard, reasons } from "../src/agent/cards.tsx"
import { sessionId, verdictOf } from "../src/agent/verdict.ts"
import { Chat } from "@ai-sdk/react"
import type { UIMessage } from "ai"
import { AgentPanel, sendsOnEnter, type AgentState } from "../src/agent/panel.tsx"

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

test("a verdict leads with the law in words; ids, pointers and codes are folded into Details", () => {
  const refused = verdictOf(refusal)
  const admitted = verdictOf({ proposal, admitted: true, stage: "admitted", receiptSha256: "eef45040c171aa", refusals: [] })
  if (refused === undefined || admitted === undefined) throw new Error("verdicts")
  for (const html of [renderToStaticMarkup(<VerdictCard verdict={refused} />), renderToStaticMarkup(<VerdictCard verdict={admitted} />)]) {
    const at = html.indexOf("<details")
    const [shown, folded] = [html.slice(0, at), html.slice(at)]
    expect(shown).not.toContain("/payload/")
    expect(shown).not.toContain("law.model.")
    expect(folded).toContain("/payload/response/expression")
  }
  expect(renderToStaticMarkup(<VerdictCard verdict={refused} />)).toContain("Stopped at: what the law means")
})

test("a refusal with no findings still names the proposal's location under Details", () => {
  const verdict = verdictOf({ ...refusal, refusals: [] })
  if (verdict === undefined) throw new Error("a verdict")
  const html = renderToStaticMarkup(<VerdictCard verdict={verdict} />)
  expect(html).toContain("Refused by the checker")
  expect(html.slice(html.indexOf("<details"))).toContain("stress.purchase_positive.law /payload/response/expression")
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
  const state: AgentState = { chat, input: "half-typed draft", setInput: () => {}, stopped: new Set(), setStopped: () => {} }
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
