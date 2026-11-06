import { expect, test } from "bun:test"
import { CellUnavailable, NotConnected, cellDefinitions, dashboardField, dashboardId, disconnected, duplicateDashboard, saveDashboard, type Dashboard } from "../src/lib/definitions.ts"
import { cellDouble, tile } from "./support/definitions.ts"

test("saving admits then installs under a stable id; a refusal comes back as findings on their fields; a newer version is a conflict", async () => {
  const store = cellDouble()
  const refused = await saveDashboard(store, dashboardId(" "), { title: " ", tiles: [] })
  expect(refused).toEqual({ findings: [{ code: "dashboard.title", message: "A dashboard needs a title.", pointer: "/title", law: null }] })
  expect(("findings" in refused ? refused.findings : []).map(({ pointer }) => dashboardField(pointer))).toEqual(["title"])
  const dashboard: Dashboard = { title: "Refinery weekly", tiles: [tile("a")] }
  const id = dashboardId(dashboard.title)
  expect(id).toBe("studio.dashboard.refinery_weekly")
  const first = await saveDashboard(store, id, dashboard)
  expect(first).toMatchObject({ saved: { id, version: 1, receipt: { receiptHash: `r-${id}-1` } } })
  expect(await saveDashboard(store, id, { ...dashboard, title: "Refinery, weekly" }, 1)).toMatchObject({ saved: { version: 2 } })
  expect((await store.get(id))?.dashboard.title).toBe("Refinery, weekly")
  expect(await saveDashboard(store, id, dashboard, 1)).toEqual({ conflict: true })
})

test("the cell's route: admit, install, retire as CELL-DO documents them; anything else saved nothing and says why", async () => {
  const calls: { path: string; body: unknown }[] = []
  const answer = (status: number, body: unknown) => async (path: string, init?: RequestInit) => {
    calls.push({ path, body: JSON.parse(String(init?.body)) })
    return new Response(JSON.stringify(body), { status })
  }
  const dashboard: Dashboard = { title: "Refinery", tiles: [] }
  expect(await cellDefinitions(answer(200, { admitted: false, diagnostics: [{ code: "q.metric", law: "metric.declared", message: "No metric demo.purity", span: { pointer: "/tiles/2/question/aggregate/0" } }] })).admit("studio.dashboard.refinery", dashboard))
    .toEqual({ admitted: false, findings: [{ code: "q.metric", law: "metric.declared", message: "No metric demo.purity", pointer: "/tiles/2/question/aggregate/0" }] })
  expect(calls.at(-1)).toEqual({ path: "/api/host/v1/definitions:admit", body: { kind: "dashboard", id: "studio.dashboard.refinery", payload: dashboard } })
  const receipt = { receiptHash: "abc", commitSeq: 7, recordedAtMs: 1 }
  expect(await cellDefinitions(answer(200, { installed: true, id: "studio.dashboard.refinery", version: 3, receipt })).install("studio.dashboard.refinery", dashboard, 2)).toEqual({ installed: true, id: "studio.dashboard.refinery", version: 3, receipt })
  expect(calls.at(-1)?.body).toEqual({ kind: "dashboard", id: "studio.dashboard.refinery", payload: dashboard, expectedVersion: 2 })
  expect(await cellDefinitions(answer(422, { format: "norn/refusal@2", code: "definition.refused", diagnostics: [{ code: "d.title", message: "Title", span: { pointer: "/title" } }] })).install("x", dashboard))
    .toEqual({ installed: false, findings: [{ code: "d.title", message: "Title", law: null, pointer: "/title" }] })
  expect(await cellDefinitions(answer(409, { code: "definition.version_conflict" })).install("x", dashboard, 1)).toEqual({ installed: false, conflict: true })
  await cellDefinitions(answer(200, { installed: true, id: "a/b", version: 2, receipt })).retire("a/b", 1)
  expect(calls.at(-1)).toEqual({ path: "/api/host/v1/definitions/a%2Fb:retire", body: { expectedVersion: 1 } })
  await expect(cellDefinitions(answer(404, {})).admit("x", dashboard)).rejects.toBeInstanceOf(NotConnected)
  await expect(cellDefinitions(answer(403, { code: "authz.denied" })).install("x", dashboard)).rejects.toThrow("Your role may not read or change these definitions.")
  await expect(cellDefinitions(async () => { throw new TypeError("network") }).install("x", dashboard)).rejects.toBeInstanceOf(CellUnavailable)
  await expect(cellDefinitions(answer(404, {})).list()).rejects.toThrow("no route to read them back")
  const reads: string[] = []
  const reader = (status: number, body: unknown) => async (path: string) => { reads.push(path); return new Response(JSON.stringify(body), { status }) }
  expect(await cellDefinitions(reader(200, { items: [{ id: "studio.dashboard.refinery", kind: "dashboard", title: "Refinery", version: 3, lifecycle: "active", updatedAtMs: 5 }], cursor: "c2" })).list())
    .toEqual({ items: [{ id: "studio.dashboard.refinery", title: "Refinery", version: 3, lifecycle: "active", updatedAtMs: 5 }], cursor: "c2" })
  await cellDefinitions(reader(200, { items: [], cursor: null })).list("c2")
  expect(reads.slice(-2)).toEqual(["/api/host/v1/definitions?kind=dashboard", "/api/host/v1/definitions?kind=dashboard&cursor=c2"])
  expect(await cellDefinitions(reader(200, { id: "studio.dashboard.refinery", kind: "dashboard", version: 3, lifecycle: "active", payload: dashboard, receipt })).get("studio.dashboard.refinery")).toEqual({ id: "studio.dashboard.refinery", version: 3, dashboard })
  expect(await cellDefinitions(reader(404, { code: "definition.not_found" })).get("nope")).toBeUndefined()
  await expect(cellDefinitions(reader(404, {})).get("x")).rejects.toThrow("no route to read them back")
})

test("until the cell's route is connected, nothing is saved or read, and it says so", async () => {
  await expect(saveDashboard(disconnected, "studio.dashboard.x", { title: "x", tiles: [] })).rejects.toBeInstanceOf(NotConnected)
  await expect(disconnected.list()).rejects.toThrow("Not saved: the cell's definition route isn't connected yet.")
})

test("a cell finding lands on the title, one tile, or the dashboard as a whole", () => {
  expect(dashboardField("/payload/title")).toBe("title")
  expect(dashboardField("/payload/tiles/2/question/aggregate/0")).toBe("tile.2")
  expect(dashboardField("/tiles")).toBe("tiles")
  expect(dashboardField(null)).toBeNull()
})

const render = (stored?: StoredDashboard) => renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><DashboardEditor store={cellDouble()} catalog={catalog} stored={stored} /></QueryClientProvider>)
