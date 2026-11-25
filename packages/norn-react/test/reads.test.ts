// How the renderer reads its answers (#1762 milestone C): every machine outcome of a read or a call
// becomes exactly one closed state, the server's document carried as sent.
import { expect, test } from "bun:test"
import { bindingRead, callStateOf, readStateOf } from "../src/reads.ts"
import type { DataBinding } from "../src/ux.ts"

const sent = { input: "{}", key: "k1" }
const refusal = { format: "norn/refusal@2", code: "authz.denied", category: "permission", message: "No." }

test("each read outcome is one read state; a lapsed or refused live feed keeps the snapshot, marked", () => {
  expect(readStateOf({ outcome: "accepted", payload: {}, body: { a: 1 } }, undefined, false)).toEqual({ kind: "ok", document: { a: 1 }, freshness: "snapshot" })
  expect(readStateOf({ outcome: "accepted", payload: {}, body: { a: 1 } }, undefined, true)).toEqual({ kind: "ok", document: { a: 1 }, freshness: "live" })
  expect(readStateOf({ outcome: "stale", payload: {}, body: { a: 1 } }, undefined, true)).toEqual({ kind: "ok", document: { a: 1 }, freshness: "stale" })
  expect(readStateOf({ outcome: "subscription_refused", payload: {}, body: { a: 1 } }, undefined, true)).toEqual({ kind: "ok", document: { a: 1 }, freshness: "snapshot" })
  expect(readStateOf({ outcome: "refused", category: "permission", payload: { code: "authz.denied" }, body: refusal }, undefined, false)).toEqual({ kind: "refused", refusal: { code: "authz.denied", category: "permission", document: refusal } })
  expect(readStateOf({ outcome: "not_served", payload: {} }, undefined, false).kind).toBe("not_served")
  expect(readStateOf({ outcome: "network_failure", payload: {} }, undefined, false)).toMatchObject({ kind: "unavailable", retry: true })
  expect(readStateOf({ outcome: "answer_moved", payload: {} }, undefined, false).kind).toBe("answer_moved")
  expect(readStateOf({ outcome: "timeout", payload: {} }, undefined, false).kind).toBe("timed_out")
  // A cancelled read leaves what was shown before it.
  const before = { kind: "ok", document: 1, freshness: "snapshot" } as const
  expect(readStateOf({ outcome: "cancelled", payload: {} }, before, false)).toBe(before)
})

test("each call outcome is one call state; a lost answer may have applied, and its retry keeps the key", () => {
  expect(callStateOf({ outcome: "accepted", payload: {}, body: { receipt: 1 } }, sent)).toEqual({ kind: "committed", receipt: { receipt: 1 }, undo: null })
  expect(callStateOf({ outcome: "pending_approval", payload: {}, body: { decision: { id: "apv_1" } } }, sent)).toEqual({ kind: "pending_approval", decision: { id: "apv_1" } })
  expect(callStateOf({ outcome: "refused", category: "permission", payload: {}, body: refusal }, sent).kind).toBe("refused")
  expect(callStateOf({ outcome: "not_served", payload: {} }, sent)).toEqual({ kind: "not_served" })
  for (const outcome of ["timeout", "cancelled", "network_failure"] as const) {
    expect(callStateOf({ outcome, payload: {} }, sent)).toMatchObject({ kind: "timed_out", sent })
  }
})

test("a binding's read is said only once the page can say it", () => {
  const list: DataBinding = { bindingId: "b", object: { id: "x.entity", version: "1" }, query: { kind: "object_list", filters: [], sort: [], page: { size: 20, maxSize: 50 } } }
  const detail: DataBinding = { bindingId: "d", object: { id: "x.entity", version: "1" }, query: { kind: "object_runtime_detail", pageId: "p", parameterId: "id" } }
  const types = (id: string) => (id === "x.entity" ? "x.Thing" : undefined)
  expect(bindingRead(list, {}, types)).toEqual({ kind: "list", objectType: "x.Thing", size: 20 })
  expect(bindingRead(detail, { id: "t-1" }, types)).toEqual({ kind: "object", objectType: "x.Thing", id: "t-1" })
  expect(bindingRead(detail, {}, types)).toBeNull()
  expect(bindingRead(list, {}, () => undefined)).toBeNull()
})
