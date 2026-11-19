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
