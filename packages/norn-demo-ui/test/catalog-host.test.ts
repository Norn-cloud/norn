// The catalog host's enforcement (UI-CATALOG-1): a component can only say what its contract
// declares, with the declared types, when its machine's current state takes it. Everything else is
// refused, reported, and never reaches the page.

import { expect, test } from "bun:test"
import { type CatalogEntry, type Violation, checkPayload, createHost, frameDocument } from "../src/catalog-host/host.ts"
import type { Evaluate, Fields } from "../src/machine/index.ts"

// The entry machine below has no guards or assignments, so no expression is ever evaluated.
const evaluate: Evaluate = () => {
  throw new Error("no expression in this machine")
}

const tile: CatalogEntry = {
  entryId: "kpi-tile",
  trust: "contract_checked",
  framework: "foldkit",
  props: [{ fieldId: "title", type: "text", required: true }],
  events: [{ eventId: "drill", payload: [{ fieldId: "period", type: "text", required: true }] }],
  machine: {
    machineVersion: "0.1",
    initialStateId: "collapsed",
    fields: [],
    events: [
      { eventId: "expand", source: { kind: "user", nodeId: "self" } },
      { eventId: "drill", source: { kind: "user", nodeId: "self" }, payload: [{ fieldId: "period", type: "text", required: true }] },
      { eventId: "collapse", source: { kind: "user", nodeId: "self" } },
    ],
    states: [{ stateId: "collapsed", role: "idle" }, { stateId: "expanded", role: "idle" }],
    transitions: [
      { transitionId: "expand", from: "collapsed", on: "expand", target: "expanded" },
      { transitionId: "drill", from: "expanded", on: "drill", target: "expanded" },
      { transitionId: "collapse", from: "expanded", on: "collapse", target: "collapsed" },
    ],
  },
}

function harness(entry: CatalogEntry) {
  const emitted: [string, Fields][] = []
  const refused: Violation[] = []
  const views: string[] = []
  const host = createHost({ entry, evaluate, onEmit: (id, payload) => emitted.push([id, payload]), onViolation: (v) => refused.push(v), onView: (view) => views.push(view.state ?? "-") })
  return { host, emitted, refused, views }
}

test("an emit reaches the page only when declared, well typed, and offered by the current state", () => {
  const { host, emitted, refused, views } = harness(tile)
  host.receive({ type: "emit", eventId: "drill", payload: { period: "2026-W45" } })
  expect(refused).toEqual([{ kind: "not_offered", eventId: "drill", state: "collapsed" }])
  host.receive({ type: "emit", eventId: "expand" })
  host.receive({ type: "emit", eventId: "drill", payload: { period: "2026-W45" } })
  expect(emitted).toEqual([["drill", { period: "2026-W45" }]])
  expect(views).toEqual(["expanded", "expanded"])
  host.receive({ type: "emit", eventId: "drill", payload: { period: 45 } })
  host.receive({ type: "emit", eventId: "drill", payload: { period: "W45", sql: "drop" } })
  host.receive({ type: "emit", eventId: "delete-everything" })
  host.receive({ type: "fetch", url: "https://elsewhere" })
  expect(refused.slice(1).map((v) => v.kind)).toEqual(["ill_typed_payload", "ill_typed_payload", "undeclared_event", "malformed_message"])
  expect(emitted).toHaveLength(1)
})

test("an island has no machine: its declared, well typed events pass; nothing else does", () => {
  const island: CatalogEntry = { entryId: "yield-map", trust: "opaque_island", events: [{ eventId: "point-selected", payload: [{ fieldId: "siteId", type: "text", required: true }] }] }
  const { host, emitted, refused } = harness(island)
  host.receive({ type: "emit", eventId: "point-selected", payload: { siteId: "site-7" } })
  host.receive({ type: "emit", eventId: "camera-moved", payload: { zoom: 3 } })
  expect(emitted).toEqual([["point-selected", { siteId: "site-7" }]])
  expect(refused).toEqual([{ kind: "undeclared_event", eventId: "camera-moved" }])
})

test("payload types follow the declaration exactly", () => {
  const fields = [
    { fieldId: "at", type: "timestamp", required: true },
    { fieldId: "day", type: "date", required: false },
    { fieldId: "n", type: "integer", required: true },
  ] as const
  expect(checkPayload(fields, { at: "2026-11-13T09:00:00Z", n: 3 })).toBeNull()
  expect(checkPayload(fields, { at: "2026-11-13T09:00:00Z", n: 3, day: null })).toBeNull()
  expect(checkPayload(fields, { at: "yesterday", n: 3 })).toContain("at")
  expect(checkPayload(fields, { at: "2026-11-13T09:00:00Z", n: 3.5 })).toContain("n")
  expect(checkPayload(fields, [])).toBe("the payload is not a record")
})

test("the frame allows no network and no same-origin access; the module cannot close its script early", () => {
  const html = frameDocument('import x from "y"; document.body.textContent = "</script><img src=x>"')
  expect(html).toContain("default-src 'none'")
  expect(html).not.toContain("connect-src")
  expect(html.match(/<\/script>/g)?.length).toBe(2)
})
