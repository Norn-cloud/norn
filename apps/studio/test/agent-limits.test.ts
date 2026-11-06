import { expect, test } from "bun:test"
import { EMPTY, admit, spend, type Limits } from "../worker/limits.ts"

const limits: Limits = { requestsPerHour: 2, tokensPerDay: 100, reserve: 30 }
const HOUR = 3_600_000

test("a scope admits its hourly requests, then refuses until the hour turns", () => {
  const first = admit(EMPTY, limits, 1_000)
  if (!first.ok) throw new Error("first request")
  const second = admit(first.ledger, limits, 2_000)
  if (!second.ok) throw new Error("second request")
  const third = admit(second.ledger, limits, 3_000)
  expect(third.ok).toBe(false)
  if (!third.ok) expect(third.retryAfterSeconds).toBe(3_598)
  expect(admit(second.ledger, limits, 1_000 + HOUR).ok).toBe(true)
})

test("a scope refuses once its remaining tokens cannot cover one more request", () => {
  const used = spend(EMPTY, 70, 1_000)
  expect(admit(used, limits, 2_000).ok).toBe(true)
  const over = admit(spend(used, 1, 2_000), limits, 3_000)
  expect(over.ok).toBe(false)
  if (!over.ok) expect(over.reason).toContain("budget")
  expect(admit(spend(used, 1, 2_000), limits, 1_000 + 24 * HOUR).ok).toBe(true)
})
