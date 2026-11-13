#!/usr/bin/env bun
// Live reads on a served cell (UI-LIVE-1): subscribe to the definitions list over the cell's
// WebSocket, commit a dashboard over HTTP, and require the new list to arrive on the socket.
// An unauthenticated socket and a non-GET path are refused. Prints the commit-to-snapshot latency.
// Usage: bun live-check.mjs <base url> <repo root>   (run by smoke.py after the cell is up)

import { readFileSync } from "node:fs"
import { join } from "node:path"

const [base, root] = process.argv.slice(2)
const SCOPE = "orgId=org_demo&environmentId=analytics"
// The fixture principal that may write definitions: `admin@demo` where the cell declares one,
// else `viewer@demo` (which holds admin in the first fixture table). The cell decides; the check
// takes the first principal whose plain GET of the list the cell answers.
const CANDIDATES = ["Bearer fixture:admin@demo", "Bearer fixture:viewer@demo"]
const LIST = `/v1/definitions:list?kind=dashboard&${SCOPE}`

function question() {
  const q = JSON.parse(readFileSync(join(root, "fixtures/ir/metrics/sales-by-store-day.question.json"), "utf8"))
  delete q.compare
  delete q.filter
  return { ...q, name: "studio.tile.yield", objectSet: "demo.Conversion", aggregate: [{ metric: "demo.yield" }], groupBy: [{ path: { property: "occurred_at" }, grain: "week" }], window: { start: "2026-07-06T00:00:00Z", end: "2026-10-26T00:00:00Z" }, freshness: { kind: "live", maxAgeMs: 5000 } }
}

function open(authorization) {
  const url = `${base.replace(/^http/, "ws")}/v1/subscribe?${SCOPE}`
  const ws = new WebSocket(url, { headers: authorization ? { authorization } : {}, protocols: ["norn.live.v1"] })
  const inbox = []
  const waiters = []
  ws.addEventListener("message", (event) => {
    const msg = JSON.parse(String(event.data))
    const waiter = waiters.findIndex((w) => w.match(msg))
    if (waiter >= 0) waiters.splice(waiter, 1)[0].resolve(msg)
    else inbox.push(msg)
  })
  const next = (match, ms = 10_000) => {
    const queued = inbox.findIndex(match)
    if (queued >= 0) return Promise.resolve(inbox.splice(queued, 1)[0])
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("no live message in time")), ms)
      waiters.push({ match, resolve: (msg) => { clearTimeout(timer); resolve(msg) } })
    })
  }
  const opened = new Promise((resolve, reject) => {
    ws.addEventListener("open", () => resolve(ws))
    ws.addEventListener("error", () => reject(new Error("socket refused")))
    ws.addEventListener("close", (event) => reject(new Error(`socket closed ${event.code}`)))
  })
  return { ws, next, opened }
}

const assert = (ok, message) => { if (!ok) throw new Error(message) }

let ADMIN = null
let plainList = null
for (const candidate of CANDIDATES) {
  const response = await fetch(`${base}${LIST}`, { headers: { authorization: candidate } })
  if (response.status === 200) { ADMIN = candidate; plainList = await response.text(); break }
}
assert(ADMIN !== null, "no fixture principal may list definitions on this cell")

// Refusals: no token, no socket; a non-upgrade request to the live route is a 426.
const anonymous = open(undefined)
assert(await anonymous.opened.then(() => false, () => true), "an anonymous live socket opened")
const plain = await fetch(`${base}/v1/subscribe?${SCOPE}`, { headers: { authorization: ADMIN } })
assert(plain.status === 426, `plain GET on the live route answered ${plain.status}`)

const live = open(ADMIN)
await live.opened
live.ws.send(JSON.stringify({ type: "subscribe", id: "bad", path: "/v1/subscribe" }))
const refused = await live.next((m) => m.id === "bad")
assert(refused.type === "refused" && refused.code === "live.not_subscribable", JSON.stringify(refused))
live.ws.send(JSON.stringify({ type: "subscribe", id: "defs", path: LIST }))
const first = await live.next((m) => m.id === "defs")
assert(first.type === "snapshot" && first.status === 200, JSON.stringify(first).slice(0, 300))
// The socket says exactly what the GET says, for the same principal.
assert(JSON.stringify(first.body) === JSON.stringify(JSON.parse(plainList)), "the first live answer differs from the GET")

const id = `studio.dashboard.live${Date.now()}`
const started = performance.now()
const installed = await fetch(`${base}/v1/definitions:install?${SCOPE}`, {
  method: "POST",
  headers: { authorization: ADMIN, "content-type": "application/json" },
  body: JSON.stringify({ kind: "dashboard", id, payload: { title: "Live", tiles: [{ id: "t1", title: "Yield by week", question: question(), view: { kind: "chart", chart: "line" }, layout: { x: 0, y: 0, w: 6, h: 4 } }] } }),
})
assert(installed.ok, `install answered ${installed.status}: ${(await installed.text()).slice(0, 300)}`)
const second = await live.next((m) => m.id === "defs")
const latency = performance.now() - started
assert(second.digest !== first.digest && JSON.stringify(second.body).includes(id), "the new definition did not arrive on the socket")
live.ws.close(1000, "done")
console.log(JSON.stringify({ live: "PASS", installToSnapshotMs: Math.round(latency * 10) / 10 }))
