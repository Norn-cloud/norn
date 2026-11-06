import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { callAction, draftAt, emptyDraft, encodeObject, fieldsOf, groupsOf, inputPointer, listActions, problemsAt, sectionLabel, sectionsOf, unplaced, unsupported, withDraftAt, type Draft } from "../src/lib/actions.ts"
import { OPEN_HOLDING } from "../e2e/action-fixtures.ts"

// Real admitted actions from the cell's own fixtures: an upsert with exact decimals and a transition.
const fixture = (name: string) => JSON.parse(readFileSync(join(import.meta.dir, "../../../crates/norn-cell/tests/fixtures", name), "utf8")) as unknown
const actions = (doc: unknown) => {
  const found: Record<string, { inputSchema: unknown; effectProgram: { kind: string } }> = {}
  const walk = (node: unknown) => {
    if (Array.isArray(node)) node.forEach(walk)
    else if (node !== null && typeof node === "object") {
      const o = node as Record<string, unknown>
      if (typeof o["actionId"] === "string" && "inputSchema" in o) found[o["actionId"]] = o as never
      Object.values(o).forEach(walk)
    }
  }
  walk(doc)
  return found
}
const melt = actions(fixture("constitution.json"))["constitution.melt.convert"]
const receive = actions(fixture("rea-inventory.json"))["inventory.cycle.receive"]

test("a real upsert's closed schema becomes typed fields, required first", () => {
  const fields = fieldsOf(melt?.inputSchema)
  expect(fields.map((f) => `${f.name}:${f.spec.kind}${f.required ? "*" : ""}`)).toEqual(["id:text*", "input_fine_g:decimal*", "output_fine_g:decimal*", "loss_fine_g:decimal"])
  expect(fields[1]?.spec).toEqual({ kind: "decimal", scale: 0, unit: "g" })
  expect(unsupported(fields)).toBeNull()
})

test("exact numbers reach the cell digit for digit; what is not a number goes as text for the cell to refuse", () => {
  const fields = fieldsOf(melt?.inputSchema)
  const draft = { ...emptyDraft(fields), id: "melt/1", input_fine_g: "9007199254740993", output_fine_g: "12,5" }
  expect(encodeObject(fields, draft)).toBe('{"id":"melt/1","input_fine_g":9007199254740993,"output_fine_g":"12,5"}')
})

test("fields left empty are omitted, so the cell names what it requires", () => {
  const fields = fieldsOf(receive?.inputSchema)
  expect(encodeObject(fields, emptyDraft(fields))).toBe("{}")
})

test("nullable leaves, fixed currencies, records, lists and the JSON leaf encode as the dialect reads them", () => {
  const schema = {
    type: "object", additionalProperties: false, required: ["lines"],
    properties: {
      note: { type: ["string", "null"] },
      currency: { type: "string", enum: ["USD"] },
      paid: { type: "boolean" },
      meta: { type: ["array", "boolean", "null", "number", "object", "string"] },
      lines: { type: "array", items: { type: "object", additionalProperties: false, required: ["sku"], properties: { sku: { type: "string" }, qty: { type: "integer" } } } },
    },
  }
  const fields = fieldsOf(schema)
  expect(fields.find((f) => f.name === "note")).toMatchObject({ nullable: true, spec: { kind: "text" } })
  expect(fields.find((f) => f.name === "meta")?.spec.kind).toBe("json")
  const draft: { [name: string]: Draft } = { ...emptyDraft(fields), note: null, paid: "true", meta: '{"a": 1.50}', lines: [{ sku: "A-1", qty: "3" }, { sku: "B", qty: "" }] }
  expect(encodeObject(fields, draft)).toBe('{"lines":[{"sku":"A-1","qty":3},{"sku":"B"}],"note":null,"currency":"USD","paid":true,"meta":{"a": 1.50}}')
})

test("a shape the form cannot offer is named, not guessed at", () => {
  const fields = fieldsOf({ type: "object", additionalProperties: false, properties: { odd: { type: ["string", "integer"] } } })
  expect(unsupported(fields)).toBe("odd: type union integer | string")
})

test("refusal pointers land on their field, nested ones on the field that holds them, the rest at the top", () => {
  const fields = fieldsOf({ type: "object", additionalProperties: false, properties: { qty: { type: "integer" }, lines: { type: "array", items: { type: "string" } } } })
  const diagnostics = [
    { code: "action.input_invalid", message: "qty must be an integer", pointer: "/input/qty" },
    { code: "action.input_invalid", message: "a line is empty", pointer: "/input/lines/1" },
    { code: "action.precondition_failed", message: "The holding is closed.", pointer: null },
    { code: "action.input_invalid", message: "unknown member", pointer: "/input/extra" },
  ]
  expect(inputPointer(diagnostics[0]!)).toBe("/qty")
  expect(problemsAt(diagnostics, "/qty").map((d) => d.message)).toEqual(["qty must be an integer"])
  expect(problemsAt(diagnostics, "/lines").map((d) => d.message)).toEqual(["a line is empty"])
  expect(unplaced(diagnostics, fields).map((d) => d.message)).toEqual(["The holding is closed.", "unknown member"])
})

