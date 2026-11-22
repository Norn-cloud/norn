import { expect, test } from "bun:test"
import { forgetParkedCalls, parkedCalls } from "../src/actions/parked-calls.ts"

const CALL = { decision: { id: "dec_1", expiresAtMs: 9 }, input: '{"amount":1240.000}', key: "k-parked" }

test("a parked call is kept exactly as sent, per session and form", () => {
  forgetParkedCalls()
  parkedCalls.set("s1", "ops.refund:{}", CALL)
  expect(parkedCalls.get("s1", "ops.refund:{}")).toEqual(CALL)
  // Another session, or another form, never sees it.
  expect(parkedCalls.get("s2", "ops.refund:{}")).toBeUndefined()
  expect(parkedCalls.get("s1", 'ops.refund:{"id":"r-2"}')).toBeUndefined()
})

test("the kept call is a copy: a later change to the sent object does not rewrite it", () => {
  forgetParkedCalls()
  const sent = { decision: { ...CALL.decision }, input: CALL.input, key: CALL.key }
  parkedCalls.set("s1", "f", sent)
  sent.decision.id = "dec_other"
  expect(parkedCalls.get("s1", "f")).toEqual(CALL)
})

test("a commit or a sign-out forgets it", () => {
  forgetParkedCalls()
  parkedCalls.set("s1", "f", CALL)
  parkedCalls.delete("s1", "f")
  expect(parkedCalls.get("s1", "f")).toBeUndefined()
  parkedCalls.set("s1", "f", CALL)
  forgetParkedCalls()
  expect(parkedCalls.get("s1", "f")).toBeUndefined()
})
