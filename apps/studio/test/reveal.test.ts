import { expect, test } from "bun:test"
import { revealContext, shownReveal, type Masked, type Revealed } from "../src/lib/privacy.ts"

const allowed: Masked = { mode: "pseudonym", markings: ["pii.contact"], revealable: true }
const receipt = { commitSeq: 3, receiptHash: "sha256:00", revealedAt: 0, actor: "user:admin", property: "display_name", purpose: "", markings: ["pii.contact"] }
const revealedAt = (context: string): Revealed => ({ value: "Amira Haddad", receipt, context })

test("a reveal shows only under the projection and version it was revealed under", () => {
  const context = revealContext(1, allowed)
  expect(shownReveal(revealedAt(context), allowed, context)?.value).toBe("Amira Haddad")
  // A newer version starts masked again.
  expect(shownReveal(revealedAt(context), allowed, revealContext(2, allowed))).toBeNull()
  // A marking added to the property is another context.
  const remarked = { ...allowed, markings: ["pii.contact", "pii.identity"] }
  expect(shownReveal(revealedAt(context), remarked, revealContext(1, remarked))).toBeNull()
})

test("a reveal is never shown once the cell's projection of the same version no longer allows one", () => {
  const context = revealContext(1, allowed)
  const revoked: Masked = { ...allowed, revealable: false }
  expect(revealContext(1, revoked)).not.toBe(context)
  expect(shownReveal(revealedAt(context), revoked, revealContext(1, revoked))).toBeNull()
  // Even an answer recorded under the current context is not shown while the projection denies it.
  expect(shownReveal(revealedAt(revealContext(1, revoked)), revoked, revealContext(1, revoked))).toBeNull()
})

test("the context does not depend on the order the cell lists markings in", () => {
  const a = { ...allowed, markings: ["pii.contact", "pii.identity"] }
  const b = { ...allowed, markings: ["pii.identity", "pii.contact"] }
  expect(revealContext(1, a)).toBe(revealContext(1, b))
})
