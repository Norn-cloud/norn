import { expect, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { renderToStaticMarkup } from "react-dom/server"
import { LoginPage, SIGNED_UP, type Mode } from "../src/login.tsx"

const render = (initial: Mode) => renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><LoginPage initial={initial} /></QueryClientProvider>)

test("sign-in asks for email and password and offers the other ways in", () => {
  const html = render("sign-in")
  expect(html).toContain('autoComplete="current-password"')
  expect(html).toContain('type="email"')
  expect(html).toContain("Create account")
  expect(html).toContain("Email link")
  expect(html).not.toContain('name="name"')
})

test("creating an account asks for a name and a 12-character password", () => {
  const html = render("sign-up")
  expect(html).toContain('name="name"')
  expect(html).toContain('minLength="12"')
  expect(html).toContain('autoComplete="new-password"')
})

test("an email link needs only the address", () => {
  const html = render("email-link")
  expect(html).not.toContain('type="password"')
  expect(html).toContain("Send link")
})

test("after sign-up the page asks the user to sign in, never saying whether the address was new", () => {
  expect(SIGNED_UP).toBe("Account ready. Sign in with your email and password.")
  expect(SIGNED_UP).not.toMatch(/already|exists|registered|taken/i)
})

// A sign-out forgets every read made as that reader (the record picker's choices included), so the
// next reader in this tab never sees them; one the server did not complete forgets nothing.
test("signing out forgets the reader's reads", async () => {
  const { endSession } = await import("../src/lib/session.ts")
  const client = new QueryClient()
  const seed = () => {
    client.setQueryData(["session"], { user: { id: "u1" } })
    client.setQueryData(["picker", "purchasing.Supplier"], [{ id: "SUP-0001" }])
    client.setQueryData(["records", "purchasing.PurchaseRequest"], [])
  }
  seed()
  await expect(endSession(client, () => Promise.reject(new Error("offline")))).rejects.toThrow("offline")
  expect(client.getQueryData<unknown>(["picker", "purchasing.Supplier"])).toEqual([{ id: "SUP-0001" }])
  await endSession(client, async () => {})
  expect(client.getQueryData(["picker", "purchasing.Supplier"])).toBeUndefined()
  expect(client.getQueryData(["records", "purchasing.PurchaseRequest"])).toBeUndefined()
  expect(client.getQueryCache().find({ queryKey: ["session"] })).toBeDefined()
})

// #1469: a session that ends without a sign-out (it expired, and /api/me answers 401) forgets the
// reader's reads at that boundary too, so whoever signs in next in this tab never sees the previous
// reader's picker choices or records, not even before they are read again.
test("a session that expires forgets the reader's reads before the next reader signs in", async () => {
  const { accountQuery } = await import("../src/lib/session.ts")
  const alice = { user: { id: "u1", name: "Alice", email: "a@example.com" }, organization: { id: "o1", name: "Org", role: "member" } }
  const bob = { user: { id: "u2", name: "Bob", email: "b@example.com" }, organization: { id: "o1", name: "Org", role: "member" } }
  const answers: Response[] = []
  const client = new QueryClient()
  const read = (answer: Response) => {
    answers.push(answer)
    return client.fetchQuery({ ...accountQuery(client, async () => answers.shift() as Response), staleTime: 0 })
  }
  const cached = () => client.getQueryCache().findAll().filter((query) => query.queryKey[0] !== "session" && query.state.data !== undefined).map((query) => query.queryKey)

  expect(await read(Response.json(alice))).toEqual(alice)
  client.setQueryData(["picker", "purchasing.Supplier"], [{ id: "SUP-0001", label: "Acme" }])
  client.setQueryData(["records", "purchasing.PurchaseRequest"], [{ id: "PR-1" }])
  // The same reader read again keeps the reads.
  expect(await read(Response.json(alice))).toEqual(alice)
  expect(cached()).toHaveLength(2)
  // The session expires: no sign-out ran, and the reads are forgotten all the same.
  expect(await read(new Response(null, { status: 401 }))).toBeNull()
  expect(cached()).toEqual([])
  // Someone else signs in: nothing of the previous reader's is there to show.
  expect(await read(Response.json(bob))).toEqual(bob)
  expect(client.getQueryData(["picker", "purchasing.Supplier"])).toBeUndefined()
  expect(client.getQueryData(["records", "purchasing.PurchaseRequest"])).toBeUndefined()
  // A reader who changes without a 401 between (another tab signed in as someone else) forgets too.
  client.setQueryData(["picker", "purchasing.Supplier"], [{ id: "SUP-0002", label: "Bob's" }])
  expect(await read(Response.json(alice))).toEqual(alice)
  expect(cached()).toEqual([])
})

