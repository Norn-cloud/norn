import { expect, test } from "bun:test"
import { guardWords, guardsWords, refusedGuardWords } from "../src/lib/guard-words.ts"

test("a plain comparison guard reads in words", () => {
  expect(guardWords("self.amount <= 1000")).toBe("amount ≤ 1000")
  expect(guardWords("self.amount > 1000")).toBe("amount > 1000")
  expect(guardWords("self.status == 'draft' and self.total_due >= 0.5")).toBe("status is draft and total due ≥ 0.5")
  expect(guardWords("self.eligible == true")).toBe("eligible is true")
  expect(guardsWords(["self.amount > 0", "self.currency != \"EGP\""])).toBe("amount > 0 and currency is not EGP")
})

test("anything else keeps the caller's fallback", () => {
  expect(guardWords("len(self.lines) > 0")).toBeNull()
  expect(guardWords("self.amount > self.limit")).toBeNull()
  expect(guardsWords([])).toBeNull()
  expect(guardsWords(["self.amount > 0", "sum(self.lines) > 0"])).toBeNull()
})

test("the guard a refusal quotes reads in words", () => {
  expect(refusedGuardWords("purchasing.PurchaseRequest/PR-1: guard `self.amount <= 1000` of transition submit_small does not hold")).toBe("amount ≤ 1000")
  expect(refusedGuardWords("a declared precondition does not hold")).toBeNull()
})
