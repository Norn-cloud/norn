import { expect, test } from "bun:test"
import { CellUnavailable, NotConnected, cellDefinitions, readStudioScope, dashboardField, dashboardId, disconnected, duplicateDashboard, saveDashboard, type Dashboard } from "../src/lib/definitions.ts"
import { cellDouble, tile } from "./support/definitions.ts"

const testDefinitions = (fetcher: Parameters<typeof cellDefinitions>[0]) => cellDefinitions(fetcher, "/api/host", async () => ({ orgId: "org_demo", environmentId: "analytics" }))

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
  expect(await testDefinitions(answer(200, { admitted: false, diagnostics: [{ code: "q.metric", law: "metric.declared", message: "No metric demo.purity", span: { pointer: "/tiles/2/question/aggregate/0" } }] })).admit("studio.dashboard.refinery", dashboard))
    .toEqual({ admitted: false, findings: [{ code: "q.metric", law: "metric.declared", message: "No metric demo.purity", pointer: "/tiles/2/question/aggregate/0" }] })
  expect(calls.at(-1)).toEqual({ path: "/api/host/v1/definitions:admit?orgId=org_demo&environmentId=analytics", body: { kind: "dashboard", id: "studio.dashboard.refinery", payload: dashboard } })
  const receipt = { receiptHash: "abc", commitSeq: 7, recordedAtMs: 1 }
  expect(await testDefinitions(answer(200, { installed: true, id: "studio.dashboard.refinery", version: 3, receipt })).install("studio.dashboard.refinery", dashboard, 2)).toEqual({ installed: true, id: "studio.dashboard.refinery", version: 3, receipt })
  expect(calls.at(-1)?.body).toEqual({ kind: "dashboard", id: "studio.dashboard.refinery", payload: dashboard, expectedVersion: 2 })
  expect(await testDefinitions(answer(422, { format: "norn/refusal@2", code: "definition.refused", diagnostics: [{ code: "d.title", message: "Title", span: { pointer: "/title" } }] })).install("x", dashboard))
    .toEqual({ installed: false, findings: [{ code: "d.title", message: "Title", law: null, pointer: "/title" }] })
  expect(await testDefinitions(answer(409, { code: "definition.version_conflict" })).install("x", dashboard, 1)).toEqual({ installed: false, conflict: true })
  await testDefinitions(answer(200, { installed: true, id: "a/b", version: 2, receipt })).retire("a/b", 1)
  expect(calls.at(-1)).toEqual({ path: "/api/host/v1/definitions/a%2Fb:retire?orgId=org_demo&environmentId=analytics", body: { expectedVersion: 1 } })
  await expect(testDefinitions(answer(404, {})).admit("x", dashboard)).rejects.toBeInstanceOf(NotConnected)
  await expect(testDefinitions(answer(403, { code: "authz.denied" })).install("x", dashboard)).rejects.toThrow("Your role may not read or change these definitions.")
  await expect(testDefinitions(async () => { throw new TypeError("network") }).install("x", dashboard)).rejects.toBeInstanceOf(CellUnavailable)
  expect(new CellUnavailable(404).message).toBe("The cell's definition route isn't connected yet; nothing was read or saved.")
  expect(new CellUnavailable(502).message).toBe("The cell did not answer (502); nothing was read or saved.")
  const reads: string[] = []
  const reader = (status: number, body: unknown) => async (path: string) => { reads.push(path); return new Response(JSON.stringify(body), { status }) }
  expect(await testDefinitions(reader(200, { items: [{ id: "studio.dashboard.refinery", kind: "dashboard", title: "Refinery", version: 3, lifecycle: "active", updatedAtMs: 5 }], cursor: "c2" })).list())
    .toEqual({ items: [{ id: "studio.dashboard.refinery", title: "Refinery", version: 3, lifecycle: "active", updatedAtMs: 5 }], cursor: "c2" })
  await testDefinitions(reader(200, { items: [], cursor: null })).list("c2")
  expect(reads.slice(-2)).toEqual(["/api/host/v1/definitions:list?kind=dashboard&limit=50&orgId=org_demo&environmentId=analytics", "/api/host/v1/definitions:list?kind=dashboard&limit=50&cursor=c2&orgId=org_demo&environmentId=analytics"])
  expect(await testDefinitions(reader(200, { id: "studio.dashboard.refinery", kind: "dashboard", version: 3, lifecycle: "active", payload: dashboard, receipt })).get("studio.dashboard.refinery")).toEqual({ id: "studio.dashboard.refinery", version: 3, dashboard })
  expect(await testDefinitions(reader(404, { code: "definition.not_found" })).get("nope")).toBeUndefined()
  await expect(testDefinitions(reader(404, {})).list()).rejects.toThrow("no route to read them back")
  await expect(testDefinitions(reader(404, {})).get("x")).rejects.toThrow("no route to read them back")
})

test("until the cell's route is connected, nothing is saved or read, and it says so", async () => {
  await expect(saveDashboard(disconnected, "studio.dashboard.x", { title: "x", tiles: [] })).rejects.toBeInstanceOf(NotConnected)
  await expect(disconnected.list()).rejects.toThrow("the cell's definition route isn't connected yet.")
})

test("a cell finding lands on the title, one tile, or the dashboard as a whole", () => {
  expect(dashboardField("/payload/title")).toBe("title")
  expect(dashboardField("/payload/tiles/2/question/aggregate/0")).toBe("tile.2")
  expect(dashboardField("/tiles")).toBe("tiles")
  expect(dashboardField(null)).toBeNull()
})


test("every definition route uses the current server scope and refuses a workspace without a cell", async () => {
  const receipt = { receiptHash: "r", commitSeq: 1, recordedAtMs: 1 }
  let environmentId: string | null = "analytics"
  let orgId = "org_demo"
  const paths: string[] = []
  const fetcher = async (path: string) => {
    if (path === "/api/me") return Response.json({ organization: { id: orgId }, environmentId })
    paths.push(path)
    if (path.includes(":list")) return Response.json({ items: [], cursor: null })
    if (path.includes(":admit")) return Response.json({ admitted: true })
    if (path.endsWith("environmentId=analytics") && path.includes(":retire")) return Response.json({ installed: true, id: "x", version: 2, receipt })
    if (path.includes(":install")) return Response.json({ installed: true, id: "x", version: 1, receipt })
    return Response.json({ id: "x", kind: "dashboard", version: 1, payload: { title: "x", tiles: [] } })
  }
  const store = cellDefinitions(fetcher, "/api/host", () => readStudioScope(fetcher))
  await store.admit("x", { title: "x", tiles: [] })
  await store.install("x", { title: "x", tiles: [] })
  await store.retire("x", 1)
  await store.list()
  await store.get("x")
  expect(paths).toHaveLength(5)
  for (const path of paths) { const url = new URL(path, "https://studio.test"); expect(url.searchParams.get("orgId")).toBe("org_demo"); expect(url.searchParams.get("environmentId")).toBe("analytics") }
  orgId = "other_org"; environmentId = "other_env"
  await store.list()
  expect(paths.at(-1)).toContain("orgId=other_org&environmentId=other_env")
  environmentId = null
  await expect(store.list()).rejects.toThrow("This workspace has no data cell yet.")
  expect(paths).toHaveLength(6)
})