// #1469 S6: a kept draft belongs to the session that wrote it: another reader in this tab never
// sees it, and a completed sign-out forgets it.
test("kept drafts are the session's and a sign-out forgets them", async () => {
  const { endSession } = await import("../src/lib/session.ts")
  const { keptDrafts, sessionKey } = await import("../src/actions/kept-drafts.ts")
  const alice = sessionKey({ user: { id: "u1" }, organization: { id: "o1" } })
  const bob = sessionKey({ user: { id: "u2" }, organization: { id: "o1" } })
  const draft = { values: { amount: "12" } } as never
  keptDrafts.set(alice, "purchasing.request.submit:{}", draft)
  expect(keptDrafts.get(bob, "purchasing.request.submit:{}")).toBeUndefined()
  const client = new QueryClient()
  await expect(endSession(client, () => Promise.reject(new Error("offline")))).rejects.toThrow("offline")
  expect(keptDrafts.get(alice, "purchasing.request.submit:{}")).toBe(draft)
  await endSession(client, async () => {})
  expect(keptDrafts.get(alice, "purchasing.request.submit:{}")).toBeUndefined()
})

// #1532 S3: a parked call's exact input and key are its reader's. A tenant switch (the same user in
// another organization), or an expiry, forgets them, and switching back does not bring them back.
test("a tenant switch forgets parked calls, and switching back does not restore them", async () => {
  const { accountQuery } = await import("../src/lib/session.ts")
  const { sessionKey } = await import("../src/actions/kept-drafts.ts")
  const { forgetParkedCalls, parkedCalls } = await import("../src/actions/parked-calls.ts")
  forgetParkedCalls()
  const inOrg = (id: string) => ({ user: { id: "u1", name: "Alice", email: "a@example.com" }, organization: { id, name: id, role: "member" } })
  const answers: Response[] = []
  const client = new QueryClient()
  const read = (answer: Response) => {
    answers.push(answer)
    return client.fetchQuery({ ...accountQuery(client, async () => answers.shift() as Response), staleTime: 0 })
  }
  const call = { decision: { id: "dec_1", expiresAtMs: 9 }, input: '{"amount":"1240.000"}', key: "k-parked" }
  const form = "canon.holding.adjust:{}"
  await read(Response.json(inOrg("o1")))
  parkedCalls.set(sessionKey(inOrg("o1")), form, call)
  // The same reader read again keeps it.
  await read(Response.json(inOrg("o1")))
  expect(parkedCalls.get(sessionKey(inOrg("o1")), form)).toEqual(call)
  // The tenant switches, then back: the call is gone in both.
  await read(Response.json(inOrg("o2")))
  expect(parkedCalls.get(sessionKey(inOrg("o1")), form)).toBeUndefined()
  await read(Response.json(inOrg("o1")))
  expect(parkedCalls.get(sessionKey(inOrg("o1")), form)).toBeUndefined()
  // An expiry forgets it too.
  parkedCalls.set(sessionKey(inOrg("o1")), form, call)
  expect(await read(new Response(null, { status: 401 }))).toBeNull()
  expect(parkedCalls.get(sessionKey(inOrg("o1")), form)).toBeUndefined()
})
