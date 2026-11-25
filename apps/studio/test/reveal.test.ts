import { expect, test } from "bun:test"
import { revealContext, revealKeys, revealProperty, shownReveal, type Masked, type Revealed } from "../src/lib/privacy.ts"

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

// #1700 B5: a reveal goes to the workspace the row was read from, never the one the session shows at click time.
test("a reveal is sent to the row's workspace with its expected-scope headers and never re-reads the session", async () => {
  const sent: { readonly url: string; readonly headers: Headers }[] = []
  const fetcher = async (input: string, init?: RequestInit) => {
    if (input.startsWith("/api/me")) throw new Error("the session must not be read for a bound reveal")
    sent.push({ url: input, headers: new Headers(init?.headers) })
    return Response.json({ value: "Amira Haddad", receipt })
  }
  const result = await revealProperty("canon.Person", "p1", "display_name", "", "key-1", fetcher, { orgId: "org_a", environmentId: "env_a" })
  expect(result.kind).toBe("revealed")
  expect(sent).toHaveLength(1)
  const url = new URL(sent[0]!.url, "https://studio.test")
  expect([url.searchParams.get("orgId"), url.searchParams.get("environmentId")]).toEqual(["org_a", "env_a"])
  expect([sent[0]!.headers.get("x-norn-expected-org"), sent[0]!.headers.get("x-norn-expected-env")]).toEqual(["org_a", "env_a"])
})

// #1700 S12: a lost or stalled request leaves a retryable state, not a busy button.
test("a transport failure or a stalled reveal is unavailable, not a thrown error", async () => {
  const scope = { orgId: "org_a", environmentId: "env_a" }
  const offline = async () => { throw new TypeError("Failed to fetch") }
  expect(await revealProperty("canon.Person", "p1", "display_name", "", "k", offline, scope)).toEqual({ kind: "unavailable" })
  const stalled = (_input: string, init?: RequestInit) => new Promise<Response>((_resolve, reject) => {
    init?.signal?.addEventListener("abort", () => reject(init.signal?.reason))
  })
  expect(await revealProperty("canon.Person", "p1", "display_name", "", "k", stalled, scope, 10)).toEqual({ kind: "unavailable" })
})

test("a retried reveal reuses its idempotency key until the cell decides or the purpose changes", () => {
  let minted = 0
  const keys = revealKeys(() => `k${++minted}`)
  expect(keys.for("audit")).toBe("k1")
  expect(keys.for(" audit ")).toBe("k1")
  expect(keys.for("support ticket")).toBe("k2")
  keys.settle()
  expect(keys.for("support ticket")).toBe("k3")
})
