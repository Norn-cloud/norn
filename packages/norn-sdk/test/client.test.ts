import { expect, test } from "bun:test"
// Wire documents in the shapes crates/norn-cell-do/tests/actions.rs pins, with neutral names.
import served from "./fixtures/action-call.json"
import { createIntentKey, createNornClient, NornUnavailable, NornUnexpectedResponse, type CallResult } from "../src/index.ts"

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

test("SDK-CALL-2: what is none of the three arms throws NornUnavailable, saying whether the request may have been applied", async () => {
  const notApplied = "not-applied", unknown = "unknown"
  for (const [response, status, notServed, outcome, unexpected] of [
    // A 404 is the route's absence, even when its body is a refusal (`route.not_found`): nothing was done.
    [new Response("", { status: 404 }), 404, true, notApplied, false],
    [Response.json(served.routeNotFound, { status: 404 }), 404, true, notApplied, false],
    // Anything else may have been applied before its answer was garbled.
    [new Response("", { status: 503 }), 503, true, unknown, false],
    [Response.json({ code: "host.down", message: "down" }, { status: 503 }), 503, true, unknown, false],
    [new Response("<html>bad gateway</html>", { status: 502 }), 502, false, unknown, false],
    // A gateway's `{ code, message }` is not the cell's refusal, whatever its status: the call behind
    // it may have reached the cell, so it is never read as "refused" (a fresh key would write twice).
    [Response.json({ code: "upstream.timeout", message: "timed out" }, { status: 502 }), 502, false, unknown, false],
    [Response.json({ code: "upstream.timeout", message: "timed out" }, { status: 504 }), 504, false, unknown, false],
    [Response.json({ code: "internal", message: "boom" }, { status: 500 }), 500, false, unknown, false],
    [Response.json({ code: "action.idempotency_conflict", message: "no format" }, { status: 409 }), 409, false, unknown, false],
    // A success status is a success document only when it is one; the cell said it succeeded.
    [Response.json({ outcome: "pending" }, { status: 202 }), 202, false, unknown, true],
    [Response.json({ outcome: "pending", decision: { id: "d", expiresAtMs: 1.5 } }, { status: 202 }), 202, false, unknown, true],
    [new Response("not json", { status: 200 }), 200, false, unknown, true],
    [Response.json({}), 200, false, unknown, true],
    [Response.json([]), 200, false, unknown, true],
    [Response.json({ ...served.committed, outcome: "maybe" }), 200, false, unknown, true],
    [Response.json({ ...served.committed, receipt: { receiptHash: "h" } }), 200, false, unknown, true],
    [Response.json({ ...served.committed, version: 1.5 }), 200, false, unknown, true],
    [Response.json({ ...served.committed, version: -1 }), 200, false, unknown, true],
    [Response.json({ ...served.committed, trace: {} }), 200, false, unknown, true],
    [Response.json({ ...served.committed, trace: { traceId: 7 } }), 200, false, unknown, true],
  ] as const) {
    const client = createNornClient({ scope, fetch: cell(() => response.clone()).fetch })
    const error = await failure(client.call("any.action", {}, { idempotencyKey: "k" })) as NornUnavailable
    expect(error).toBeInstanceOf(NornUnavailable)
    expect([status, error.status, error.notServed, error.outcome, error instanceof NornUnexpectedResponse]).toEqual([status, status, notServed, outcome, unexpected])
    // An unknown outcome never claims nothing was saved; it says to retry with the original key.
    if (outcome === unknown) {
      expect(error.message).not.toContain("nothing was")
      expect(error.message).toContain("original idempotency key")
    } else expect(error.message).toContain("nothing was read or saved")
  }
  // A receipt that carries a whole trace is a receipt.
  const traced = createNornClient({ scope, fetch: cell(() => Response.json({ ...served.committed, trace: { traceId: "t-1" } })).fetch })
  expect((await traced.call("any.action", {}, { idempotencyKey: "k" })).kind).toBe("receipt")
})

