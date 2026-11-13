// The cell host's side of live reads (apps/norn-cell-do/subscribe.mjs, UI-LIVE-1), against a fake
// socket and a fake cell: the socket only ever carries what the cell's own GET answers, as the
// subscriber, now. When the subscriber loses access, the next answer is the cell's refusal and
// no data follows; when the token no longer verifies, the socket closes.

import { expect, test } from "bun:test"
// @ts-expect-error: the host module is plain JavaScript without declarations.
import * as live from "../../../apps/norn-cell-do/subscribe.mjs"

class FakeSocket {
  attachment: unknown
  readonly sent: Record<string, unknown>[] = []
  closed: number | null = null
  constructor(attachment: unknown) {
    this.attachment = structuredClone(attachment)
  }
  serializeAttachment(value: unknown) {
    this.attachment = structuredClone(value)
  }
  deserializeAttachment() {
    return structuredClone(this.attachment)
  }
  send(data: string) {
    this.sent.push(JSON.parse(data))
  }
  close(code: number) {
    this.closed = code
  }
}

type Reply = { status: number; body: string }

/** A cell whose answer to the subscriber the test changes between commits. */
function cell(initial: Reply) {
  let reply = initial
  const asked: { path: string; query: string; authorization: string }[] = []
  return {
    ask: (path: string, query: string, authorization: string) => {
      asked.push({ path, query, authorization })
      return reply
    },
    set: (next: Reply) => {
      reply = next
    },
    asked,
  }
}

const RECORDS = JSON.stringify({ items: [{ id: "dec_1", question: "Approve the refund?" }] })
const DENIED = JSON.stringify({ format: "norn/refusal@2", code: "authz.denied", category: "permission", message: "the read request is not authorized" })

test("a subscriber who loses access gets the cell's refusal on the next commit, and no data after it", async () => {
  const ws = new FakeSocket({ scope: ["org_demo", "dev"], authorization: "Bearer approver", subs: {} })
  const host = cell({ status: 200, body: RECORDS })
  await live.message(ws, JSON.stringify({ type: "subscribe", id: "inbox", path: "/v1/decisions?state=pending&orgId=org_demo&environmentId=dev" }), host.ask)
  expect(ws.sent).toEqual([{ type: "snapshot", id: "inbox", status: 200, digest: expect.any(String), body: JSON.parse(RECORDS) }])
  // The cell is asked as the subscriber, for exactly the GET they named.
  expect(host.asked.at(-1)).toEqual({ path: "/v1/decisions", query: "state=pending&orgId=org_demo&environmentId=dev", authorization: "Bearer approver" })

  // Access is revoked; the next commit re-asks, and the cell refuses.
  host.set({ status: 403, body: DENIED })
  await live.publish({ getWebSockets: () => [ws] }, host.ask)
  expect(ws.sent.at(-1)).toEqual({ type: "snapshot", id: "inbox", status: 403, digest: expect.any(String), body: JSON.parse(DENIED) })

  // Data changes behind the refusal: nothing is sent while the cell keeps refusing.
  const before = ws.sent.length
  await live.publish({ getWebSockets: () => [ws] }, host.ask)
  expect(ws.sent).toHaveLength(before)
  expect(ws.sent.filter((m) => m.status === 200)).toHaveLength(1)
})

test("a token the cell no longer verifies closes the socket with 4401 and sends nothing", async () => {
  const ws = new FakeSocket({ scope: ["org_demo", "dev"], authorization: "Bearer expired", subs: { inbox: { pathname: "/v1/decisions", query: "", digest: "x" } } })
  const host = cell({ status: 401, body: "" })
  expect(await live.evaluate(ws, host.ask)).toBe(false)
  expect(ws.closed).toBe(4401)
  expect(ws.sent).toEqual([])
})

test("only GET routes of the cell can be subscribed, at most a few per socket", async () => {
  expect(live.subscribable("/v1/subscribe")).toBeNull()
  expect(live.subscribable("https://elsewhere/v1/objects")).toBeNull()
  expect(live.subscribable("/v1/objects/demo.Order?limit=5")).toEqual({ pathname: "/v1/objects/demo.Order", query: "limit=5" })
  const ws = new FakeSocket({ scope: ["o", "e"], authorization: "Bearer a", subs: {} })
  const host = cell({ status: 200, body: "{}" })
  for (let n = 0; n <= live.MAX_SUBSCRIPTIONS; n += 1) await live.message(ws, JSON.stringify({ type: "subscribe", id: `s${n}`, path: `/v1/objects/t${n}` }), host.ask)
  expect(ws.sent.at(-1)).toMatchObject({ type: "refused", code: "live.too_many_subscriptions" })
})
