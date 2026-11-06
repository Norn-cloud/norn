import { expect, test } from "bun:test"
import { existsSync, readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import { place, type CheckResult, type Finding } from "../src/lib/canvas/diagnostics.ts"
import * as g from "../src/lib/canvas/graph.ts"
import { compile, decompile, lifecycleKinds, lifecyclesIn } from "../src/lib/canvas/lifecycle.ts"
import type { Envelope } from "../src/lib/transport.ts"

const directory = join(import.meta.dir, "../../../registry/core/artifacts")
const definitions = readdirSync(directory).map((name) => JSON.parse(readFileSync(join(directory, name), "utf8")) as Envelope)
const refs = lifecyclesIn(definitions)
const envelopeOf = (artifactId: string) => definitions.find((envelope) => envelope.artifactId === artifactId) as Envelope
const order = refs.find(({ objectType }) => objectType === "commerce.SalesOrder")!
const orderEnvelope = envelopeOf(order.artifactId)

test("every core lifecycle compiles back to the artifact it was drawn from", () => {
  expect(refs.map(({ objectType, property }) => `${objectType}.${property}`).sort()).toEqual(["commerce.SalesOrder.status", "core.analytics.Restatement.status"])
  for (const ref of refs) {
    const envelope = envelopeOf(ref.artifactId)
    expect(compile(envelope, ref, decompile(envelope, ref)).envelope).toEqual(envelope)
  }
})

test("a transition from several states is one step drawn as several edges", () => {
  const graph = decompile(orderEnvelope, order)
  expect(graph.edges.filter(({ config }) => config.transition === "cancel").map(({ source }) => source)).toEqual(["draft", "submitted", "on_hold", "approved"])
})

test("ports refuse a state of another lifecycle, and an unknown port", () => {
  const kinds = { ...lifecycleKinds(order), other: () => [{ id: "in", type: "state:core.analytics.Restatement.status", side: "in" as const }] }
  const graph = g.addNode(decompile(orderEnvelope, order), { id: "elsewhere", kind: "other", position: { x: 0, y: 0 }, config: {} })
  const edge = { id: "x:draft", source: "draft", sourcePort: "out", target: "elsewhere", targetPort: "in", config: {} }
  expect(g.connect(graph, kinds, edge)).toMatchObject({ code: "canvas.port.type_mismatch" })
  expect(g.connect(graph, kinds, { ...edge, targetPort: "nope" })).toMatchObject({ code: "canvas.port.unknown" })
  expect(g.connect(graph, kinds, { ...edge, target: "completed" })).not.toHaveProperty("code")
})

test("an edit undoes and redoes; a drag undoes as one step", () => {
  const start = g.history(decompile(orderEnvelope, order))
  const added = g.commit(start, g.addNode(start.present, { id: g.freshId(start.present, "draft"), kind: "state", position: { x: 0, y: 0 }, config: {} }))
  expect(added.present.nodes.at(-1)?.id).toBe("draft_2")
  expect(g.undo(added).present).toBe(start.present)
  expect(g.redo(g.undo(added)).present).toBe(added.present)
  let dragged = g.checkpoint(added)
  for (const x of [10, 20, 30]) dragged = g.replace(dragged, g.moveNode(dragged.present, "draft", { x, y: 0 }))
  expect(g.undo(dragged).present).toBe(added.present)
})

test("renaming a state carries its steps; removing a state removes its steps", () => {
  const graph = g.renameNode(decompile(orderEnvelope, order), "on_hold", "paused")
  expect(graph.edges.filter(({ source, target }) => source === "paused" || target === "paused").map(({ config }) => config.transition).sort()).toEqual(["approve", "cancel", "hold"])
  expect(g.remove(graph, ["paused"]).edges.some(({ source, target }) => source === "paused" || target === "paused")).toBe(false)
})

const finding = (code: string, pointer: string, message = ""): Finding => ({ code, severity: "error", message, span: { pointer }, artifactId: order.artifactId })

test("findings land on the node or edge they point into", () => {
  const graph = decompile(orderEnvelope, order)
  const { origins } = compile(orderEnvelope, order, graph)
  const states = new Set(graph.nodes.map(({ id }) => id))
  const at = "/payload/objectType/lifecycles/0"
  const placed = place([
    finding("lifecycle.validation.self_transition", `${at}/transitions/1/from/1`),
    finding("lifecycle.validation.dead_end_state", at, 'state "invoiced" has no outgoing transition'),
    finding("lifecycle.validation.empty_initial", `${at}/initial`),
    { ...finding("x", "/payload"), artifactId: "other.artifact" },
  ], order.artifactId, origins, states)
  expect(placed.map(({ node, edge }) => node ?? edge ?? null)).toEqual(["approve:on_hold", "invoiced", null, null])
})

// The real checker, when its package is built into src/checker/ (Boat and the build do; CI's Studio job does not yet).
const pkg = join(import.meta.dir, "../src/checker")
test.skipIf(!existsSync(join(pkg, "norn_checker_wasm_bg.wasm")))("the checker finds the stuck state a deleted step leaves", async () => {
  const wasm = await import(join(pkg, "norn_checker_wasm.js")) as { initSync: (input: { module: BufferSource }) => unknown; checkArtifactSet: (a: string, c: string) => string }
  wasm.initSync({ module: readFileSync(join(pkg, "norn_checker_wasm_bg.wasm")) })
  const check = (set: readonly Envelope[]) => JSON.parse(wasm.checkArtifactSet(JSON.stringify(set), "null")) as CheckResult
  const graph = decompile(orderEnvelope, order)
  const clean = check(definitions)
  const edited = compile(orderEnvelope, order, g.remove(graph, [], ["complete:invoiced"]))
  const result = check(definitions.map((envelope) => envelope.artifactId === order.artifactId ? edited.envelope : envelope))
  const placed = place(result.diagnostics.diagnostics, order.artifactId, edited.origins, new Set(graph.nodes.map(({ id }) => id)))
  const added = placed.filter(({ finding: { code } }) => !clean.diagnostics.diagnostics.some((before) => before.code === code))
  expect(added.map(({ finding, node }) => `${finding.code}@${node}`).sort()).toEqual(["lifecycle.validation.dead_end_state@invoiced", "lifecycle.validation.unreachable_state@completed"])
})
