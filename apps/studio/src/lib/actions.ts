import { CellUnavailable, readStudioScope, scopedHostFetch } from "./definitions.ts"

/**
 * Governed actions from Studio: the catalog the cell offers this reader, and one call.
 * Contract Studio proposes to CELL-DO (routes 3 and 4), alongside `GET /v1/objects/{type}`:
 *
 *   GET  /v1/actions?orgId&environmentId[&objectKind=T]
 *        200 { items: ActionSummary[] }: only actions this actor may call and the kernel can run
 *        (the cell decides; Studio filters nothing). Each item carries the admitted `inputSchema`
 *        (ActionIR's closed input dialect with DEFS's title, description, enum and x-norn.groups),
 *        `effect` and `objectIdField` from its effect program, `title` (falls back to `actionId`)
 *        and `uiEffects`, the kernel's effective effects (always present).
 *
 *   POST /v1/actions/{actionId}?orgId&environmentId
 *        body { input: <object>, idempotencyKey: string }   (1..128 chars and at most 256 UTF-8
 *        bytes; Studio sends one random UUID per draft)
 *        200 { outcome: "committed" | "replayed", objectType, objectId, version,
 *              receipt: { receiptHash, commitSeq, recordedAtMs } }
 *        4xx Refusal { code, category, message, diagnostics: [{ code, message, pointer }] }:
 *            every input violation listed, each pointer into the request body (`/input/qty`,
 *            `/input/lines/0/sku`); a rule or precondition refusal has `pointer: null`.
 *        409 `action.idempotency_conflict` when the key was used for a different call.
 *
 *   Any 404 or 503, whatever its body (the cell's carries `route.not_found`), means the cell does
 *   not serve the route: Studio says so and offers no action.
 *
 * Studio never validates input. It encodes what was typed as faithfully as JSON allows and
 * shows the cell's refusal at the field its pointer names.
 */

export interface UiEffects { readonly irreversible: boolean; readonly destructive: boolean; readonly money: readonly string[] }

export interface ActionSummary {
  readonly actionId: string
  readonly title: string
  readonly description: string | null
  readonly objectKind: string
  /** `mutate_parent_with_rows` writes a parent and its owned rows; `objectIdField` is the parent's id. */
  readonly effect: "upsert_input_object" | "mutate_parent_with_rows" | "fire_transition"
  readonly objectIdField: string
  readonly inputSchema: unknown
  /** The kernel's effective effects: destructive can be true without being declared. */
  readonly uiEffects: UiEffects
}

/** Whether an action writes a record whole (creating it when the id is new): offered as "New …". */
export const writesWhole = (action: ActionSummary) => action.effect !== "fire_transition"

/** Whether the form confirms before it calls. */
export const risky = (action: ActionSummary) => action.uiEffects.destructive || action.uiEffects.irreversible

/** One idempotency key per draft: a random UUID, ASCII and 36 bytes, well inside the cell's 256-byte bound. */
export const newIdempotencyKey = (): string => crypto.randomUUID()

export interface ActionReceipt {
  readonly outcome: "committed" | "replayed"
  readonly objectType: string
  readonly objectId: string
  readonly version: number
  readonly receipt: { readonly receiptHash: string; readonly commitSeq: number; readonly recordedAtMs: number }
}

export interface Diagnostic { readonly code: string; readonly message: string; readonly pointer: string | null }

export type CatalogRead =
  | { readonly kind: "catalog"; readonly items: readonly ActionSummary[] }
  | { readonly kind: "no-cell" }
  /** The cell does not serve the action routes yet (404 without a refusal, or 503: no host). */
  | { readonly kind: "unavailable" }
  | { readonly kind: "refused"; readonly status: number; readonly code: string; readonly message: string }

export type CallResult =
  | { readonly kind: "receipt"; readonly receipt: ActionReceipt }
  | { readonly kind: "refused"; readonly status: number; readonly code: string; readonly message: string; readonly diagnostics: readonly Diagnostic[] }
  | { readonly kind: "no-cell" }
  | { readonly kind: "unavailable" }

export const ACTIONS_UNAVAILABLE = "This cell doesn't take actions from Studio yet."

type Fetch = (input: string, init?: RequestInit) => Promise<Response>
const defaultFetch: Fetch = (input, init) => fetch(input, init)

interface Refusal { readonly code: string; readonly message: string; readonly diagnostics: readonly Diagnostic[] }

