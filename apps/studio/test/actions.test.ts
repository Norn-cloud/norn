import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { instantText, isDiagnosticPlaced, offsetSuffix, offsetText, pickerDateTime, resolveDateTime, runtimeWallOffsets, selectedAlternative, type LocalWallOffsets } from "../src/lib/actions.ts"
import { BOOK_EQUIPMENT } from "../e2e/interval-fixtures.ts"
import { callAction, decimalExample, itemNoun, newIdempotencyKey, pastTense, receiptVerb, writesWhole, draftAt, emptyDraft, emptyFor, encodeObject, fieldsOf, groupsOf, inputPointer, listActions, problemsAt, sectionLabel, sectionsOf, unplaced, unsupported, withDraftAt, type Draft } from "../src/lib/actions.ts"
import { OPEN_HOLDING, RESTOCK_HOLDING } from "../e2e/action-fixtures.ts"

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
  expect(encodeObject(fields, draft)).toBe('{"id":"melt/1","input_fine_g":"9007199254740993","output_fine_g":"12,5"}')
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

test("unchecking \"No value\" restores a value of the field's kind, so Submit still encodes", () => {
  const fields = fieldsOf({
    type: "object", additionalProperties: false,
    properties: {
      note: { type: ["string", "null"] },
      tags: { type: ["array", "null"], items: { type: "string" } },
      one: { type: ["object", "null"], additionalProperties: false, properties: { sku: { type: "string" } } },
    },
  })
  const restored = Object.fromEntries(fields.map((f) => [f.name, emptyFor(f.spec)]))
  expect(restored).toEqual({ note: "", tags: [], one: { sku: "" } })
  expect(encodeObject(fields, { note: null, tags: null, one: null })).toBe('{"note":null,"tags":null,"one":null}')
  // A record or list restored from "No value" encodes like a fresh one rather than throwing on a string.
  expect(encodeObject(fields, restored)).toBe(encodeObject(fields, emptyDraft(fields)))
})

test("a shape the form cannot offer is named, not guessed at", () => {
  const fields = fieldsOf({ type: "object", additionalProperties: false, properties: { odd: { type: ["string", "integer"] } } })
  expect(unsupported(fields)).toBe("odd: type union integer | string")
  // ...also inside a list of records, where the member would otherwise be dropped silently.
  const nested = fieldsOf({ type: "object", additionalProperties: false, properties: { lines: { type: "array", items: { type: "object", additionalProperties: false, properties: { odd: { type: ["string", "integer"] } } } } } })
  expect(unsupported(nested)).toBe("lines[].odd: type union integer | string")
})

