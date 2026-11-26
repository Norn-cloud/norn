// Studio's record page (#1762 C2) through the renderer: one admitted page for every object type. Its
// record is a context read the host serves (`BindingRead` `record`), each outcome leaves the state
// the machine declares, each island draws only in the states it is held in, its text resolves
// through the host's translations, and a page with no expression never loads the engine.
import { expect, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import { recordPageArtifact } from "../../../registry/studio/record-page.ts"
import { missingCapabilities, requiredCapabilities } from "../src/capabilities.ts"
import type { IslandRegistry } from "../src/islands.ts"
import { lazyEngine } from "../src/machine/evaluate.ts"
import type { Classified } from "../src/machine/index.ts"
import type { Machine } from "../src/machine/machine.ts"
import { Node, type PageScope } from "../src/nodes.tsx"
import type { BindingRead, BindingReadState, WidgetReader } from "../src/reads.ts"
import { NornAppRenderer } from "../src/renderer.tsx"
import { createPageRuntime, needsEngine, type MachineRuntime, type RuntimeView } from "../src/runtime.ts"
import { text, type Message, type WidgetUx } from "../src/ux.ts"
import { matrixArtifact } from "./fixtures/matrix-page.ts"

const ux = (recordPageArtifact() as unknown as { readonly payload: { readonly ux: WidgetUx } }).payload.ux
const page = ux.pages[0]!
const machine = page.machine as Machine
const context = { objectType: "canon.Party", id: "demo/party/0001" }
const flush = () => new Promise((resolve) => setTimeout(resolve, 0))
const noEngine = lazyEngine(() => Promise.reject(new Error("the record page evaluates no expression")))

/** Answer the read that asks for it later: it is held until its signal aborts it, as a fetch is. */
const HOLD = "hold"

/** A reader that answers each record read with the next answer, or holds it (none left, or `HOLD`). */
function reader(answers: (Classified | typeof HOLD)[]): WidgetReader & { readonly asked: BindingRead[] } {
  const asked: BindingRead[] = []
  return {
    kind: "live",
    asked,
    widget: (read) => ({
      queryKey: ["widget", read],
      queryFn: ({ signal }: { readonly signal: AbortSignal }) => {
        asked.push(read)
        const answer = answers.shift()
        if (answer !== undefined && answer !== HOLD) return Promise.resolve(answer)
        return new Promise<Classified>((_, reject) => signal.addEventListener("abort", () => reject(signal.reason), { once: true }))
      },
    }),
  }
}

function run(answers: (Classified | typeof HOLD)[], hostContext: Readonly<Record<string, unknown>> = context) {
  const read = reader(answers)
  let view: RuntimeView | undefined
  const runtime = createPageRuntime({
    ux, machine, reader: read, calls: { call: async () => ({ outcome: "not_served", payload: {} }) }, engine: noEngine,
    host: () => ({ context: hostContext, objectTypeOf: () => undefined, actionIdOf: () => undefined }),
  }, (next) => { view = next })
  return { read, runtime, view: () => view ?? runtime.view() }
}

test("the record page is drawn whole: it needs only what this renderer declares, a context read among them", () => {
  expect(requiredCapabilities(ux)).toContain("read:context")
  expect(missingCapabilities(ux)).toEqual([])
})

test("the record is read as the context names it, through the host's reader, and each outcome leaves its declared state", async () => {
  const cases: readonly (readonly [Classified, string, BindingReadState["kind"]])[] = [
    [{ outcome: "accepted", payload: {}, body: { object: {} } }, "showing", "ok"],
    [{ outcome: "not_served", payload: {} }, "absent", "not_served"],
    [{ outcome: "refused", category: "visibility", payload: { code: "object.not_found" }, body: { code: "object.not_found" } }, "absent", "refused"],
    [{ outcome: "refused", category: "permission", payload: { code: "access.denied" }, body: { code: "access.denied" } }, "failed", "refused"],
    [{ outcome: "network_failure", payload: {} }, "failed", "unavailable"],
    [{ outcome: "answer_moved", payload: {} }, "failed", "answer_moved"],
  ]
  for (const [answer, state, read] of cases) {
    const page = run([answer])
    await flush()
    expect(page.read.asked).toEqual([{ kind: "record", objectType: "canon.Party", id: "demo/party/0001" }])
    expect(page.view().snapshot.stateId).toBe(state)
    expect(page.view().reads.get("record")?.kind).toBe(read)
    page.runtime.dispose()
  }
})

test("Retry reads again from failed; Read again keeps the page in refreshing until the cell answers", async () => {
  const page = run([{ outcome: "network_failure", payload: {} }, { outcome: "accepted", payload: {}, body: {} }])
  await flush()
  expect(page.view().snapshot.stateId).toBe("failed")
  page.runtime.send({ kind: "user", eventId: "retry", payload: {} })
  await flush()
  expect(page.view().snapshot.stateId).toBe("showing")
  page.runtime.send({ kind: "user", eventId: "read-again", payload: {} })
  expect(page.view().snapshot.stateId).toBe("refreshing")
  expect(page.read.asked).toHaveLength(3)
  page.runtime.dispose()
})

test("an action that finishes while the record is read again is never dropped: the earlier read is cancelled and the record read again", async () => {
  for (const eventId of ["read-again", "corrected"]) {
    const before = { object: { version: 1 } }
    const after = { object: { version: 2 } }
    const page = run([{ outcome: "accepted", payload: {}, body: before }, HOLD, { outcome: "accepted", payload: {}, body: after }])
    await flush()
    page.runtime.send({ kind: "user", eventId: "read-again", payload: {} })
    expect(page.view().snapshot.stateId).toBe("refreshing")
    // The action finishes while the read it may predate is still in flight.
    page.runtime.send({ kind: "user", eventId, payload: {} })
    await flush()
    expect(page.read.asked).toHaveLength(3)
    expect(page.view().snapshot.stateId).toBe("showing")
    expect(page.view().reads.get("record")).toEqual({ kind: "ok", document: after, freshness: "snapshot" })
    page.runtime.dispose()
  }
})

test("a read that answered just before it was cancelled delivers nothing: the record is read again and the later answer wins", async () => {
  for (const eventId of ["read-again", "corrected"]) {
    const before = { object: { version: 1 } }
    const stale = { object: { version: 1, stale: true } }
    const after = { object: { version: 2 } }
    const page = run([{ outcome: "accepted", payload: {}, body: before }, { outcome: "accepted", payload: {}, body: stale }, { outcome: "accepted", payload: {}, body: after }])
    await flush()
    page.runtime.send({ kind: "user", eventId: "read-again", payload: {} })
    expect(page.read.asked).toHaveLength(2)
    // Read #2 has its answer, but its fulfilment has not run when the event cancels it.
    page.runtime.send({ kind: "user", eventId, payload: {} })
    await flush()
    expect(page.read.asked).toHaveLength(3)
    expect(page.view().snapshot.stateId).toBe("showing")
    expect(page.view().reads.get("record")).toEqual({ kind: "ok", document: after, freshness: "snapshot" })
    page.runtime.dispose()
  }
})

test("a host that does not name the record is not served it: nothing is read", async () => {
  const page = run([], { objectType: "canon.Party" })
  await flush()
  expect(page.read.asked).toEqual([])
  expect(page.view().snapshot.stateId).toBe("absent")
  page.runtime.dispose()
})

test("the record page never loads the engine; a page with an expression does", () => {
  expect(needsEngine(ux, machine)).toBe(false)
  const matrix = matrixArtifact().payload.ux as WidgetUx
  expect(needsEngine(matrix, matrix.pages[0]!.machine as Machine)).toBe(true)
})

function scope(stateId: string, islands: IslandRegistry, translate?: (id: string, locale: string) => string | undefined): PageScope {
  const runtime: MachineRuntime = { snapshot: { stateId, fields: {}, entry: 1 }, engine: "idle", evaluate: noEngine.evaluate, send: () => {}, retryEngine: () => {}, reads: new Map(), calls: new Map(), waiting: 0, fault: null }
  return { ux, page, machine, runtime, islands, locale: "ar", translate, navigate: () => {}, raisedBy: { current: null }, actionEffects: () => undefined }
}

test("each island is drawn only in the states it is held in, and the page's root names its region without a heading", () => {
  const islands = Object.fromEntries((ux.catalog ?? []).map((entry) => [entry.entryId, () => <i>{entry.entryId}</i>])) as IslandRegistry
  const at = (state: string) => renderToStaticMarkup(<Node scope={scope(state, islands)} node={page.root} />)
  expect(at("loading")).toContain("<i>record-reading</i>")
  expect(at("loading")).not.toContain("record-header</i>")
  expect(at("failed")).toContain("<i>record-failed</i>")
  for (const state of ["showing", "refreshing"]) {
    expect(at(state)).toContain("<i>record-header</i>")
    expect(at(state)).toContain("<i>record-body</i>")
    expect(at(state)).not.toContain("<i>record-reading</i>")
  }
  expect(at("absent")).toContain("<i>record-absent</i>")
  expect(at("opening")).not.toContain("<i>")
  expect(at("showing")).toMatch(/^<section aria-label="Record"/)
  expect(at("showing")).not.toContain("<h2")
})

test("a message draws through the host's translations, and its source text when they have none", () => {
  const label = page.root.component.kind === "section" ? page.root.component.section.label : { en: "" }
  const { id: recordId } = (label as Message).msg
  const arabic = (id: string, locale: string) => (locale === "ar" && id === recordId ? "السجل" : undefined)
  expect(text(label, "ar", arabic)).toBe("السجل")
  expect(text(label, "en", arabic)).toBe("Record")
  expect(text(label, "ar")).toBe("Record")
  expect(text({ en: "Records", ar: "السجلات" }, "ar")).toBe("السجلات")
  const markup = renderToStaticMarkup(<Node scope={scope("showing", {}, arabic)} node={page.root} />)
  expect(markup).toContain('aria-label="السجل"')
})

test("an unavailable workspace carries the shell's marker, with the host's Retry", () => {
  const islands = Object.fromEntries((ux.catalog ?? []).map((entry) => [entry.entryId, () => null])) as IslandRegistry
  const markup = renderToStaticMarkup(
    <NornAppRenderer ux={ux} context={context} workspace={{ kind: "unavailable", error: new Error("The mode read failed.") }} calls={{ call: async () => ({ outcome: "not_served", payload: {} }) }}
      engine={() => Promise.reject(new Error("unused"))} islands={islands} objectTypeOf={() => undefined} actionIdOf={() => undefined} onRetryWorkspace={() => {}} />,
  )
  expect(markup).toContain("data-workspace-unavailable=\"\"")
  expect(markup).toContain(">Retry</button>")
  // With no Retry to offer, the page does not ask the shell to step aside: the shell's own status, and its Retry, stay.
  const bare = renderToStaticMarkup(
    <NornAppRenderer ux={ux} context={context} workspace={{ kind: "unavailable", error: new Error("The mode read failed.") }} calls={{ call: async () => ({ outcome: "not_served", payload: {} }) }}
      engine={() => Promise.reject(new Error("unused"))} islands={islands} objectTypeOf={() => undefined} actionIdOf={() => undefined} />,
  )
  expect(bare).toContain('data-workspace="unavailable"')
  expect(bare).not.toContain("data-workspace-unavailable")
})

test("only a msg object with a string id and source is a message", () => {
  expect(() => text({ msg: null } as never, "en")).not.toThrow()
  expect(text({ msg: "Retry", ar: "إعادة" } as never, "ar")).toBe("إعادة")
  expect(text({ msg: { id: "m1", source: "Retry" } }, "en")).toBe("Retry")
})