test("SDK-CALL-3: no answer, or an abort once a write was sent, is NornUnavailable (outcome unknown, the error its cause)", async () => {
  const lost = new TypeError("fetch failed")
  const offline = createNornClient({ scope, fetch: async () => { throw lost } })
  const error = await failure(offline.call("any.action", {}, { idempotencyKey: "k" })) as NornUnavailable
  expect(error).toBeInstanceOf(NornUnavailable)
  expect([error.status, error.notServed, error.cause]).toEqual([0, false, lost])
  expect(error.outcome).toBe("unknown")
  expect(error.message).toContain("could not be read, so the outcome is unknown")
  expect(await failure(offline.listActions())).toBeInstanceOf(NornUnavailable)

  // Aborted before it was sent: nothing was sent, and the abort passes through as is.
  const early = new AbortController()
  early.abort()
  let sent = 0
  const counting = createNornClient({ scope, fetch: async () => { sent++; return Response.json(served.committed) } })
  const notSent = await failure(counting.call("any.action", {}, { idempotencyKey: "k", signal: early.signal }))
  expect([notSent instanceof NornUnavailable, (notSent as Error).name, sent]).toEqual([false, "AbortError", 0])
  // Aborted once sent: a write may have been applied, so its outcome is unknown, the abort its cause.
  for (const during of ["fetch", "body"] as const) {
    const controller = new AbortController()
    const abort = new DOMException("The operation was aborted.", "AbortError")
    const fetchAborted = async () => {
      controller.abort(abort)
      if (during === "fetch") throw abort
      return { status: 200, ok: true, text: async () => { throw abort } } as unknown as Response
    }
    const write = await failure(createNornClient({ scope, fetch: fetchAborted }).call("any.action", {}, { idempotencyKey: "k", signal: controller.signal })) as NornUnavailable
    expect([during, write instanceof NornUnavailable, write.outcome, write.status, write.cause]).toEqual([during, true, "unknown", 0, abort])
  }
  // A read's abort passes through as is.
  const reading = new AbortController()
  const readAbort = new DOMException("The operation was aborted.", "AbortError")
  const read = await failure(createNornClient({ scope, fetch: async () => { reading.abort(readAbort); throw readAbort } }).listActions({}, reading.signal))
  expect(read).toBe(readAbort)
  // A body that stops arriving is no answer either.
  const broken = { status: 200, ok: true, text: async () => { throw new TypeError("terminated") } } as unknown as Response
  const truncated = await failure(createNornClient({ scope, fetch: async () => broken }).call("any.action", {}, { idempotencyKey: "k" })) as NornUnavailable
  expect(truncated).toBeInstanceOf(NornUnavailable)
  expect([truncated.status, (truncated.cause as Error).message]).toEqual([0, "terminated"])
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
    ? Response.json({ definitionsVersion: 8, items: [summary] })
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
  expect(catalog).toEqual({ kind: "catalog", items: [summary] as never, definitionsVersion: 8 })
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

test("SDK-REFUSAL-1: only a whole Refusal@2 envelope is a refusal; anything less is an unknown outcome", async () => {
  const partial = [
    { format: "norn/refusal@2", code: "x.y" },
    (({ retryable: _, ...rest }) => rest)(served.conflict),
    (({ reason: _, ...rest }) => rest)(served.conflict),
    { ...served.conflict, category: "law" },
    { ...served.conflict, reason: { law: "l" } },
    { ...served.invalidKey, diagnostics: [null] },
    { ...served.invalidKey, diagnostics: [{ code: "y", message: "n", pointer: "/input/a" }] },
  ]
  for (const [status, body] of partial.flatMap((b) => [[500, b], [409, b]] as const)) {
    const client = createNornClient({ scope, fetch: cell(() => Response.json(body, { status })).fetch })
    const error = await failure(client.call("any.action", {}, { idempotencyKey: "k" })) as NornUnavailable
    expect([status, body, error instanceof NornUnavailable, error.outcome]).toEqual([status, body, true, "unknown"])
  }
  // A whole envelope keeps its findings, each at its span's pointer.
  const findings = { ...served.invalidKey, diagnostics: [{ code: "x", severity: "error", message: "m", span: null }, { code: "y", severity: "error", message: "n", span: { pointer: "/input/a" } }] }
  const client = createNornClient({ scope, fetch: cell(() => Response.json(findings, { status: 400 })).fetch })
  const result = await client.call("any.action", {}, { idempotencyKey: "k" })
  expect(result.kind === "refused" && result.diagnostics).toEqual([{ code: "x", message: "m", pointer: null }, { code: "y", message: "n", pointer: "/input/a" }])
  // A disclosed reason is validated too, and kept.
  const reason = { law: "fixture.total_ok", subject: "fixture.Record/r-1", values: { total: "3" }, pin: { kind: "law", artifactId: "fixture.total_ok.law", lawId: "total_ok", version: "1.0.0" } }
  const disclosed = createNornClient({ scope, fetch: cell(() => Response.json({ ...served.precondition, reason }, { status: 422 })).fetch })
  expect(await disclosed.call("any.action", {}, { idempotencyKey: "k" })).toMatchObject({ kind: "refused", reason })
})

const summary = {
  actionId: "ml.model.register", title: "Register a model", objectKind: "ml.Model", effect: "upsert_input_object",
  objectIdField: "model_id", inputSchema: { type: "object", properties: {}, additionalProperties: false },
  uiEffects: { destructive: false, irreversible: false, money: [] },
}

test("SDK-CATALOG-1: catalog refusals, unserved routes and malformed documents", async () => {
  const denied = createNornClient({ scope, fetch: cell(() => Response.json(served.denied, { status: 403 })).fetch })
  expect(await denied.listActions()).toMatchObject({ kind: "refused", status: 403, code: "authz.denied", category: "permission" })
  const absent = createNornClient({ scope, fetch: cell(() => Response.json(served.routeNotFound, { status: 404 })).fetch })
  expect(((await failure(absent.listActions())) as NornUnavailable).notServed).toBe(true)
  const unconfigured = createNornClient({ scope, fetch: cell(() => Response.json(served.unconfigured, { status: 503 })).fetch })
  expect(await unconfigured.listActions()).toMatchObject({ kind: "refused", status: 503, code: "action.receipts_unconfigured" })
  for (const document of [
    { items: "no", definitionsVersion: 1 }, { items: [] }, [], { definitionsVersion: 1, items: [null] },
    { definitionsVersion: 1, items: [{ actionId: "a" }] }, { definitionsVersion: 1.5, items: [] },
    { definitionsVersion: 1, items: [{ ...summary, uiEffects: { destructive: false, irreversible: false } }] },
    { definitionsVersion: 1, items: [{ ...summary, effect: "teleport" }] },
  ]) {
    const garbled = createNornClient({ scope, fetch: cell(() => Response.json(document)).fetch })
    expect([document, await failure(garbled.listActions())]).toEqual([document, expect.any(NornUnexpectedResponse)])
  }
  // An objectId without an objectKind names nothing: it is not sent.
  const wire = cell(() => Response.json({ definitionsVersion: 1, items: [] }))
  await createNornClient({ scope, fetch: wire.fetch, headers: () => ({ authorization: "Bearer t" }) }).listActions({ objectId: "x" })
  expect(wire.sent[0]!.url).toBe("/v1/actions?orgId=org_a&environmentId=dev")
  expect(wire.sent[0]!.headers["authorization"]).toBe("Bearer t")
})