test("a nullable union keeps the node's other keywords: choices, items and members", () => {
  const fields = fieldsOf({
    type: "object", additionalProperties: false,
    properties: {
      kind: { type: ["string", "null"], enum: ["a", "b"] },
      tags: { type: ["array", "null"], items: { type: "string" } },
      one: { type: ["object", "null"], additionalProperties: false, properties: { sku: { type: "string" } } },
    },
  })
  expect(fields.find((f) => f.name === "kind")).toMatchObject({ nullable: true, spec: { kind: "choice", values: ["a", "b"] } })
  expect(fields.find((f) => f.name === "tags")).toMatchObject({ nullable: true, spec: { kind: "array", item: { kind: "text" } } })
  expect(fields.find((f) => f.name === "one")).toMatchObject({ nullable: true, spec: { kind: "object", fields: [{ name: "sku" }] } })
  expect(unsupported(fields)).toBeNull()
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

test("an approval-gated call answers 202 pending with the decision it opened, and commits nothing", async () => {
  const gated = cell(() => Response.json({ outcome: "pending", decision: { id: "dec_1", expiresAtMs: 1800000000000 } }, { status: 202 }))
  expect(await callAction("ops.refund", "{}", "k", gated.fetcher)).toEqual({ kind: "pending", decision: { id: "dec_1", expiresAtMs: 1800000000000 } })
})

// The documents the cell's route serves (crates/norn-cell-do/tests/actions.rs asserts it serves exactly these).
const served = JSON.parse(readFileSync(join(import.meta.dir, "fixtures/action-call.json"), "utf8")) as Record<"committed" | "conflict" | "invalidKey", unknown>

test("the cell's own receipt and refusal documents read as Studio's receipt and placed refusals", async () => {
  const ok = cell(() => Response.json(served.committed))
  expect(await callAction("constitution.melt.convert", '{"id":"melt-1","input_fine_g":12,"output_fine_g":10}', "k", ok.fetcher)).toEqual({ kind: "receipt", receipt: served.committed as never })
  const conflict = await callAction("constitution.melt.convert", "{}", "k", cell(() => Response.json(served.conflict, { status: 409 })).fetcher)
  // The cell's explanation and host-clock timestamp travel with the refusal, as it sent them.
  const conflictDoc = served.conflict as { explanation: string; decidedAt?: string }
  expect(conflict).toEqual({
    kind: "refused",
    status: 409,
    code: "action.idempotency_conflict",
    message: "the request conflicts with current state",
    diagnostics: [],
    explanation: conflictDoc.explanation,
    ...conflictDoc.decidedAt ? { decidedAt: conflictDoc.decidedAt } : {},
  })
  const invalid = await callAction("constitution.melt.convert", "{}", "", cell(() => Response.json(served.invalidKey, { status: 400 })).fetcher)
  expect(invalid.kind === "refused" && invalid.diagnostics).toEqual([{ code: "request.invalid", message: "idempotencyKey must be 1 to 128 printable characters and at most 256 bytes", pointer: "/idempotencyKey" }])
  // The key is not an input field: its finding reads at the top of the form.
  expect(invalid.kind === "refused" && unplaced(invalid.diagnostics, [])).toHaveLength(1)
})

test("DEFS's dialect: titles label fields, descriptions help, a multi-value enum is a choice", () => {
  const fields = fieldsOf(OPEN_HOLDING.inputSchema)
  const method = fields.find((f) => f.name === "valuation_method")
  expect(method).toMatchObject({ label: "Valuation method", spec: { kind: "choice", values: ["fifo", "moving_average"] } })
  expect(method?.description).toContain("How the cost of stock is worked out")
  expect(fields.find((f) => f.name === "note")?.label).toBe("Note") // no title: the id in words, sentence-cased
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
  expect(bin && sectionLabel(fields, bin)).toBe("Location › Bin")
  // The record's id, required and ungrouped, leads the first section; declared groups keep their fields.
  expect(sectionsOf(fields, groupsOf(OPEN_HOLDING.inputSchema), "id").map((s) => [s.title, s.fields.map((f) => f.pointer)])).toEqual([
    ["What is held", ["/id", "/resource_code", "/quantity", "/valuation_method"]],
    ["Where it is", ["/location/site"]],
    [null, ["/location/bin", "/note"]],
  ])
  const grouped = { ...OPEN_HOLDING.inputSchema, "x-norn": { groups: [{ id: "who", title: "Who", fields: ["/id"] }, ...OPEN_HOLDING.inputSchema["x-norn"].groups] } }
  expect(sectionsOf(fieldsOf(grouped), groupsOf(grouped), "id")[0]).toMatchObject({ title: "Who", fields: [{ pointer: "/id" }] })
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
  expect(encodeObject(fields, { ...(draft as { [k: string]: Draft }), id: "h1", quantity: "10.500" })).toBe('{"id":"h1","quantity":"10.500","location":{"site":"vault-a"}}')
})

test("contract v1 as CELL-DO approved it: any 404 or 503 is not served, whatever its body", async () => {
  const notFound = () => Response.json({ code: "route.not_found", message: "No such route." }, { status: 404 })
  expect(await listActions(undefined, cell(notFound).fetcher)).toEqual({ kind: "unavailable" })
  expect(await callAction("a", "{}", "k", cell(notFound).fetcher)).toEqual({ kind: "unavailable" })
  expect(await listActions(undefined, cell(() => Response.json({ code: "host.down", message: "down" }, { status: 503 })).fetcher)).toEqual({ kind: "unavailable" })
})

test("a parent with owned rows is offered as a whole-record write, like an upsert; a transition is not", () => {
  const base = { actionId: "a", title: "A", description: null, objectKind: "t", objectIdField: "id", inputSchema: {}, uiEffects: { irreversible: false, destructive: false, money: [] } }
  expect(writesWhole({ ...base, effect: "mutate_parent_with_rows" })).toBe(true)
  expect(writesWhole({ ...base, effect: "upsert_input_object" })).toBe(true)
  expect(writesWhole({ ...base, effect: "fire_transition" })).toBe(false)
})

test("idempotency keys are ASCII UUIDs, far inside the cell's 256-byte bound, and new each draft", () => {
  const [a, b] = [newIdempotencyKey(), newIdempotencyKey()]
  expect(a).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/)
  expect(new TextEncoder().encode(a).length).toBeLessThanOrEqual(256)
  expect(a).not.toBe(b)
})

test("the business sentence's verb and the decimal example", () => {
  expect(["Open a holding", "Adjust quantity", "Retire holding", "Restock holding", "Send invoice", "Copy lines", "Recount"].map(pastTense))
    .toEqual(["opened", "adjusted", "retired", "restocked", "sent", "copied", "recounted"])
  expect(pastTense("3-way match")).toBeNull()
  expect([decimalExample(3), decimalExample(0), decimalExample(null)]).toEqual(["e.g. 12.500", "e.g. 12", "e.g. 12"])
})

test("a list names its entries: the items' own title, else its label made singular", () => {
  expect(itemNoun("Lot", "Lots")).toBe("lot")
  expect([itemNoun(null, "Lots"), itemNoun(null, "Entries"), itemNoun(null, "Boxes"), itemNoun(null, "Status"), itemNoun(null, "Stock")]).toEqual(["lot", "entry", "box", "status", "stock"])
  const restock = fieldsOf(RESTOCK_HOLDING.inputSchema).find((f) => f.name === "lots")
  expect(restock?.spec).toMatchObject({ kind: "array", itemTitle: null })
})

test("a record's catalog is narrowed by the cell to what its state can take", async () => {
  const live = cell(() => Response.json({ items: [] }))
  await listActions("purchasing.PurchaseRequest", live.fetcher, "PR-1")
  expect(Object.fromEntries(live.calls[0]!.url.searchParams)).toEqual({ objectKind: "purchasing.PurchaseRequest", objectId: "PR-1", orgId: "org_demo", environmentId: "analytics" })
})

test("a receipt says what the record is now, from the cell's answer, never a guessed past tense", () => {
  const action = (effect: "upsert_input_object" | "fire_transition", toState?: string) =>
    ({ actionId: "a", title: effect === "fire_transition" ? "Submit for approval" : "New purchase request", description: null, objectKind: "purchasing.PurchaseRequest", effect, objectIdField: "request_id", inputSchema: {}, uiEffects: { irreversible: false, destructive: false, money: [] }, ...toState === undefined ? {} : { toState } }) as const
  expect(receiptVerb(action("upsert_input_object"), 1)).toBe("created")
  expect(receiptVerb(action("upsert_input_object"), 3)).toBe("updated")
  expect(receiptVerb(action("fire_transition", "submitted"), 2)).toBe("is now submitted")
  expect(receiptVerb(action("fire_transition", "on_hold"), 2)).toBe("is now on hold")
  expect(receiptVerb(action("fire_transition"), 2)).toBe("updated")
})

test("a refusal's request is the input as sent: every member, lists, objects and explicit nulls too, digit for digit", async () => {
  const { sentMembers } = await import("../src/lib/actions.ts")
  const sent = '{"id":"SCAN-INV-1041","total":"30.00","qty":12345678901234567890.10,"note":null,"lines":[{"amount":"18.50"},{"amount":"12.00"}],"meta":{"by":"scan \\"7\\""},"empty":""}'
  expect(sentMembers(sent)).toEqual([
    { name: "id", text: "SCAN-INV-1041" },
    { name: "total", text: "30.00" },
    // A number the form sent goes back as its own digits, never through a float.
    { name: "qty", text: "12345678901234567890.10" },
    { name: "note", text: "null" },
    { name: "lines", text: '[{"amount":"18.50"},{"amount":"12.00"}]' },
    { name: "meta", text: '{"by":"scan \\"7\\""}' },
    { name: "empty", text: "" },
  ])
  expect(sentMembers("{}")).toEqual([])
  expect(sentMembers(' { "a" : 1 , "b" : [ 1 , 2 ] } ')).toEqual([{ name: "a", text: "1" }, { name: "b", text: "[ 1 , 2 ]" }])
  for (const text of ["", "null", "[1]", "{"]) expect(sentMembers(text)).toBeNull()
})

test("a string input's format offers a date, time or instant picker; a nullable one keeps its picker behind No value", () => {
  const fields = fieldsOf(BOOK_EQUIPMENT.inputSchema)
  expect(fields.map((f) => `${f.name}:${f.spec.kind}${f.nullable ? "?" : ""}`)).toEqual(["id:text", "label:text", "equipment:text", "starts_at:date-time", "ends_at:date-time", "inspect_on:date?", "daily_check:time?"])
  // A format the dialect doesn't name stays a text input.
  expect(fieldsOf({ type: "object", properties: { at: { type: "string", format: "duration" } } })[0]?.spec).toEqual({ kind: "text" })
})

test("pickers send their formats: a date as typed, a time with seconds, a wall time with the reader's offset at it", () => {
  const fields = fieldsOf(BOOK_EQUIPMENT.inputSchema)
  const draft = { ...emptyDraft(fields), id: "booking/9", inspect_on: "2026-11-30", daily_check: "07:45", starts_at: "2026-11-27T09:30:00Z" }
  expect(JSON.parse(encodeObject(fields, draft))).toEqual({ id: "booking/9", inspect_on: "2026-11-30", daily_check: "07:45:00", starts_at: "2026-11-27T09:30:00Z" })
  // The offset is the reader's zone's at that wall time (DST included), never the offset of today.
  expect(instantText("2026-11-27T09:30", () => 120)).toBe("2026-11-27T09:30:00+02:00")
  expect(instantText("2026-07-01T23:05:07", () => -240)).toBe("2026-07-01T23:05:07-04:00")
  expect(instantText("2026-11-27T09:30", () => 0)).toBe("2026-11-27T09:30:00Z")
  expect(instantText("2026-11-27T09:30", () => 330)).toBe("2026-11-27T09:30:00+05:30")
  // Text that is not a picker's wall time goes as typed, for the cell to refuse at its pointer.
  expect(instantText("next friday")).toBe("next friday")
  expect(offsetText(-570)).toBe("-09:30")
})

test("an instant shows in the picker as the reader's wall time; a wall time shows as it is", () => {
  expect(pickerDateTime("2026-11-27T09:30")).toBe("2026-11-27T09:30")
  const at = new Date(Date.UTC(2026, 10, 27, 7, 30))
  const two = (n: number) => String(n).padStart(2, "0")
  expect(pickerDateTime("2026-11-27T07:30:00Z")).toBe(`${at.getFullYear()}-${two(at.getMonth() + 1)}-${two(at.getDate())}T${two(at.getHours())}:${two(at.getMinutes())}`)
  expect(pickerDateTime("garbage")).toBe("")
})

test("a refusal's nested pointer lands on the member's row; unmatched nested pointers stay on the form", () => {
  const fields = fieldsOf(OPEN_HOLDING.inputSchema)
  const diagnostics = [
    { code: "action.input_invalid", message: "Site is unknown.", pointer: "/input/location/site" },
    { code: "action.input_invalid", message: "Unknown nested key.", pointer: "/input/location/unknown" },
    { code: "request.invalid", message: "input must be a JSON object", pointer: "/input" },
    { code: "core.time.improper_interval", message: "Ends must be after it starts.", pointer: "/input/ends_at" },
  ]
  expect(problemsAt(diagnostics, "/location/site").map((d) => d.message)).toEqual(["Site is unknown."])
  expect(problemsAt(diagnostics, "/location").map((d) => d.message)).toEqual(["Site is unknown.", "Unknown nested key."])
  expect(inputPointer(diagnostics[2]!)).toBeNull()
  // `/location/site` is placed on its leaf FieldRow; `/location/unknown` and `/ends_at` have no FieldRow and stay in `unplaced`.
  expect(isDiagnosticPlaced("/location/site", fields)).toBe(true)
  expect(isDiagnosticPlaced("/location", fields)).toBe(true)
  expect(isDiagnosticPlaced("/location/unknown", fields)).toBe(false)
  expect(unplaced(diagnostics, fields).map((d) => d.message)).toEqual([
    "Unknown nested key.",
    "input must be a JSON object",
    "Ends must be after it starts.",
  ])

  // Array of objects: only rendered item indices with declared member fields are placed.
  const arrayFields = fieldsOf({
    type: "object",
    properties: {
      lines: { type: "array", items: { type: "object", properties: { sku: { type: "string" } } } },
    },
  })
  const draft: Draft = { lines: [{ sku: "A" }] }
  const arrayDiagnostics = [
    { code: "a", message: "Bad sku", pointer: "/input/lines/0/sku" },
    { code: "b", message: "Unknown item field", pointer: "/input/lines/0/unknown" },
    { code: "c", message: "Out of range item", pointer: "/input/lines/5/sku" },
    { code: "d", message: "List-level refusal", pointer: "/input/lines" },
  ]
  expect(unplaced(arrayDiagnostics, arrayFields, draft).map((d) => d.message)).toEqual([
    "Unknown item field",
    "Out of range item",
  ])

  // Nullable object set to null: child FieldRows are hidden by Nullable, so child pointers must stay in `unplaced`.
  const nullableObjectFields = fieldsOf({
    type: "object",
    properties: {
      location: { type: ["object", "null"], properties: { site: { type: "string" } } },
    },
  })
  const nullDraft: Draft = { location: null }
  const presentDraft: Draft = { location: { site: "" } }
  const nullableDiagnostics = [
    { code: "e", message: "Site required", pointer: "/input/location/site" },
    { code: "f", message: "Location required", pointer: "/input/location" },
  ]
  expect(isDiagnosticPlaced("/location/site", nullableObjectFields, nullDraft)).toBe(false)
  expect(isDiagnosticPlaced("/location", nullableObjectFields, nullDraft)).toBe(true)
  expect(unplaced(nullableDiagnostics, nullableObjectFields, nullDraft).map((d) => d.message)).toEqual(["Site required"])
  expect(isDiagnosticPlaced("/location/site", nullableObjectFields, presentDraft)).toBe(true)
  expect(unplaced(nullableDiagnostics, nullableObjectFields, presentDraft)).toEqual([])
})

test("DST spring gap is flagged without shifting; fall repeat matches by epoch instant and supports 2-hour repeats", () => {
  // Simulate America/New_York DST transitions:
  // - 2026-03-08 02:00..02:59 does not exist (spring forward -05:00 -> -04:00)
  // - 2026-11-01 01:00..01:59 occurs twice (fall back -04:00 then -05:00)
  const newYork: LocalWallOffsets = (p) => {
    if (p.year === 2026 && p.month === 3 && p.day === 8 && p.hour === 2) return []
    if (p.year === 2026 && p.month === 11 && p.day === 1 && p.hour === 1) return [-240, -300]
    return [-300]
  }
  // Spring gap: flagged nonexistent and never fabricated as `02:30:00-04:00` (which would shift to 01:30 EST).
  const gap = resolveDateTime("2026-03-08T02:30", newYork)
  expect(gap).toEqual({ kind: "nonexistent", text: "2026-03-08T02:30" })

  // Fall repeat from picker wall time: offers both occurrences chronologically.
  const repeat = resolveDateTime("2026-11-01T01:30", newYork)
  expect(repeat).toEqual({
    kind: "ok",
    text: "2026-11-01T01:30:00-04:00",
    alternatives: ["2026-11-01T01:30:00-04:00", "2026-11-01T01:30:00-05:00"],
  })

  // Fall repeat restored as UTC `Z` (`2026-11-01T06:30:00Z` === `01:30:00-05:00`, the second occurrence):
  // `selectedAlternative` matches by epoch instant rather than string offset equality.
  expect(selectedAlternative("2026-11-01T06:30:00Z", repeat.kind === "ok" ? repeat.alternatives : [])).toBe("2026-11-01T01:30:00-05:00")
  expect(selectedAlternative("2026-11-01T05:30:00Z", repeat.kind === "ok" ? repeat.alternatives : [])).toBe("2026-11-01T01:30:00-04:00")

  // Fractional seconds on DST repeat: preserved in both alternatives, epoch match selects second occurrence, and offsetSuffix stays clean.
  const fractional = resolveDateTime("2026-11-01T01:30:00.250", newYork)
  expect(fractional).toEqual({
    kind: "ok",
    text: "2026-11-01T01:30:00.250-04:00",
    alternatives: ["2026-11-01T01:30:00.250-04:00", "2026-11-01T01:30:00.250-05:00"],
  })
  expect(selectedAlternative("2026-11-01T06:30:00.250Z", fractional.kind === "ok" ? fractional.alternatives : [])).toBe("2026-11-01T01:30:00.250-05:00")
  expect(offsetSuffix("2026-11-01T01:30:00.250-05:00")).toBe("-05:00")
  expect(offsetSuffix("2026-11-01T06:30:00.250Z")).toBe("Z")

  // 2-hour fall-back repeat (Antarctica/Troll: +02:00 -> Z): both occurrences are exposed and matched by instant.
  const troll: LocalWallOffsets = (p) =>
    p.year === 2026 && p.month === 10 && p.day === 25 && p.hour === 1 ? [120, 0] : [0]
  const trollRepeat = resolveDateTime("2026-10-25T01:30", troll)
  expect(trollRepeat).toEqual({
    kind: "ok",
    text: "2026-10-25T01:30:00+02:00",
    alternatives: ["2026-10-25T01:30:00+02:00", "2026-10-25T01:30:00Z"],
  })
  expect(selectedAlternative("2026-10-25T01:30:00+00:00", trollRepeat.kind === "ok" ? trollRepeat.alternatives : [])).toBe("2026-10-25T01:30:00Z")
  // When running under real TZ=Antarctica/Troll, runtimeWallOffsets also discovers both 2-hour-apart occurrences.
  if (Intl.DateTimeFormat().resolvedOptions().timeZone === "Antarctica/Troll") {
    expect(runtimeWallOffsets({ year: 2026, month: 10, day: 25, hour: 1, minute: 30, second: 0 })).toEqual([120, 0])
  }
  if (Intl.DateTimeFormat().resolvedOptions().timeZone === "America/New_York") {
    expect(runtimeWallOffsets({ year: 2026, month: 3, day: 8, hour: 2, minute: 30, second: 0 })).toEqual([])
    expect(runtimeWallOffsets({ year: 2026, month: 11, day: 1, hour: 1, minute: 30, second: 0 })).toEqual([-240, -300])
    const utcSecond = resolveDateTime("2026-11-01T06:30:00Z")
    expect(utcSecond.kind).toBe("ok")
    if (utcSecond.kind === "ok") {
      expect(selectedAlternative(utcSecond.text, utcSecond.alternatives)).toBe("2026-11-01T01:30:00-05:00")
    }
    const utcFracSecond = resolveDateTime("2026-11-01T06:30:00.250Z")
    expect(utcFracSecond).toEqual({
      kind: "ok",
      text: "2026-11-01T06:30:00.250Z",
      alternatives: ["2026-11-01T01:30:00.250-04:00", "2026-11-01T01:30:00.250-05:00"],
    })
    if (utcFracSecond.kind === "ok") {
      expect(selectedAlternative(utcFracSecond.text, utcFracSecond.alternatives)).toBe("2026-11-01T01:30:00.250-05:00")
    }
  }

  // Fall repeat already carrying `-05:00` (restored/prefilled): preserves exact `-05:00` in both preview & encoded payload.
  const restored = resolveDateTime("2026-11-01T01:30:00-05:00", newYork)
  expect(restored.kind).toBe("ok")
  expect(restored.text).toBe("2026-11-01T01:30:00-05:00")
  const fields = fieldsOf(BOOK_EQUIPMENT.inputSchema)
  expect(JSON.parse(encodeObject(fields, { ...emptyDraft(fields), starts_at: "2026-11-01T01:30:00-05:00" }))).toEqual({
    starts_at: "2026-11-01T01:30:00-05:00",
  })
})