const ME = { user: { id: "u" }, organization: { id: "org_demo", name: "Demo", role: "member" }, environmentId: "analytics" }
function cell(host: (url: URL, init?: RequestInit) => Response, me: unknown = ME) {
  const calls: { url: URL; body: string | undefined }[] = []
  const fetcher = async (input: string, init?: RequestInit) => {
    const url = new URL(input, "https://studio.test")
    if (url.pathname === "/api/me") return Response.json(me)
    calls.push({ url, body: typeof init?.body === "string" ? init.body : undefined })
    return host(url, init)
  }
  return { calls, fetcher }
}

test("the catalog is read scoped, filtered by kind on the server, and absent routes read as unavailable", async () => {
  const live = cell(() => Response.json({ items: [{ actionId: "a" }] }))
  expect(await listActions("canon.Holding", live.fetcher)).toMatchObject({ kind: "catalog", items: [{ actionId: "a" }] })
  expect(live.calls[0]?.url.pathname).toBe("/api/host/v1/actions")
  expect(Object.fromEntries(live.calls[0]!.url.searchParams)).toEqual({ objectKind: "canon.Holding", orgId: "org_demo", environmentId: "analytics" })
  expect(await listActions(undefined, cell(() => new Response("", { status: 404 })).fetcher)).toEqual({ kind: "unavailable" })
  const none = cell(() => { throw new Error("no call") }, { ...ME, environmentId: null })
  expect(await listActions(undefined, none.fetcher)).toEqual({ kind: "no-cell" })
  expect(none.calls).toHaveLength(0)
})

test("a call sends the encoded input and its key; a refusal keeps every diagnostic", async () => {
  const receipt = { outcome: "committed", objectType: "constitution.Melt", objectId: "melt/1", version: 1, receipt: { receiptHash: "ab", commitSeq: 7, recordedAtMs: 1 } }
  const ok = cell(() => Response.json(receipt))
  expect(await callAction("constitution.melt.convert", '{"input_fine_g":12.000}', "k1", ok.fetcher)).toEqual({ kind: "receipt", receipt: receipt as never })
  expect(ok.calls[0]?.url.pathname).toBe("/api/host/v1/actions/constitution.melt.convert")
  expect(ok.calls[0]?.body).toBe('{"input":{"input_fine_g":12.000},"idempotencyKey":"k1"}')
  const refused = cell(() => Response.json({ code: "action.input_invalid", message: "The input does not fit.", diagnostics: [{ code: "x", message: "m", pointer: "/input/a" }, { code: "y", message: "n", pointer: "/input/b" }] }, { status: 422 }))
  const result = await callAction("a", "{}", "k", refused.fetcher)
  expect(result).toMatchObject({ kind: "refused", status: 422, code: "action.input_invalid" })
  expect(result.kind === "refused" && result.diagnostics.map((d) => d.pointer)).toEqual(["/input/a", "/input/b"])
})

test("DEFS's dialect: titles label fields, descriptions help, a multi-value enum is a choice", () => {
  const fields = fieldsOf(OPEN_HOLDING.inputSchema)
  const method = fields.find((f) => f.name === "valuation_method")
  expect(method).toMatchObject({ label: "Valuation method", spec: { kind: "choice", values: ["fifo", "moving_average"] } })
  expect(method?.description).toContain("How the cost of stock is worked out")
  expect(fields.find((f) => f.name === "note")?.label).toBe("note") // no title: the id in words
})

test("groups become sections in their order, reaching into a record; the rest come last, untitled, each field once", () => {
  const fields = fieldsOf(OPEN_HOLDING.inputSchema)
  const sections = sectionsOf(fields, groupsOf(OPEN_HOLDING.inputSchema))
  expect(sections.map((s) => [s.title, s.fields.map((f) => f.pointer)])).toEqual([
    ["What is held", ["/resource_code", "/quantity", "/valuation_method"]],
    ["Where it is", ["/location/site"]],
    [null, ["/id", "/location/bin", "/note"]],
  ])
  const bin = sections[2]?.fields.find((f) => f.pointer === "/location/bin")
  expect(bin && sectionLabel(fields, bin)).toBe("Location › bin")
})

test("without groups the whole form is one untitled section", () => {
  const fields = fieldsOf(melt?.inputSchema)
  expect(sectionsOf(fields, groupsOf(melt?.inputSchema)).map((s) => [s.title, s.fields.length])).toEqual([[null, 4]])
})

test("a draft is read and written by pointer, nested records included", () => {
  const fields = fieldsOf(OPEN_HOLDING.inputSchema)
  const draft = withDraftAt(emptyDraft(fields), "/location/site", "vault-a")
  expect(draftAt(draft, "/location/site")).toBe("vault-a")
  expect(draftAt(draft, "/location/bin")).toBe("")
  expect(encodeObject(fields, { ...(draft as { [k: string]: Draft }), id: "h1", quantity: "10.500" })).toBe('{"id":"h1","quantity":10.500,"location":{"site":"vault-a"}}')
})
