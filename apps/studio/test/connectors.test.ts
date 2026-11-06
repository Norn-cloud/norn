import { expect, test } from "bun:test"
import { api, BINDING_VERBS, ConnectorError, CONNECTION_VERBS, ERROR_VERBS, fixAction, fixLabel, MATRIX, matrixTable } from "../src/lib/connectors.ts"

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
