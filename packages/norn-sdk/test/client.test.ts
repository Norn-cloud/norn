import { expect, test } from "bun:test"
// Wire documents in the shapes crates/norn-cell-do/tests/actions.rs pins, with neutral names.
import served from "./fixtures/action-call.json"
import { createIntentKey, createNornClient, NornUnavailable, type CallResult } from "../src/index.ts"

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
const body = (sent: Sent) => JSON.parse(sent.body ?? "null") as { input: unknown; idempotencyKey: string; trace?: unknown }
const failure = (promise: Promise<unknown>) => promise.then(() => null, (error: unknown) => error)

test("SDK-CALL-1: a call answers exactly receipt, pending or refused", async () => {
  const answers: Response[] = [
    Response.json(served.committed),
    Response.json(served.pending, { status: 202 }),
    Response.json(served.conflict, { status: served.status.conflict }),
    Response.json(served.unconfigured, { status: served.status.unconfigured }),
  ]
  const wire = cell(() => answers.shift()!)
  const client = createNornClient<Actions>({ baseUrl: "https://cell.test", scope, fetch: wire.fetch })
  const results: CallResult[] = []
  for (let i = 0; i < 4; i++) results.push(await client.call("tracking.scan.record", { scan_id: "s-1", weight_kg: "7.50" }, { idempotencyKey: createIntentKey() }))
  expect(results.map((r) => r.kind)).toEqual(["receipt", "pending", "refused", "refused"])
  expect(results[0]).toEqual({ kind: "receipt", receipt: served.committed as never })
  expect(results[1]).toEqual({ kind: "pending", decision: served.pending.decision })
  // The generated Refusal@2 members are kept: category, retryable, explanation, decidedAt.
  expect(results[2]).toEqual({
    kind: "refused", status: 409, code: "action.idempotency_conflict", message: served.conflict.message, diagnostics: [],
    category: "conflict", retryable: false, explanation: served.conflict.explanation, decidedAt: served.conflict.decidedAt,
  })
  // A 503 that carries a refusal is that refusal, not an unserved route.
  expect(results[3]).toMatchObject({ kind: "refused", status: 503, code: "action.receipts_unconfigured", category: "internal", retryable: true })
  // Scope is explicit on every request; the exact decimal reaches the cell as its text.
  expect(wire.sent[0]!.url).toBe("https://cell.test/v1/actions/tracking.scan.record?orgId=org_a&environmentId=dev")
  expect(wire.sent[0]!.body).toContain('"weight_kg":"7.50"')
  // Every arm is handled: the switch is exhaustive over three kinds.
  const label = (r: CallResult) => { switch (r.kind) { case "receipt": return r.receipt.objectId; case "pending": return r.decision.id; case "refused": return r.code } }
  expect(results.map(label)).toEqual([served.committed.objectId, served.pending.decision.id, "action.idempotency_conflict", "action.receipts_unconfigured"])
})

test("SDK-CALL-2: what is none of the three arms throws NornUnavailable; nothing is guessed", async () => {
  for (const [response, status, notServed] of [
    [new Response("", { status: 404 }), 404, true],
    // A 404 is the route's absence even when its body is a refusal (`route.not_found`).
    [Response.json(served.routeNotFound, { status: 404 }), 404, true],
    [new Response("", { status: 503 }), 503, true],
    // A gateway's 503 with a `code` is still not a Refusal@2.
    [Response.json({ code: "host.down", message: "down" }, { status: 503 }), 503, true],
    [new Response("<html>bad gateway</html>", { status: 502 }), 502, false],
    [Response.json({ outcome: "pending" }, { status: 202 }), 202, false],
    [new Response("not json", { status: 200 }), 200, false],
    // A 2xx is a receipt only when it is one.
    [Response.json({}), 200, false],
    [Response.json([]), 200, false],
    [Response.json({ ...served.committed, outcome: "maybe" }), 200, false],
    [Response.json({ ...served.committed, receipt: { receiptHash: "h" } }), 200, false],
  ] as const) {
    const client = createNornClient({ scope, fetch: cell(() => response.clone()).fetch })
    const error = await failure(client.call("any.action", {}, { idempotencyKey: "k" }))
    expect(error).toBeInstanceOf(NornUnavailable)
    expect((error as NornUnavailable).status).toBe(status)
    expect((error as NornUnavailable).notServed).toBe(notServed)
  }
})