function refusalOf(text: string): Refusal | null {
  let body: unknown
  try { body = JSON.parse(text) } catch { return null }
  const { code, message, diagnostics } = (body ?? {}) as { code?: unknown; message?: unknown; diagnostics?: unknown }
  if (typeof code !== "string") return null
  const list = Array.isArray(diagnostics) ? diagnostics.map((d: { code?: unknown; message?: unknown; pointer?: unknown }) => ({
    code: String(d.code ?? ""), message: String(d.message ?? ""), pointer: typeof d.pointer === "string" ? d.pointer : null,
  })) : []
  return { code, message: typeof message === "string" ? message : code, diagnostics: list }
}

async function scoped(fetcher: Fetch) {
  const scope = await readStudioScope(fetcher)
  return scope === null ? null : scopedHostFetch(fetcher, "/api/host", async () => scope)
}

export async function listActions(objectKind: string | undefined, fetcher: Fetch = defaultFetch): Promise<CatalogRead> {
  let call
  try { call = await scoped(fetcher) } catch (error) { if (error instanceof CellUnavailable) return { kind: "unavailable" }; throw error }
  if (call === null) return { kind: "no-cell" }
  const query = objectKind === undefined ? "" : `?${new URLSearchParams({ objectKind })}`
  const response = await call(`/v1/actions${query}`, { headers: { accept: "application/json" } })
  const text = await response.text()
  if (response.status === 404 || response.status === 503) return { kind: "unavailable" }
  if (response.ok) {
    let items: unknown
    try { items = (JSON.parse(text) as { items?: unknown }).items } catch { items = undefined }
    if (!Array.isArray(items)) throw new CellUnavailable(response.status, "The cell's action catalog was not a list.")
    return { kind: "catalog", items: items as ActionSummary[] }
  }
  const refusal = refusalOf(text)
  if (refusal !== null) return { kind: "refused", status: response.status, code: refusal.code, message: refusal.message }
  throw new CellUnavailable(response.status)
}

/** Send one call. `input` is already-encoded JSON text, so exact numbers reach the cell as typed. */
export async function callAction(actionId: string, input: string, idempotencyKey: string, fetcher: Fetch = defaultFetch): Promise<CallResult> {
  const call = await scoped(fetcher)
  if (call === null) return { kind: "no-cell" }
  const response = await call(`/v1/actions/${encodeURIComponent(actionId)}`, {
    method: "POST",
    headers: { accept: "application/json", "content-type": "application/json" },
    body: `{"input":${input},"idempotencyKey":${JSON.stringify(idempotencyKey)}}`,
  })
  const text = await response.text()
  if (response.status === 404 || response.status === 503) return { kind: "unavailable" }
  if (response.ok) return { kind: "receipt", receipt: JSON.parse(text) as ActionReceipt }
  const refusal = refusalOf(text)
  if (refusal !== null) return { kind: "refused", status: response.status, ...refusal }
  throw new CellUnavailable(response.status)
}

// ---- The form a closed input schema implies -------------------------------------------------

export type Spec =
  | { readonly kind: "text" }
  | { readonly kind: "integer" }
  | { readonly kind: "number" }
  | { readonly kind: "decimal"; readonly scale: number | null; readonly unit: string | null }
  | { readonly kind: "boolean" }
  /** One admitted value only (a fixed currency, or an enum of one): shown, not chosen. */
  | { readonly kind: "fixed"; readonly value: string }
  /** A string input that writes an enum property: its admitted values, in order. */
  | { readonly kind: "choice"; readonly values: readonly string[] }
  /** The dialect's bounded JSON leaf (the full six-type union). */
  | { readonly kind: "json" }
  | { readonly kind: "object"; readonly fields: readonly FormField[]; readonly groups: readonly Group[] }
  | { readonly kind: "array"; readonly item: Spec }
  /** A shape this form cannot offer; the action is shown but not callable from Studio. */
  | { readonly kind: "unsupported"; readonly reason: string }

/** A titled set of fields (`x-norn.groups`): a form section, or a wizard step. Pointers are RFC 6901, into this object's value. */
export interface Group { readonly id: string; readonly title: string; readonly fields: readonly string[] }

export interface FormField {
  readonly name: string
  /** The schema's `title`, else the id in words. */
  readonly label: string
  readonly description: string | null
  /** JSON pointer into `input`. */
  readonly pointer: string
  readonly required: boolean
  /** The schema also admits null: the field offers an explicit "no value". */
  readonly nullable: boolean
  readonly spec: Spec
}

