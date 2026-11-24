import { expect, test } from "bun:test"
import {
  bulkRecoveryKey,
  encodeBulkSharedInputs,
  matchesBulkRecovery,
  retryBulkTarget,
  runBulkTargets,
  unknownRecoveryBanner,
  type BulkRecoveryState,
} from "../src/actions/bulk-action-sheet.tsx"
import { callAction, type ActionSummary, type FormField } from "../src/lib/actions.ts"
import type { Fetch } from "../src/lib/cell-read.ts"
import type { StudioScope } from "../src/lib/definitions.ts"
import { offeredFor } from "../src/lib/screen/call.ts"
import { cellViews, cleanView, duplicateView, newViewId, sortKeys, sortText, viewField, type SavedView } from "../src/lib/views.ts"

const ACTION: ActionSummary = {
  actionId: "demo.record.act",
  title: "Act on record",
  description: null,
  objectKind: "demo.Record",
  effect: "fire_transition",
  objectIdField: "id",
  inputSchema: { type: "object", required: ["id"], properties: { id: { type: "string" } } },
  uiEffects: { irreversible: false, destructive: false, money: [] },
}

test("views helpers normalize payloads, preserve space and offered actions, duplicate and map pointers", () => {
  const draft: SavedView = {
    title: "  Items needing review  ",
    description: "  ",
    space: "  Operations  ",
    objectType: "demo.Record",
    filter: { all: [{ property: "score", op: "lt", value: "60" }] },
    sort: sortText([{ property: "score", direction: "asc" }]),
    columns: ["stage", "score"],
    rowActions: ["demo.record.act"],
    bulkActions: ["demo.record.act"],
  }
  const cleaned = cleanView(draft)
  expect(cleaned.title).toBe("Items needing review")
  expect(cleaned.description).toBeUndefined()
  expect(cleaned.space).toBe("Operations")
  expect(cleaned.rowActions).toEqual(["demo.record.act"])
  expect(cleaned.bulkActions).toEqual(["demo.record.act"])
  expect(sortKeys(cleaned.sort)).toEqual([{ property: "score", direction: "asc" }])

  const copy = duplicateView(cleaned)
  expect(copy.title).toBe("Items needing review (copy)")
  expect(copy.space).toBe("Operations")
  expect(copy.rowActions).toEqual(["demo.record.act"])

  expect(newViewId("demo.Record", "Needs help!", () => "abcd1234")).toBe("studio.view.demo_record.needs_help_abcd1234")
  expect(viewField("/payload/rowActions/0")).toBe("rowActions")
  expect(viewField("/space")).toBe("space")
  expect(viewField("/filter/all/0/value")).toBe("filter")
  expect(viewField("/unknown")).toBeNull()
})

test("offeredFor fails closed when row offers are missing, empty, or malformed", () => {
  const catalog = [ACTION]
  expect(offeredFor(catalog, { id: "r1" })).toEqual([])
  expect(offeredFor(catalog, { id: "r1", offers: null })).toEqual([])
  expect(offeredFor(catalog, { id: "r1", offers: "demo.record.act" })).toEqual([])
  expect(offeredFor(catalog, { id: "r1", offers: [] })).toEqual([])
  expect(offeredFor(catalog, { id: "r1", offers: ["demo.record.act"] })).toEqual([ACTION])
})

test("bulk shared inputs reject blank required numbers, allow explicit 0, and encode declared scalar types", () => {
  const formFields: readonly FormField[] = [
    { name: "count", label: "Count", description: null, pointer: "/count", required: true, nullable: false, spec: { kind: "integer" } },
    { name: "note", label: "Note", description: null, pointer: "/note", required: false, nullable: false, spec: { kind: "text" } },
    { name: "urgent", label: "Urgent", description: null, pointer: "/urgent", required: false, nullable: false, spec: { kind: "boolean" } },
  ]
  expect(encodeBulkSharedInputs(formFields, { count: "", note: "", urgent: "false" }).ok).toBe(false)
  expect(encodeBulkSharedInputs(formFields, { count: "0", note: "", urgent: "true" })).toEqual({ ok: true, shared: { count: 0, urgent: true } })
  expect(encodeBulkSharedInputs(formFields, { count: "2.5", note: "", urgent: "false" }).ok).toBe(false)
})

