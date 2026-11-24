import { expect, test } from "bun:test"
// Wire documents the cell itself produced (crates/norn-cell-do/tests/actions.rs).
import served from "../../../apps/studio/test/fixtures/action-call.json"
import { createIntentKey, createNornClient, NornUnavailable, type CallResult } from "../src/client.ts"

interface Sent { readonly url: string; readonly method: string; readonly body: string | null; readonly headers: Record<string, string> }

function cell(answer: (sent: Sent) => Response) {
  const sent: Sent[] = []
  const fetch = async (url: string, init?: RequestInit) => {
    const request = { url, method: init?.method ?? "GET", body: typeof init?.body === "string" ? init.body : null, headers: (init?.headers ?? {}) as Record<string, string> }
    sent.push(request)
    return answer(request)
  }
  return { sent, fetch }
}

const scope = { orgId: "org_a", environmentId: "dev" }
// Two unrelated domains, as `norn codegen --ts` would type them.
type Actions = {
  readonly "tracking.scan.record": { readonly input: { readonly scan_id: string; readonly weight_kg: string }; readonly output: { readonly scan_id: string } }
  readonly "ml.model.register": { readonly input: { readonly model_id: string }; readonly output: { readonly model_id: string } }
}
const body = (sent: Sent) => JSON.parse(sent.body ?? "null") as { input: unknown; idempotencyKey: string }

test("SDK-CALL-1: a call answers exactly receipt, pending or refused", async () => {
  const answers: Response[] = [
    Response.json(served.committed),
    Response.json({ outcome: "pending", decision: { id: "dec_1", expiresAtMs: 1_800_000_000_000 } }, { status: 202 }),
    Response.json(served.conflict, { status: served.status.conflict }),
  ]
  const wire = cell(() => answers.shift()!)
  const client = createNornClient<Actions>({ baseUrl: "https://cell.test", scope, fetch: wire.fetch })
  const results: CallResult[] = []
  for (let i = 0; i < 3; i++) results.push(await client.call("tracking.scan.record", { scan_id: "s-1", weight_kg: "7.50" }, { idempotencyKey: createIntentKey() }))
  expect(results.map((r) => r.kind)).toEqual(["receipt", "pending", "refused"])
  expect(results[0]).toEqual({ kind: "receipt", receipt: served.committed as never })
  expect(results[1]).toEqual({ kind: "pending", decision: { id: "dec_1", expiresAtMs: 1_800_000_000_000 } })
  expect(results[2]).toMatchObject({ kind: "refused", status: 409, code: "action.idempotency_conflict", diagnostics: [], decidedAt: "2023-11-14T22:13:20Z" })
  // Scope is explicit on every request; the exact decimal reaches the cell as its text.
  expect(wire.sent[0]!.url).toBe("https://cell.test/v1/actions/tracking.scan.record?orgId=org_a&environmentId=dev")
  expect(wire.sent[0]!.body).toContain('"weight_kg":"7.50"')
  // Every arm is handled: the switch is exhaustive over three kinds.
  const label = (r: CallResult) => { switch (r.kind) { case "receipt": return r.receipt.objectId; case "pending": return r.decision.id; case "refused": return r.code } }
  expect(results.map(label)).toEqual([served.committed.objectId, "dec_1", "action.idempotency_conflict"])
})

test("SDK-CALL-2: what is none of the three arms throws NornUnavailable; nothing is guessed", async () => {
  for (const [response, notServed] of [
    [new Response("", { status: 404 }), true],
    [Response.json(served.routeNotFound, { status: 404 }), true],
    [new Response("", { status: 503 }), true],
    [new Response("<html>bad gateway</html>", { status: 502 }), false],
    [Response.json({ outcome: "pending" }, { status: 202 }), false],
    [new Response("not json", { status: 200 }), false],
  ] as const) {
    const client = createNornClient({ scope, fetch: cell(() => response.clone()).fetch })
    const error = await client.call("any.action", {}, { idempotencyKey: "k" }).then(() => null, (e: unknown) => e)
    expect(error).toBeInstanceOf(NornUnavailable)
    expect((error as NornUnavailable).notServed).toBe(notServed)
  }
})