const escapePointer = (name: string) => name.replaceAll("~", "~0").replaceAll("/", "~1")
const JSON_DOMAIN = ["array", "boolean", "null", "number", "object", "string"]

function specOf(schema: unknown, pointer: string): { spec: Spec; nullable: boolean } {
  const node = (schema ?? {}) as Record<string, unknown>
  const type = node["type"]
  if (Array.isArray(type)) {
    const names = [...new Set(type.map(String))].sort()
    if (names.join() === JSON_DOMAIN.join()) return { spec: { kind: "json" }, nullable: false }
    const inner = names.filter((name) => name !== "null")
    if (names.includes("null") && inner.length === 1) return { spec: specOf({ type: inner[0] }, pointer).spec, nullable: true }
    return { spec: { kind: "unsupported", reason: `type union ${names.join(" | ")}` }, nullable: false }
  }
  switch (type) {
    case "string": {
      const values = node["enum"]
      if (Array.isArray(values) && values.length > 0 && values.every((v) => typeof v === "string")) {
        return values.length === 1 ? { spec: { kind: "fixed", value: values[0] as string }, nullable: false } : { spec: { kind: "choice", values: values as string[] }, nullable: false }
      }
      return { spec: { kind: "text" }, nullable: false }
    }
    case "integer": return { spec: { kind: "integer" }, nullable: false }
    case "number": {
      const norn = node["nornType"] as { kind?: unknown; scale?: unknown; unit?: { code?: unknown } } | undefined
      if (norn?.kind === "decimal") return { spec: { kind: "decimal", scale: typeof norn.scale === "number" ? norn.scale : null, unit: typeof norn.unit?.code === "string" ? norn.unit.code : null }, nullable: false }
      return { spec: { kind: "number" }, nullable: false }
    }
    case "boolean": return { spec: { kind: "boolean" }, nullable: false }
    case "object": return { spec: { kind: "object", fields: fieldsOf(node, pointer), groups: groupsOf(node) }, nullable: false }
    case "array": return { spec: { kind: "array", item: specOf(node["items"], `${pointer}/0`).spec }, nullable: false }
    default: return { spec: { kind: "unsupported", reason: `type ${String(type)}` }, nullable: false }
  }
}

const text = (value: unknown, max: number) => typeof value === "string" && value.length > 0 && value.length <= max ? value : null

/** `x-norn.groups` on an object node, as admitted; malformed entries are dropped, never repaired. */
export function groupsOf(schema: unknown): readonly Group[] {
  const groups = ((schema ?? {}) as { "x-norn"?: { groups?: unknown } })["x-norn"]?.groups
  if (!Array.isArray(groups)) return []
  return groups.flatMap((g: { id?: unknown; title?: unknown; fields?: unknown }) =>
    typeof g.id === "string" && typeof g.title === "string" && Array.isArray(g.fields) ? [{ id: g.id, title: g.title, fields: g.fields.map(String) }] : [])
}

/** The fields of a closed object schema, required first, each group in the order the schema lists them. */
export function fieldsOf(schema: unknown, base = ""): readonly FormField[] {
  const node = (schema ?? {}) as { properties?: Record<string, unknown>; required?: unknown }
  const required = new Set(Array.isArray(node.required) ? node.required.map(String) : [])
  const fields = Object.entries(node.properties ?? {}).map(([name, child]): FormField => {
    const pointer = `${base}/${escapePointer(name)}`
    const { spec, nullable } = specOf(child, pointer)
    const meta = (child ?? {}) as { title?: unknown; description?: unknown }
    return { name, label: text(meta.title, 120) ?? fieldLabel(name), description: text(meta.description, 2000), pointer, required: required.has(name), nullable, spec }
  })
  return [...fields.filter((f) => f.required), ...fields.filter((f) => !f.required)]
}

/** Whether every field can be offered; a form with an unsupported shape is shown read-only with the reason. */
export function unsupported(fields: readonly FormField[]): string | null {
  for (const field of fields) {
    if (field.spec.kind === "unsupported") return `${field.name}: ${field.spec.reason}`
    if (field.spec.kind === "object") { const inner = unsupported(field.spec.fields); if (inner) return `${field.name}.${inner}` }
    if (field.spec.kind === "array" && field.spec.item.kind === "unsupported") return `${field.name}: items ${field.spec.item.reason}`
  }
  return null
}

/** What was typed: text for leaves ("" = not given), `null` for an explicit no-value, lists and records for the rest. */
export type Draft = string | null | readonly Draft[] | { readonly [name: string]: Draft }

