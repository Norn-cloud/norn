import { expect, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { renderToStaticMarkup } from "react-dom/server"
import { inviteFromLocation, LoginPage, SIGNED_UP, type Mode } from "../src/login.tsx"
import { readAuthConfig, type AuthConfig } from "../src/lib/session.ts"

const render = (initial: Mode, config?: AuthConfig) =>
  renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><LoginPage initial={initial} config={config} /></QueryClientProvider>)

test("sign-in asks for email and password and hides Email link unless server config enables it", () => {
  const withoutMagic = render("sign-in")
  expect(withoutMagic).toContain('autoComplete="current-password"')
  expect(withoutMagic).toContain('type="email"')
  expect(withoutMagic).toContain("Create account")
  expect(withoutMagic).not.toContain("Email link")
  expect(withoutMagic).not.toContain('name="name"')

  // Even requesting email-link initially falls back to sign-in when magic links are off.
  expect(render("email-link", { magicLink: false, inviteOnly: false })).not.toContain("Send link")

  // When server config reports magicLink: true, the Email link tab is shown.
  const withMagic = render("sign-in", { magicLink: true, inviteOnly: false })
  expect(withMagic).toContain("Email link")
})

test("creating an account asks for a name and a 12-character password and states invite-only when configured", () => {
  const open = render("sign-up", { magicLink: false, inviteOnly: false })
  expect(open).toContain('name="name"')
  expect(open).toContain('minLength="12"')
  expect(open).toContain('autoComplete="new-password"')
  expect(open).not.toContain("invite-only")
  expect(open).not.toContain('name="invite"')

  const inviteOnly = render("sign-up", { magicLink: false, inviteOnly: true })
  expect(inviteOnly).toContain("invite-only")
  expect(inviteOnly).not.toContain("Email link")
  // Invite-only sign-up carries the operator's invitation (from the link's #invite=), required.
  expect(inviteOnly).toContain('name="invite"')
  expect(inviteFromLocation("#invite=v1.123.abc.def")).toBe("v1.123.abc.def")
  expect(inviteFromLocation("#other=1")).toBe("")
})

test("an email link needs only the address when the server enables magic links", () => {
  const html = render("email-link", { magicLink: true, inviteOnly: false })
  expect(html).not.toContain('type="password"')
  expect(html).toContain("Send link")
})

test("readAuthConfig parses server booleans strictly and rejects transient failures so queries can retry", async () => {
  expect(await readAuthConfig(async () => Response.json({ magicLink: true, inviteOnly: true }))).toEqual({ magicLink: true, inviteOnly: true })
  expect(await readAuthConfig(async () => Response.json({ magicLink: false, inviteOnly: false }))).toEqual({ magicLink: false, inviteOnly: false })
  expect(await readAuthConfig(async () => Response.json({ magicLink: "yes", inviteOnly: 1 }))).toEqual({ magicLink: false, inviteOnly: false })
  await expect(readAuthConfig(async () => new Response(null, { status: 503 }))).rejects.toThrow("503")
  await expect(readAuthConfig(async () => { throw new TypeError("offline") })).rejects.toThrow("offline")
})

test("readWorkspaces returns null only on 401/404 and throws retryable WorkspaceError on 503/offline", async () => {
  const { readWorkspaces, WorkspaceError } = await import("../src/lib/workspaces-api.ts")
  expect(await readWorkspaces(async () => new Response(null, { status: 401 }))).toBeNull()
  expect(await readWorkspaces(async () => new Response(null, { status: 404 }))).toBeNull()
  await expect(readWorkspaces(async () => new Response(null, { status: 503 }))).rejects.toBeInstanceOf(WorkspaceError)
  await expect(readWorkspaces(async () => { throw new TypeError("offline") })).rejects.toBeInstanceOf(WorkspaceError)
})

