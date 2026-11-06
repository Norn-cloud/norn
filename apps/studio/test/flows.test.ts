import { expect, test } from "bun:test"
import { FlowCellError, FlowRoutesMissing, flowStore, newRunId, nodeOfFinding, type FlowPayload } from "../src/lib/canvas/flows.ts"

const scope = { orgId: "org_demo", environmentId: "analytics" }
const payload: FlowPayload = { flow: { id: "studio.automation", version: "0.1.0" }, artifacts: [{ kind: "entity" }], layout: { nodes: [], edges: [] } }
const calls: { path: string; body: unknown }[] = []
const answering = (status: number, body: unknown) => flowStore(scope, async (input, init) => {
  calls.push({ path: input, body: init?.body ? JSON.parse(String(init.body)) : undefined })
  return Response.json(body, { status })
})

test("admit and install send the flow kind, the drawing as layout, and the version opened; never a sign-off", async () => {
  expect(await answering(200, { admitted: true, diagnostics: [] }).admit("studio.automation", payload)).toEqual({ admitted: true })
  expect(calls.at(-1)).toEqual({ path: "/api/host/v1/definitions:admit?orgId=org_demo&environmentId=analytics", body: { kind: "flow", id: "studio.automation", payload } })
  expect(await answering(200, { installed: true, id: "studio.automation", version: 3, receipt: { receiptHash: "abcdef0123456789" } }).install("studio.automation", payload, 2)).toEqual({ installed: true, version: 3, receipt: "abcdef0123456789" })
  expect(calls.at(-1)?.body).toEqual({ kind: "flow", id: "studio.automation", payload, expectedVersion: 2 })
  expect(JSON.stringify(calls)).not.toContain("signoff")
})

test("the cell's answers: refused with findings, a newer version, live runs on retire, routes not there yet", async () => {
  const refusal = { code: "definition.refused", diagnostics: [{ code: "definition.flow_function_unknown", message: "step remind calls billing.notify@1, which is not installed", span: { pointer: "/artifacts/1/payload" } }] }
  expect(await answering(422, refusal).install("studio.automation", payload)).toEqual({ installed: false, findings: [{ code: "definition.flow_function_unknown", message: "step remind calls billing.notify@1, which is not installed", pointer: "/artifacts/1/payload" }] })
  expect(await answering(409, { code: "definition.version_conflict" }).install("studio.automation", payload, 1)).toEqual({ installed: false, conflict: true })
  expect(await answering(409, { code: "definition.flow_runs_live" }).retire("studio.automation", 2)).toEqual({ installed: false, runsLive: true })
  expect(calls.at(-1)).toEqual({ path: "/api/host/v1/definitions/studio.automation:retire?orgId=org_demo&environmentId=analytics", body: { expectedVersion: 2 } })
  await expect(answering(404, {}).install("studio.automation", payload)).rejects.toBeInstanceOf(FlowRoutesMissing)
  await expect(answering(403, { code: "authz.denied" }).admit("studio.automation", payload)).rejects.toBeInstanceOf(FlowCellError)
})

test("reopen reads the flow and its drawing; a missing one is undefined", async () => {
  expect(await answering(200, { id: "studio.automation", kind: "flow", version: 2, lifecycle: "active", payload }).get("studio.automation")).toEqual({ id: "studio.automation", version: 2, lifecycle: "active", payload })
  expect(await answering(404, { code: "definition.not_found" }).get("studio.automation")).toBeUndefined()
})

test("a run starts the installed flow by id with a fresh run id; an unknown flow says to save first", async () => {
  expect(await answering(200, { id: "run_1", flow: { flowId: "studio.automation" }, state: "running", generation: 1 }).start("studio.automation", "run_1")).toEqual({ id: "run_1", state: "running", flow: { flowId: "studio.automation" } })
  expect(calls.at(-1)).toEqual({ path: "/api/host/v1/runs?orgId=org_demo&environmentId=analytics", body: { flow: "studio.automation", runId: "run_1" } })
  await expect(answering(404, { code: "run.flow_not_found" }).start("studio.automation", "run_2")).rejects.toThrow("save it first")
  expect(newRunId(0, () => 0)).toBe("run_0_000000")
  expect(newRunId(Date.now()).length).toBeLessThanOrEqual(128)
})

test("a refusal lands on the step it names, or on a step by a state it lowered to, else on the automation", () => {
  const states = new Map([["remind", ["awaiting_perform_remind"]], ["load", ["awaiting_perform_load"]]])
  expect(nodeOfFinding({ code: "c", message: "step remind calls an unknown function", pointer: null }, ["load", "remind"], states)).toBe("remind")
  expect(nodeOfFinding({ code: "c", message: "state awaiting_perform_load has no way out", pointer: null }, ["start"], states)).toBe("load")
  expect(nodeOfFinding({ code: "c", message: "the flow id does not match", pointer: "/flow/id" }, ["load", "remind"], states)).toBeUndefined()
})