export const emptyDraft = (fields: readonly FormField[]): { readonly [name: string]: Draft } =>
  Object.fromEntries(fields.map((field) => [field.name, emptyFor(field.spec)]))

function emptyFor(spec: Spec): Draft {
  switch (spec.kind) {
    case "object": return emptyDraft(spec.fields)
    case "array": return []
    case "fixed": return spec.value
    default: return ""
  }
}

const NUMBER_TOKEN = /^-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?$/

function encodeLeaf(spec: Spec, value: string): string {
  switch (spec.kind) {
    case "integer":
    case "number":
    case "decimal":
      // A number token goes as typed, digit for digit; anything else goes as text for the cell to refuse at its pointer.
      return NUMBER_TOKEN.test(value.trim()) ? value.trim() : JSON.stringify(value)
    case "boolean": return value === "true" ? "true" : value === "false" ? "false" : JSON.stringify(value)
    case "json": try { JSON.parse(value); return value.trim() } catch { return JSON.stringify(value) }
    default: return JSON.stringify(value)
  }
}

function encodeValue(spec: Spec, draft: Draft): string | undefined {
  if (draft === null) return "null"
  if (spec.kind === "object") return encodeObject(spec.fields, draft as { readonly [name: string]: Draft })
  if (spec.kind === "array") {
    const items = (draft as readonly Draft[]).map((item) => encodeValue(spec.item, item) ?? "null")
    return `[${items.join(",")}]`
  }
  const text = draft as string
  return text === "" ? undefined : encodeLeaf(spec, text)
}

/** The draft as the `input` object's JSON text. Fields left empty are omitted; the cell says which it requires. */
export function encodeObject(fields: readonly FormField[], draft: { readonly [name: string]: Draft }): string {
  const members = fields.flatMap((field) => {
    // An explicit null ("No value") is sent; only an absent member falls back to empty.
    const value = encodeValue(field.spec, field.name in draft ? draft[field.name] as Draft : emptyFor(field.spec))
    return value === undefined ? [] : [`${JSON.stringify(field.name)}:${value}`]
  })
  return `{${members.join(",")}}`
}

/**
 * The input pointer a diagnostic names (`/input/lines/0/sku` → `/lines/0/sku`), or null when it
 * names no input field (a rule, a precondition, the key).
 */
export function inputPointer(diagnostic: Diagnostic): string | null {
  const pointer = diagnostic.pointer
  if (pointer === null || !(pointer === "/input" || pointer.startsWith("/input/"))) return null
  return pointer.slice("/input".length) || null
}

/** The diagnostics for one field: those at its pointer or below it. */
export const problemsAt = (diagnostics: readonly Diagnostic[], pointer: string): readonly Diagnostic[] =>
  diagnostics.filter((d) => { const at = inputPointer(d); return at !== null && (at === pointer || at.startsWith(`${pointer}/`)) })

/** Diagnostics that land on no field the form shows: they read at the top of the form. */
export function unplaced(diagnostics: readonly Diagnostic[], fields: readonly FormField[]): readonly Diagnostic[] {
  const pointers: string[] = []
  const walk = (list: readonly FormField[]) => { for (const f of list) { pointers.push(f.pointer); if (f.spec.kind === "object") walk(f.spec.fields) } }
  walk(fields)
  return diagnostics.filter((d) => {
    const at = inputPointer(d)
    return at === null || !pointers.some((p) => at === p || at.startsWith(`${p}/`))
  })
}

/** A field's id in words, sentence-cased: `cost_currency` → "Cost currency". Used only when the schema gives no title. */
export const fieldLabel = (name: string) => { const words = name.replaceAll("_", " "); return words.charAt(0).toUpperCase() + words.slice(1) }

/** "e.g. 12.500" for an exact decimal of scale 3: an example, never a value. */
export const decimalExample = (scale: number | null) => scale === null || scale === 0 ? "e.g. 12" : `e.g. 12.${"5".padEnd(scale, "0")}`

const IRREGULAR: Readonly<Record<string, string>> = { send: "sent", make: "made", pay: "paid", set: "set", put: "put", run: "ran", take: "taken", give: "given", get: "got", hold: "held", sell: "sold", buy: "bought", split: "split", write: "written", begin: "begun", shut: "shut", cut: "cut", reset: "reset" }