test("SDK-INTENT-1: one key per user intent, reused on retry and never derived from the payload", async () => {
  let lose = true
  const wire = cell(() => {
    // The first answer is lost on the way back; the retry must be the same call.
    if (lose) { lose = false; throw new TypeError("network") }
    return Response.json(served.committed)
  })
  const client = createNornClient<Actions>({ scope, fetch: wire.fetch })
  const intent = client.intent("tracking.scan.record", { scan_id: "s-1", weight_kg: "8.00" })
  await expect(intent.send()).rejects.toThrow("network")
  expect((await intent.send()).kind).toBe("receipt")
  expect(wire.sent).toHaveLength(2)
  expect(body(wire.sent[0]!)).toEqual(body(wire.sent[1]!))
  expect(body(wire.sent[1]!).idempotencyKey).toBe(intent.idempotencyKey)

  // Two deliberate, identical submissions are two intents: two keys, so the cell commits both.
  const again = client.intent("tracking.scan.record", { scan_id: "s-1", weight_kg: "8.00" })
  expect(again.input).toBe(intent.input)
  expect(again.idempotencyKey).not.toBe(intent.idempotencyKey)
  expect(createIntentKey()).not.toBe(createIntentKey())

  // A key restored for a parked intent is sent as given.
  const restored = client.intent("ml.model.register", { model_id: "m-1" }, "kept-key")
  await restored.send()
  expect(body(wire.sent.at(-1)!).idempotencyKey).toBe("kept-key")
  // `callEncoded` sends JSON text as typed, digit for digit, under the caller's key.
  await client.callEncoded("tracking.scan.record", '{"scan_id":"s-2","weight_kg":"1.250"}', { idempotencyKey: "k2" })
  expect(wire.sent.at(-1)!.body).toBe('{"input":{"scan_id":"s-2","weight_kg":"1.250"},"idempotencyKey":"k2"}')
})

test("SDK-DEFS-1: a pinned definitions version pins every call and fails closed on a stale catalog", async () => {
  const wire = cell((sent) => sent.method === "GET"
    ? Response.json({ definitionsVersion: 8, items: [{ actionId: "ml.model.register" }] })
    : Response.json(served.stale, { status: served.status.stale }))
  const pinned = createNornClient<Actions>({ scope, fetch: wire.fetch, definitionsVersion: 7 })
  const call = await pinned.call("ml.model.register", { model_id: "m-1" }, { idempotencyKey: "k" })
  expect(new URL(wire.sent[0]!.url, "http://x").searchParams.get("version")).toBe("7")
  expect(call).toMatchObject({ kind: "refused", status: 409, code: "defs.stale" })
  // The catalog was read from another version: refused before any item is trusted.
  expect(await pinned.listActions()).toMatchObject({ kind: "refused", status: 409, code: "defs.stale", diagnostics: [] })

  const current = createNornClient<Actions>({ scope, fetch: wire.fetch, definitionsVersion: 8 })
  expect(await current.listActions({ objectKind: "ml.Model", objectId: "m-1" })).toEqual({ kind: "catalog", items: [{ actionId: "ml.model.register" }] as never, definitionsVersion: 8 })
  expect(wire.sent.at(-1)!.url).toBe("/v1/actions?orgId=org_a&environmentId=dev&objectKind=ml.Model&objectId=m-1")
  // Unpinned: no version is sent and the catalog's version is reported, not enforced.
  const open = createNornClient({ scope, fetch: wire.fetch })
  await open.call("ml.model.register", { model_id: "m-1" }, { idempotencyKey: "k" })
  expect(new URL(wire.sent.at(-1)!.url, "http://x").searchParams.has("version")).toBe(false)
  expect(await open.listActions()).toMatchObject({ kind: "catalog", definitionsVersion: 8 })
})

test("SDK-CATALOG-1: catalog refusals and unserved routes", async () => {
  const denied = createNornClient({ scope, fetch: cell(() => Response.json(served.denied, { status: 403 })).fetch })
  expect(await denied.listActions()).toMatchObject({ kind: "refused", status: 403, code: "authz.denied" })
  const absent = createNornClient({ scope, fetch: cell(() => new Response("", { status: 404 })).fetch })
  expect(await absent.listActions().then(() => null, (e: unknown) => (e as NornUnavailable).notServed)).toBe(true)
  const garbled = createNornClient({ scope, fetch: cell(() => Response.json({ items: "no" })).fetch })
  expect(await garbled.listActions().then(() => null, (e: unknown) => e)).toBeInstanceOf(NornUnavailable)
  // An objectId without an objectKind names nothing: it is not sent.
  const wire = cell(() => Response.json({ items: [] }))
  await createNornClient({ scope, fetch: wire.fetch, headers: () => ({ authorization: "Bearer t" }) }).listActions({ objectId: "x" })
  expect(wire.sent[0]!.url).toBe("/v1/actions?orgId=org_a&environmentId=dev")
  expect(wire.sent[0]!.headers["authorization"]).toBe("Bearer t")
})