test("bulk batch stops immediately when active scope changes between targets and before retry", async () => {
  let activeScope: StudioScope = { orgId: "org_a", environmentId: "prod" }
  const sent: { readonly id: string; readonly key: string; readonly scope: StudioScope }[] = []
  let seq = 0

  const first = await runBulkTargets({
    actionId: ACTION.actionId,
    objectIdField: "id",
    targets: [
      { id: "r1", name: "Record 1" },
      { id: "r2", name: "Record 2" },
    ],
    shared: { note: "hello" },
    scope: activeScope,
    readScope: async () => activeScope,
    newKey: () => `k-${++seq}`,
    dispatch: async (_actionId, payloadJson, idempotencyKey, scope) => {
      const parsed = JSON.parse(payloadJson) as { id: string }
      sent.push({ id: parsed.id, key: idempotencyKey, scope })
      // Switch active environment after target 1 completes.
      activeScope = { orgId: "org_a", environmentId: "staging" }
      throw new Error("connection lost")
    },
  })

  expect(first.scopeChanged).toBe(true)
  expect(sent).toHaveLength(1)
  expect(sent[0]).toEqual({ id: "r1", key: "k-1", scope: { orgId: "org_a", environmentId: "prod" } })
  expect(first.outcomes).toHaveLength(1)
  expect(first.outcomes[0]!.status).toBe("unknown")

  // Retrying while activeScope is still "staging" must refuse without dispatching.
  const blockedRetry = await retryBulkTarget({
    actionId: ACTION.actionId,
    outcome: first.outcomes[0]!,
    readScope: async () => activeScope,
    dispatch: async () => { throw new Error("must not dispatch under changed scope") },
  })
  expect(blockedRetry.scopeChanged).toBe(true)
  expect(blockedRetry.outcome.status).toBe("unknown")

  // Returning to the original scope replays with the exact original payload, key and scope.
  activeScope = { orgId: "org_a", environmentId: "prod" }
  const retriedCalls: { readonly payloadJson: string; readonly key: string; readonly scope: StudioScope }[] = []
  const okRetry = await retryBulkTarget({
    actionId: ACTION.actionId,
    outcome: first.outcomes[0]!,
    readScope: async () => activeScope,
    dispatch: async (_actionId, payloadJson, idempotencyKey, scope) => {
      retriedCalls.push({ payloadJson, key: idempotencyKey, scope })
      return {
        kind: "receipt",
        receipt: {
          outcome: "replayed",
          objectType: "demo.Record",
          objectId: "r1",
          version: 2,
          receipt: { receiptHash: "h1", commitSeq: 11, recordedAtMs: 1000 },
        },
      }
    },
  })
  expect(okRetry.scopeChanged).toBe(false)
  expect(okRetry.outcome.status).toBe("committed")
  expect(okRetry.outcome.detail).toContain("replayed commit #11")
  expect(retriedCalls).toEqual([{
    payloadJson: '{"note":"hello","id":"r1"}',
    key: "k-1",
    scope: { orgId: "org_a", environmentId: "prod" },
  }])
})

test("bulk recovery matching preserves unknown outcomes across close/reopen regardless of target order", () => {
  const scope: StudioScope = { orgId: "org_a", environmentId: "prod" }
  const targets = [{ id: "r2", name: "Two" }, { id: "r1", name: "One" }]
  const recovery: BulkRecoveryState = {
    key: bulkRecoveryKey(scope, ACTION.actionId, targets),
    action: ACTION,
    targets,
    scope,
    outcomes: [{
      target: targets[0]!,
      status: "unknown",
      detail: "Connection lost",
      idempotencyKey: "k-1",
      payloadJson: '{"id":"r2"}',
      scope,
    }],
  }
  expect(matchesBulkRecovery(recovery, { action: ACTION, targets: [{ id: "r1", name: "One" }, { id: "r2", name: "Two" }] })).toBe(true)
  expect(matchesBulkRecovery(recovery, { action: ACTION, targets: [{ id: "r1", name: "One" }] })).toBe(false)
  expect(unknownRecoveryBanner(1)).toBe("1 result unknown — retry with the same keys")
  expect(unknownRecoveryBanner(3)).toBe("3 results unknown — retry with the same keys")
})

test("callAction honors pinnedScope without rereading /api/me", async () => {
  const urls: string[] = []
  const fetcher: Fetch = async (input) => {
    urls.push(String(input))
    return new Response(JSON.stringify({
      outcome: "committed",
      objectType: "demo.Record",
      objectId: "r1",
      version: 2,
      receipt: { receiptHash: "h1", commitSeq: 5, recordedAtMs: 100 },
    }), { status: 200 })
  }
  const result = await callAction("demo.record.act", '{"id":"r1"}', "key-1", fetcher, { orgId: "org_pin", environmentId: "prod" })
  expect(result.kind).toBe("receipt")
  expect(urls).toEqual(["/api/host/v1/actions/demo.record.act?orgId=org_pin&environmentId=prod"])
})

test("views list operations pin one scope and surface non-404 read failures", async () => {
  let scopeOrg = "org_a"
  const seenScopes: string[] = []
  const fetcher: Fetch = async (input) => {
    const url = String(input)
    if (url === "/api/me") {
      return new Response(JSON.stringify({ organization: { id: scopeOrg }, environmentId: "dev" }), { status: 200 })
    }
    const parsed = new URL(url, "https://studio.test")
    seenScopes.push(parsed.searchParams.get("orgId") ?? "")
    if (parsed.pathname.endsWith("/v1/definitions:list")) {
      scopeOrg = "org_b"
      return new Response(JSON.stringify({ items: [{ id: "studio.view.a", updatedAtMs: 1 }], cursor: null }), { status: 200 })
    }
    if (parsed.pathname.endsWith("/v1/definitions/studio.view.a")) {
      return new Response(JSON.stringify({ id: "studio.view.a", version: 1, payload: { title: "A", objectType: "demo.Record" } }), { status: 200 })
    }
    return new Response(JSON.stringify({ code: "host.unavailable", message: "down" }), { status: 503 })
  }
  const views = await cellViews(fetcher).listAll()
  expect(views.length).toBe(1)
  expect(seenScopes).toEqual(["org_a", "org_a"])

  // A 503 on per-id definition GET throws instead of returning an empty list.
  const failingFetch: Fetch = async (input) => {
    const url = String(input)
    if (url === "/api/me") return new Response(JSON.stringify({ organization: { id: "org_a" }, environmentId: "dev" }), { status: 200 })
    if (url.includes("/v1/definitions:list")) return new Response(JSON.stringify({ items: [{ id: "studio.view.a" }], cursor: null }), { status: 200 })
    return new Response(JSON.stringify({ code: "host.error", message: "service unavailable" }), { status: 503 })
  }
  await expect(cellViews(failingFetch).listAll()).rejects.toThrow()
})