/** The past tense of an action's leading verb ("Open a holding" → "opened"), for the business sentence; null when it isn't a plain verb. */
export function pastTense(title: string): string | null {
  const verb = title.trim().split(/\s+/)[0]?.toLowerCase() ?? ""
  if (!/^[a-z]{2,}$/.test(verb)) return null
  if (IRREGULAR[verb]) return IRREGULAR[verb]
  if (verb.endsWith("e")) return `${verb}d`
  if (/[^aeiou]y$/.test(verb)) return `${verb.slice(0, -1)}ied`
  return `${verb}ed`
}

// ---- Sections (and wizard steps) from x-norn.groups ------------------------------------------

/** One section of a form: a group's fields in its order, or the untitled rest. */
export interface Section { readonly id: string | null; readonly title: string | null; readonly fields: readonly FormField[] }

const unescape = (segment: string) => segment.replaceAll("~1", "/").replaceAll("~0", "~")
export const segments = (pointer: string): readonly string[] => pointer === "" ? [] : pointer.slice(1).split("/").map(unescape)

function index(fields: readonly FormField[], into = new Map<string, FormField>()) {
  for (const field of fields) {
    into.set(field.pointer, field)
    if (field.spec.kind === "object") index(field.spec.fields, into)
  }
  return into
}

/**
 * The sections of an object's form. Each group is a section of the fields its pointers name (a
 * pointer may reach into a nested record: `/address/city`). The fields no group names come last,
 * untitled; a record with some grouped members is split, so each member appears exactly once.
 */
export function sectionsOf(fields: readonly FormField[], groups: readonly Group[], idField?: string): readonly Section[] {
  const known = index(fields)
  const named = groups.map((group) => ({ id: group.id, title: group.title, fields: group.fields.flatMap((p) => known.get(p) ?? []) }))
  const taken = new Set(named.flatMap((section) => section.fields.map((f) => f.pointer)))
  const rest: FormField[] = []
  const place = (list: readonly FormField[]) => {
    for (const field of list) {
      if (taken.has(field.pointer)) continue
      if (field.spec.kind === "object" && [...taken].some((p) => p.startsWith(`${field.pointer}/`))) place(field.spec.fields)
      else rest.push(field)
    }
  }
  place(fields)
  // The record's id leads the form when it is required and no group places it; declared groups are never regrouped.
  const id = idField === undefined ? undefined : rest.find((f) => f.pointer === `/${idField}` && f.required)
  const others = id === undefined ? rest : rest.filter((f) => f !== id)
  const sections: Section[] = named.filter((section) => section.fields.length > 0)
  if (others.length > 0) sections.push({ id: null, title: null, fields: others })
  if (id !== undefined) {
    const [first, ...tail] = sections
    return first === undefined ? [{ id: null, title: null, fields: [id] }] : [{ ...first, fields: [id, ...first.fields] }, ...tail]
  }
  return sections
}

/** The draft value at a pointer (relative to the draft's own object). */
export function draftAt(draft: Draft, pointer: string): Draft | undefined {
  let at: Draft | undefined = draft
  for (const segment of segments(pointer)) {
    if (at === null || typeof at !== "object") return undefined
    at = Array.isArray(at) ? (at as readonly Draft[])[Number(segment)] : (at as { readonly [k: string]: Draft })[segment]
  }
  return at
}

/** A copy of the draft with the value at `pointer` replaced. */
export function withDraftAt(draft: Draft, pointer: string, value: Draft): Draft {
  const [head, ...tail] = segments(pointer)
  if (head === undefined) return value
  const rest = tail.length === 0 ? "" : `/${tail.map((s) => s.replaceAll("~", "~0").replaceAll("/", "~1")).join("/")}`
  if (Array.isArray(draft)) {
    const list = [...(draft as readonly Draft[])]
    list[Number(head)] = withDraftAt(list[Number(head)] ?? "", rest, value)
    return list
  }
  const record = (draft !== null && typeof draft === "object" ? draft : {}) as { readonly [k: string]: Draft }
  return { ...record, [head]: withDraftAt(record[head] ?? "", rest, value) }
}

/** The label a field reads with in a section: a member pulled out of its record names the record too. */
export const sectionLabel = (all: readonly FormField[], field: FormField): string => {
  const parts = segments(field.pointer)
  if (parts.length < 2) return field.label
  const known = index(all)
  const parents = parts.slice(0, -1).map((_, i) => known.get(`/${parts.slice(0, i + 1).map((s) => s.replaceAll("~", "~0").replaceAll("/", "~1")).join("/")}`)?.label ?? "")
  return [...parents.filter(Boolean), field.label].join(" › ")
}
