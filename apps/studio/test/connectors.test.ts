import { expect, test } from "bun:test"
import { api, BINDING_VERBS, ConnectorError, CONNECTION_VERBS, ERROR_VERBS, EXECUTABLE_FIXES, fixAction, fixLabel, MATRIX, matrixTable, offersButton, startFix } from "../src/lib/connectors.ts"

const answering = (status: number, body: unknown) => (async () => Response.json(body, { status })) as unknown as typeof fetch

test("a refusal becomes a typed error carrying the server's fix", async () => {
  const refused = api("/inventory", undefined, answering(409, { code: "connectors.account_reconnect", error: "Reconnect it.", action: { kind: "reconnect_account", app: "github", account: "acc1" } }))
  const error = await refused.catch((e: unknown) => e)
  expect(error).toBeInstanceOf(ConnectorError)
  expect(error).toMatchObject({ status: 409, code: "connectors.account_reconnect", message: "Reconnect it.", action: { kind: "reconnect_account", account: "acc1" } })
  expect(fixLabel((error as ConnectorError).action ?? { kind: "ask_admin" })).toBe("Reconnect the account")
  const unreadable = await api("/status", undefined, (async () => new Response("<html>", { status: 502 })) as unknown as typeof fetch).catch((e: unknown) => e)
  expect(unreadable).toMatchObject({ code: "connectors.unreadable", action: null })
  const fallback = await api("/status", undefined, (async () => new Response("<!doctype html>", { status: 200 })) as unknown as typeof fetch).catch((e: unknown) => e)
  expect(fallback).toMatchObject({ status: 200, code: "connectors.unreadable" })
})

test("every kind's verbs are ✓ or N/A with a reason, and the table says so", () => {
  for (const verb of CONNECTION_VERBS) {
    expect(MATRIX["App account"][verb]).toBe("supported")
    const support = MATRIX["Gateway sign-in"][verb]
    expect(support === "supported" || support.na.length > 0).toBe(true)
  }
  for (const verb of ERROR_VERBS) expect(MATRIX["Connector error"][verb]).toBe("supported")
  for (const verb of BINDING_VERBS) expect(MATRIX["Tool binding"][verb]).toBe("supported")
  expect(matrixTable(MATRIX)).toContain("N/A: one grant per gateway; it has no name of its own")
})

test("only a fix action the page knows reaches it", () => {
  expect(fixAction({ kind: "reconnect_account", app: "github", account: "acc1" })).toEqual({ kind: "reconnect_account", app: "github", account: "acc1" })
  expect(fixAction({ kind: "reconnect_account", app: null, account: "acc1" })).toEqual({ kind: "reconnect_account", app: null, account: "acc1" })
  expect(fixAction({ kind: "connect_account", app: "github" })).toBeNull()
  expect(fixAction({ kind: "delete_everything" })).toBeNull()
  expect(fixAction("sign_in_gateway")).toBeNull()
  expect(fixAction(null)).toBeNull()
})

test("every fix the page offers a button for executes: it reaches a gateway page", async () => {
  const calls: string[] = []
  const gatewayPage = "https://gateway.example/form"
  const call = (async (path: string) => {
    calls.push(path)
    if (path === "/link") return { url: gatewayPage }
    if (path === "/accounts/acc1") return { apps: ["other", "github"] }
    if (path === "/apps/other/accounts") return { requirements: [{ requirement: "x", account: "acc9", state: null }] }
    if (path === "/apps/github/accounts") return { requirements: [{ requirement: "github", account: "acc1", state: "reconnect_required" }] }
    if (path.endsWith("/connect")) return { url: gatewayPage }
    throw new Error(`unexpected ${path}`)
  }) as Parameters<typeof startFix>[1]
  const offered = [
    { kind: "sign_in_gateway" }, { kind: "connect_account", app: "github", requirement: "github" },
    { kind: "reconnect_account", app: "github", account: "acc1" }, { kind: "reconnect_account", app: null, account: "acc1" },
  ] as const
  for (const action of offered) {
    expect(offersButton(action)).toBe(true)
    expect(await startFix(action, call)).toBe(gatewayPage)
  }
  // A reconnect without its app found the app from the account, and reconnected that account.
  expect(calls).toContain("/accounts/acc1")
  // connect_account, and both reconnects (with and without its app), each start one connection.
  expect(calls.filter((p) => p === "/apps/github/accounts/github/connect").length).toBe(3)
  // Every kind with a button has a handler; advice has no button.
  expect([...EXECUTABLE_FIXES].sort()).toEqual(["connect_account", "reconnect_account", "sign_in_gateway"])
  expect(offersButton({ kind: "ask_admin" })).toBe(false)
  // An account linked to no app fails with a typed error, never silently.
  const unlinked = (async (path: string) => path === "/accounts/gone" ? { apps: [] } : { requirements: [] }) as Parameters<typeof startFix>[1]
  expect(await startFix({ kind: "reconnect_account", app: null, account: "gone" }, unlinked).catch((e: unknown) => e)).toMatchObject({ code: "connectors.account_not_linked" })
})