test("SDK-CALL-3: no answer at all is NornUnavailable with status 0 and its cause; an abort or a host error passes through", async () => {
  const lost = new TypeError("fetch failed")
  const offline = createNornClient({ scope, fetch: async () => { throw lost } })
  const error = await failure(offline.call("any.action", {}, { idempotencyKey: "k" })) as NornUnavailable
  expect(error).toBeInstanceOf(NornUnavailable)
  expect([error.status, error.notServed, error.cause]).toEqual([0, false, lost])
  expect(error.message).toContain("(network)")
  expect(await failure(offline.listActions())).toBeInstanceOf(NornUnavailable)

  const controller = new AbortController()
  controller.abort()
  const aborted = createNornClient({ scope, fetch: async () => { throw new TypeError("aborted") } })
  expect(await failure(aborted.call("any.action", {}, { idempotencyKey: "k", signal: controller.signal }))).not.toBeInstanceOf(NornUnavailable)
  class HostError extends Error {}
  const host = createNornClient({ scope, fetch: async () => { throw new HostError("scope moved") } })
  expect(await failure(host.call("any.action", {}, { idempotencyKey: "k" }))).toBeInstanceOf(HostError)
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
  const lost = await failure(intent.send())
  expect(lost).toBeInstanceOf(NornUnavailable)
  expect((lost as NornUnavailable).status).toBe(0)
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
  // `callEncoded` sends JSON text as typed, digit for digit, under the caller's key, with its trace.
  await client.callEncoded("tracking.scan.record", '{"scan_id":"s-2","weight_kg":"1.250"}', { idempotencyKey: "k2", trace: { traceId: "t-1" } })
  expect(wire.sent.at(-1)!.body).toBe('{"input":{"scan_id":"s-2","weight_kg":"1.250"},"idempotencyKey":"k2","trace":{"traceId":"t-1"}}')
})

test("SDK-DEFS-1: a definitions version pins calls; a catalog from another version reads as stale", async () => {
  const wire = cell((sent) => sent.method === "GET"
    ? Response.json({ definitionsVersion: 8, items: [{ actionId: "ml.model.register" }] })
    : Response.json(served.stale, { status: served.status.stale }))
  const version = (sent: Sent) => new URL(sent.url, "http://x").searchParams.get("version")
  const pinned = createNornClient<Actions>({ scope, fetch: wire.fetch, definitionsVersion: 7 })
  const call = await pinned.call("ml.model.register", { model_id: "m-1" }, { idempotencyKey: "k" })
  expect(version(wire.sent[0]!)).toBe("7")
  expect(call).toMatchObject({ kind: "refused", status: 409, code: "defs.stale" })
  // The client's own check: no 409 is invented, the catalog is simply not trusted.
  expect(await pinned.listActions()).toEqual({ kind: "stale", pinned: 7, served: 8 })

  // Read the version from the catalog and pin to it, without rebuilding the client.
  const open = createNornClient<Actions>({ scope, fetch: wire.fetch })
  const catalog = await open.listActions({ objectKind: "ml.Model", objectId: "m-1" })
  expect(catalog).toEqual({ kind: "catalog", items: [{ actionId: "ml.model.register" }] as never, definitionsVersion: 8 })
  expect(wire.sent.at(-1)!.url).toBe("/v1/actions?orgId=org_a&environmentId=dev&objectKind=ml.Model&objectId=m-1")
  if (catalog.kind !== "catalog") throw new Error("catalog")
  const current = open.pinned(catalog.definitionsVersion)
  expect(current.definitionsVersion).toBe(8)
  expect((await current.listActions()).kind).toBe("catalog")
  await current.call("ml.model.register", { model_id: "m-1" }, { idempotencyKey: "k" })
  expect(version(wire.sent.at(-1)!)).toBe("8")
  // Or per call, overriding the client's pin.
  await pinned.call("ml.model.register", { model_id: "m-1" }, { idempotencyKey: "k", definitionsVersion: 8 })
  expect(version(wire.sent.at(-1)!)).toBe("8")
  // Unpinned: no version is sent.
  await open.call("ml.model.register", { model_id: "m-1" }, { idempotencyKey: "k" })
  expect(new URL(wire.sent.at(-1)!.url, "http://x").searchParams.has("version")).toBe(false)
})

test("SDK-CATALOG-1: catalog refusals, unserved routes and malformed documents", async () => {
  const denied = createNornClient({ scope, fetch: cell(() => Response.json(served.denied, { status: 403 })).fetch })
  expect(await denied.listActions()).toMatchObject({ kind: "refused", status: 403, code: "authz.denied", category: "permission" })
  const absent = createNornClient({ scope, fetch: cell(() => Response.json(served.routeNotFound, { status: 404 })).fetch })
  expect(((await failure(absent.listActions())) as NornUnavailable).notServed).toBe(true)
  const unconfigured = createNornClient({ scope, fetch: cell(() => Response.json(served.unconfigured, { status: 503 })).fetch })
  expect(await unconfigured.listActions()).toMatchObject({ kind: "refused", status: 503, code: "action.receipts_unconfigured" })
  for (const document of [{ items: "no", definitionsVersion: 1 }, { items: [] }, []]) {
    const garbled = createNornClient({ scope, fetch: cell(() => Response.json(document)).fetch })
    expect(await failure(garbled.listActions())).toBeInstanceOf(NornUnavailable)
  }
  // An objectId without an objectKind names nothing: it is not sent.
  const wire = cell(() => Response.json({ definitionsVersion: 1, items: [] }))
  await createNornClient({ scope, fetch: wire.fetch, headers: () => ({ authorization: "Bearer t" }) }).listActions({ objectId: "x" })
  expect(wire.sent[0]!.url).toBe("/v1/actions?orgId=org_a&environmentId=dev")
  expect(wire.sent[0]!.headers["authorization"]).toBe("Bearer t")
})