test("after sign-up the page asks the user to sign in with conditional wording, never claiming the address was new", () => {
  expect(SIGNED_UP).toBe("If this address can sign up, you can now sign in with your email and password.")
  expect(SIGNED_UP).not.toMatch(/account ready/i)
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

// PR #1631 S1: changing the active cell environment within the SAME user and org invalidates the
// session key, forgets parked calls, and clears cached mode/learning queries.
test("an environment switch within the same user and org invalidates sessionKey, parked calls, and cached reads", async () => {
  const { accountQuery } = await import("../src/lib/session.ts")
  const { sessionKey } = await import("../src/actions/kept-drafts.ts")
  const { forgetParkedCalls, parkedCalls } = await import("../src/actions/parked-calls.ts")
  forgetParkedCalls()
  const inEnv = (environmentId: string | null) => ({
    user: { id: "u1", name: "Alice", email: "a@example.com" },
    organization: { id: "org_demo", name: "Demo", role: "member" },
    environmentId,
  })
  expect(sessionKey(inEnv("env-a"))).not.toBe(sessionKey(inEnv("env-b")))
  expect(sessionKey(inEnv("env-a"))).not.toBe(sessionKey(inEnv(null)))

  const answers: Response[] = []
  const client = new QueryClient()
  const read = (answer: Response) => {
    answers.push(answer)
    return client.fetchQuery({ ...accountQuery(client, async () => answers.shift() as Response), staleTime: 0 })
  }
  const call = { decision: { id: "dec_env", expiresAtMs: 9 }, input: '{"amount":"5"}', key: "k-env" }
  await read(Response.json(inEnv("env-a")))
  parkedCalls.set(sessionKey(inEnv("env-a")), "learning.triage:{}", call)
  client.setQueryData(["studio-mode", '["org_demo","env-a"]'], { kind: "live" })
  client.setQueryData(["learning", "episodes", '["org_demo","env-a"]', ""], { kind: "page" })

  // Same user, same org, NEW environment -> parked calls and cached reads are cleared.
  await read(Response.json(inEnv("env-b")))
  expect(parkedCalls.get(sessionKey(inEnv("env-a")), "learning.triage:{}")).toBeUndefined()
  expect(client.getQueryData(["studio-mode", '["org_demo","env-a"]'])).toBeUndefined()
  expect(client.getQueryData(["learning", "episodes", '["org_demo","env-a"]', ""])).toBeUndefined()
})

// #1700 B4: a role downgrade in the same user, org and environment is a different reader: the
// previous role's catalog, mode and counts are forgotten even though the workspace scope is unchanged.
test("a role change in the same workspace forgets the previous role's catalog, actions and counts", async () => {
  const { accountQuery } = await import("../src/lib/session.ts")
  const { sessionKey } = await import("../src/actions/kept-drafts.ts")
  const asRole = (role: string) => ({
    user: { id: "u1", name: "Alice", email: "a@example.com" },
    organization: { id: "org_demo", name: "Demo", role },
    environmentId: "env-a",
  })
  expect(sessionKey(asRole("admin"))).not.toBe(sessionKey(asRole("viewer")))

  const answers: Response[] = []
  const client = new QueryClient()
  const read = (answer: Response) => {
    answers.push(answer)
    return client.fetchQuery({ ...accountQuery(client, async () => answers.shift() as Response), staleTime: 0 })
  }
  await read(Response.json(asRole("admin")))
  // The scope query already agrees with the session: only the role is about to change.
  client.setQueryData(["studio-scope"], { orgId: "org_demo", environmentId: "env-a" })
  client.setQueryData(["studio-mode", '["org_demo","env-a"]'], { kind: "live" })
  client.setQueryData(["actions", "canon.Item", null, '["org_demo","env-a"]'], { kind: "catalog", items: [{ actionId: "canon.item.delete" }] })
  client.setQueryData(["facets", "canon.Item"], { rows: 12 })

  await read(Response.json(asRole("viewer")))
  expect(client.getQueryData(["studio-mode", '["org_demo","env-a"]'])).toBeUndefined()
  expect(client.getQueryData(["actions", "canon.Item", null, '["org_demo","env-a"]'])).toBeUndefined()
  expect(client.getQueryData(["facets", "canon.Item"])).toBeUndefined()
})

// #1700 S13: a session check that never answers fails within its deadline, so the root can offer a retry.
test("a stalled session check fails within its deadline instead of leaving Studio blank", async () => {
  const { accountQuery } = await import("../src/lib/session.ts")
  const { CellUnavailable } = await import("../src/lib/definitions.ts")
  const client = new QueryClient()
  const stalled = (_input: string, init?: RequestInit) => new Promise<Response>((_resolve, reject) => {
    init?.signal?.addEventListener("abort", () => reject(init.signal?.reason))
  })
  const started = Date.now()
  await expect(client.fetchQuery(accountQuery(client, stalled, 20))).rejects.toBeInstanceOf(CellUnavailable)
  expect(Date.now() - started).toBeLessThan(2_000)
})
