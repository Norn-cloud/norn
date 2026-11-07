import { expect, test } from "bun:test"
import { createHash } from "node:crypto"
import { FlowCellError, FlowRoutesMissing, automationId, flowStore, newRunId, nodeOfFinding, type FlowPayload } from "../src/lib/canvas/flows.ts"

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
  expect(await answering(200, { id: "studio.automation", kind: "flow", version: 2, lifecycle: "active", payload, signoff: null, restatement: null, restatementSha256: null }).get("studio.automation")).toEqual({ id: "studio.automation", version: 2, lifecycle: "active", payload, signoff: null, restatement: null })
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

test("an automation's id is minted from its title, a valid flow id, and unique against the ids taken", () => {
  expect(automationId("Payment reminders")).toBe("studio.automation.payment_reminders")
  expect(automationId("Payment reminders", ["studio.automation.payment_reminders"])).toBe("studio.automation.payment_reminders_2")
  expect(automationId("2nd pass!")).toBe("studio.automation.a_2nd_pass")
  expect(automationId("  ")).toBe("studio.automation.untitled")
  for (const id of [automationId("Payment reminders"), automationId("2nd pass!")]) expect(id).toMatch(/^[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)+$/)
})

const sha = (text: string) => createHash("sha256").update(text).digest("hex")
test("the cell's words come with its check, only when they hash as it says; a signed install sends their hash", async () => {
  const text = "Invoice follow-up, version 0.1.0: starts when a run starts; 1. Load calls billing.load_invoice@1"
  expect(await answering(200, { admitted: true, diagnostics: [], restatement: text, restatementSha256: sha(text) }).admit("studio.automation", payload)).toEqual({ admitted: true, restatement: { text, sha256: sha(text) } })
  expect(await answering(200, { admitted: true, diagnostics: [], restatement: text, restatementSha256: "e".repeat(64) }).admit("studio.automation", payload)).toEqual({ admitted: true })
  await answering(200, { installed: true, id: "x", version: 1, receipt: {} }).install("studio.automation", payload, undefined, sha(text))
  expect(calls.at(-1)?.body).toMatchObject({ signoff: { restatementSha256: sha(text) } })
})

test("a mismatch brings the cell's own words; reopening reads who signed and the words now", async () => {
  const theirs = "Invoice follow-up, as the cell reads it now"
  expect(await answering(422, { code: "definition.signoff_mismatch", restatement: theirs, restatementSha256: sha(theirs) }).install("studio.automation", payload, 1, "a".repeat(64))).toEqual({ installed: false, signoff: "mismatch", restatement: { text: theirs, sha256: sha(theirs) } })
  expect(await answering(422, { code: "definition.signoff_unavailable" }).install("studio.automation", payload, 1, "a".repeat(64))).toEqual({ installed: false, signoff: "unavailable" })
  const stored = await answering(200, { id: "studio.automation", kind: "flow", version: 2, lifecycle: "active", payload, signoff: { signer: "u_e2e", signedAtMs: 5, restatementSha256: sha(theirs) }, restatement: theirs, restatementSha256: sha(theirs) }).get("studio.automation")
  expect(stored?.signoff).toEqual({ signer: "u_e2e", signedAtMs: 5, restatementSha256: sha(theirs) })
  expect(stored?.restatement).toEqual({ text: theirs, sha256: sha(theirs) })
})

test("the saved automations list reads kind flow", async () => {
  expect(await answering(200, { items: [{ id: "studio.automation.a", title: "A", version: 2, lifecycle: "active" }], cursor: null }).list()).toEqual({ items: [{ id: "studio.automation.a", title: "A", version: 2, lifecycle: "active" }], cursor: null })
  expect(calls.at(-1)?.path).toBe("/api/host/v1/definitions:list?orgId=org_demo&environmentId=analytics&kind=flow&limit=50")
})
