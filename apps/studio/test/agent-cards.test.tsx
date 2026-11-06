import { expect, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import { DecisionCard, VerdictCard, reasons } from "../src/agent/cards.tsx"
import { sessionId, stepOf, verdictOf } from "../src/agent/verdict.ts"

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
  expect(html).toContain("self.ordered &gt; 0")
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

test("the progress steps follow the stream: drafting, then checking, then the answer", () => {
  const turn = (parts: unknown[]) => ({ id: "m", role: "assistant", parts }) as never
  expect(stepOf(undefined)).toBe(0)
  expect(stepOf(turn([{ type: "text", text: "Let me look." }]))).toBe(0)
  expect(stepOf(turn([{ type: "tool-propose", toolCallId: "t", state: "input-available", input: {} }]))).toBe(1)
  expect(stepOf(turn([{ type: "tool-propose", toolCallId: "t", state: "output-available", input: {}, output: refusal }]))).toBe(2)
})
