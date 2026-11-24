import { CellUnavailable, getDisplayedScope, readStudioScope, scopedHostFetch, type StudioScope } from "./definitions.ts"

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
 *   POST /v1/actions/{actionId}?orgId&environmentId[&version]   (served: norn_api::actions)
 *        body { input: <object>, idempotencyKey: string }   (1..128 chars and at most 256 UTF-8
 *        bytes; Studio sends one random UUID per draft)
 *        200 { outcome: "committed" | "replayed", objectType, objectId, version,
 *              receipt: { receiptHash, commitSeq, recordedAtMs } }: a replay reports the
 *              original commit; `receiptHash` is a keyed handle unless the reader holds Values
 *              over the whole cell.
 *        4xx Refusal@2 { code, category, message, explanation, diagnostics?: [{ code, message,
 *            span: { pointer } | null }] }: each pointer into the request body. Only a malformed
 *            envelope carries one today (`/input` not an object, `/idempotencyKey`). Input the
 *            action's schema refuses is one 400 `action.invalid_input` with no field pointers, so
 *            Studio shows it on the form, not on a field; a rule, law or precondition refusal
 *            carries none either. A diagnostic at `/input/<field>` (nested: `/input/lines/0/sku`)
 *            reads on that field's row; one at `/input` itself, or at no pointer, reads on the form.
 *        202 { outcome: "pending", decision: { id, expiresAtMs } }: an approval-gated action
 *            (APPROVAL-1); nothing is committed until a decider approves it in the inbox.
 *        409 `action.idempotency_conflict` when the key was used for a different call.
 *        Fixture: test/fixtures/action-call.json, produced by crates/norn-cell-do/tests/actions.rs.
 *
 *   A visibility or permission refusal is the one opaque 403 `authz.denied`, whatever the target or
 *   action, so that refusal never tells a known action from an unknown one. The guarantee is not
 *   universal: a caller the host lets call any action and that pins a stale `version` gets 409
 *   `defs.stale` for a known action but 403 `authz.denied` for an unknown one, until action-level
 *   host authorization closes that gap.
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
  /** Input fields that name an existing record of `objectKind`: the form offers a picker (ACTION-CATALOG-LINKS-1). */
  readonly inputLinks?: readonly { readonly field: string; readonly objectKind: string }[]
  /** A transition's target state, so the receipt says what the record is now. */
  readonly toState?: string
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

/** An approval-gated call (APPROVAL-1): 202, nothing committed yet; a decider settles it in the inbox. */
export interface PendingDecision { readonly id: string; readonly expiresAtMs: number }

export type CallResult =
  | { readonly kind: "receipt"; readonly receipt: ActionReceipt }
  | { readonly kind: "pending"; readonly decision: PendingDecision }
  | { readonly kind: "refused"; readonly status: number; readonly code: string; readonly message: string; readonly diagnostics: readonly Diagnostic[]; readonly explanation?: string; readonly reason?: RefusalReason }
  | { readonly kind: "no-cell" }
  | { readonly kind: "unavailable" }

export const ACTIONS_UNAVAILABLE = "This cell doesn't take actions from Studio yet."

type Fetch = (input: string, init?: RequestInit) => Promise<Response>
const defaultFetch: Fetch = (input, init) => fetch(input, init)

/**
 * DIAG-REASON-1: what refused a call, disclosed with the caller's authority: the law (its pinned
 * declaration), the subject, and the stored values it checked, by path (`total`, `ledger_entry.amount`).
 */
export interface RefusalReason {
  readonly law: string
  readonly subject: string
  readonly values: Readonly<Record<string, unknown>>
  readonly pin?: { readonly kind?: string; readonly artifactId?: string; readonly lawId?: string; readonly version?: string }
}

interface Refusal { readonly code: string; readonly message: string; readonly diagnostics: readonly Diagnostic[]; readonly explanation?: string; readonly reason?: RefusalReason }

function refusalOf(text: string): Refusal | null {
  let body: unknown
  try { body = JSON.parse(text) } catch { return null }
  const { code, message, diagnostics, explanation, reason } = (body ?? {}) as { code?: unknown; message?: unknown; diagnostics?: unknown; explanation?: unknown; reason?: unknown }
  if (typeof code !== "string") return null
  // The cell's refusal@2 places a finding at `span.pointer`; a bare `pointer` is read too.
  const list = Array.isArray(diagnostics) ? diagnostics.map((d: { code?: unknown; message?: unknown; pointer?: unknown; span?: { pointer?: unknown } | null }) => ({
    code: String(d.code ?? ""), message: String(d.message ?? ""),
    pointer: typeof d.span?.pointer === "string" ? d.span.pointer : typeof d.pointer === "string" ? d.pointer : null,
  })) : []
  // The cell's own explanation names what refused the call (a law and its statement, say); it is shown as sent.
  const disclosed = reason !== null && typeof reason === "object" && typeof (reason as { law?: unknown }).law === "string" ? reason as RefusalReason : undefined
  return { code, message: typeof message === "string" ? message : code, diagnostics: list, ...typeof explanation === "string" ? { explanation } : {}, ...disclosed ? { reason: disclosed } : {} }
}

async function scoped(fetcher: Fetch) {
  const scope = await readStudioScope(fetcher)
  return scope === null ? null : scopedHostFetch(fetcher, "/api/host", async () => scope)
}

/**
 * The catalog for `objectKind`; with `objectId`, only what can act on that record in its current
 * state (ACTION-CATALOG-STATE-1). The cell decides the list; Studio filters nothing.
 */
export async function listActions(objectKind: string | undefined, fetcher: Fetch = defaultFetch, objectId?: string): Promise<CatalogRead> {
  let call
  try { call = await scoped(fetcher) } catch (error) { if (error instanceof CellUnavailable) return { kind: "unavailable" }; throw error }
  if (call === null) return { kind: "no-cell" }
  const query = objectKind === undefined ? "" : `?${new URLSearchParams({ objectKind, ...objectId === undefined ? {} : { objectId } })}`
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

/**
 * Send one call. `input` is already-encoded JSON text, so exact numbers reach the cell as typed.
 * Bound to `boundScope` (the workspace the form was opened in) or the tab's displayed scope rather
 * than re-reading `/api/me`, so switching workspaces in another tab cannot redirect a stale form.
 */
export async function callAction(
  actionId: string,
  input: string,
  idempotencyKey: string,
  fetcher: Fetch = defaultFetch,
  boundScope?: StudioScope | null,
): Promise<CallResult> {
  const scope = boundScope !== undefined
    ? boundScope
    : fetcher === defaultFetch && getDisplayedScope() !== undefined
      ? getDisplayedScope()!
      : await readStudioScope(fetcher)
  if (scope === null) return { kind: "no-cell" }
  const call = scopedHostFetch(fetcher, "/api/host", async () => scope)
  const response = await call(`/v1/actions/${encodeURIComponent(actionId)}`, {
    method: "POST",
    headers: { accept: "application/json", "content-type": "application/json" },
    body: `{"input":${input},"idempotencyKey":${JSON.stringify(idempotencyKey)}}`,
  })
  const text = await response.text()
  if (response.status === 202) {
    const pending = JSON.parse(text) as { readonly outcome?: unknown; readonly decision?: PendingDecision }
    if (pending.outcome === "pending" && pending.decision !== undefined) return { kind: "pending", decision: pending.decision }
    throw new CellUnavailable(response.status)
  }
  if (response.ok) return { kind: "receipt", receipt: JSON.parse(text) as ActionReceipt }
  const refusal = refusalOf(text)
  if (response.status === 404 || response.status === 503) return { kind: "unavailable" }
  if (refusal !== null) return { kind: "refused", status: response.status, ...refusal }
  throw new CellUnavailable(response.status)
}

// ---- The form a closed input schema implies -------------------------------------------------

export type Spec =
  | { readonly kind: "text" }
  /**
   * A string input the schema gives a JSON Schema `format`: a calendar date (`YYYY-MM-DD`), an
   * instant (RFC 3339 with an offset) or a time of day (`HH:MM:SS`). The form offers a picker and
   * sends the picker's value as that text; the cell decodes and decides it.
   */
  | { readonly kind: "date" }
  | { readonly kind: "date-time" }
  | { readonly kind: "time" }
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
  /** `itemTitle`: the items schema's own `title`, naming one entry ("Lot"). */
  | { readonly kind: "array"; readonly item: Spec; readonly itemTitle: string | null }
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
    if (names.includes("null") && inner.length === 1) return { spec: specOf({ ...node, type: inner[0] }, pointer).spec, nullable: true }
    return { spec: { kind: "unsupported", reason: `type union ${names.join(" | ")}` }, nullable: false }
  }
  switch (type) {
    case "string": {
      const values = node["enum"]
      if (Array.isArray(values) && values.length > 0 && values.every((v) => typeof v === "string")) {
        return values.length === 1 ? { spec: { kind: "fixed", value: values[0] as string }, nullable: false } : { spec: { kind: "choice", values: values as string[] }, nullable: false }
      }
      const format = node["format"]
      if (format === "date" || format === "date-time" || format === "time") return { spec: { kind: format }, nullable: false }
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
    case "array": return { spec: { kind: "array", item: specOf(node["items"], `${pointer}/0`).spec, itemTitle: text((node["items"] as { title?: unknown } | undefined)?.title, 120) }, nullable: false }
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
    if (field.spec.kind === "array") {
      const item = field.spec.item
      if (item.kind === "unsupported") return `${field.name}: items ${item.reason}`
      if (item.kind === "object") { const inner = unsupported(item.fields); if (inner) return `${field.name}[].${inner}` }
    }
  }
  return null
}

/** What was typed: text for leaves ("" = not given), `null` for an explicit no-value, lists and records for the rest. */
export type Draft = string | null | readonly Draft[] | { readonly [name: string]: Draft }

export const emptyDraft = (fields: readonly FormField[]): { readonly [name: string]: Draft } =>
  Object.fromEntries(fields.map((field) => [field.name, emptyFor(field.spec)]))

/** The blank draft of a field of this kind: a record of blanks, an empty list, the fixed value, or empty text. */
export function emptyFor(spec: Spec): Draft {
  switch (spec.kind) {
    case "object": return emptyDraft(spec.fields)
    case "array": return []
    case "fixed": return spec.value
    default: return ""
  }
}

/** `+02:00` for 120 minutes east of UTC, `-05:30` for 330 west; `Z` for UTC. */
export function offsetText(minutesEast: number): string {
  if (minutesEast === 0) return "Z"
  const sign = minutesEast > 0 ? "+" : "-"
  const abs = Math.abs(minutesEast)
  return `${sign}${String(Math.floor(abs / 60)).padStart(2, "0")}:${String(abs % 60).padStart(2, "0")}`
}

const LOCAL_DATE_TIME = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2})(\.\d+)?)?$/
const RFC3339_OFFSET = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(\.\d+)?(Z|[+-]\d{2}:\d{2})$/

/** Wall-clock parts of a local date-time (`month` is 1..12). */
export interface WallParts {
  readonly year: number
  readonly month: number
  readonly day: number
  readonly hour: number
  readonly minute: number
  readonly second: number
}

/**
 * Resolves a local wall time in one time zone into every valid UTC offset (in minutes east of UTC)
 * that displays that exact wall time:
 * - `[]` for a spring-forward DST gap (that wall time does not exist in the zone),
 * - `[offset]` for an unambiguous wall time,
 * - `[earlierOffset, laterOffset]` for a fall-back DST repeat (chronological order).
 */
export type LocalWallOffsets = (parts: WallParts) => readonly number[]

const sameWall = (at: Date, p: WallParts): boolean =>
  at.getFullYear() === p.year
  && at.getMonth() === p.month - 1
  && at.getDate() === p.day
  && at.getHours() === p.hour
  && at.getMinutes() === p.minute
  && at.getSeconds() === p.second

/** The runtime's local time zone resolver, detecting both spring-forward gaps and fall-back repeats. */
export function runtimeWallOffsets(parts: WallParts): readonly number[] {
  const wall = new Date(parts.year, parts.month - 1, parts.day, parts.hour, parts.minute, parts.second)
  if (!sameWall(wall, parts)) return []
  const matches = new Map<number, number>()
  const add = (at: Date) => {
    if (!sameWall(at, parts)) return
    const offset = -at.getTimezoneOffset()
    if (!matches.has(offset)) matches.set(offset, at.getTime())
  }
  add(wall)
  // Probe half-hour steps across ±3 hours around `wall` to discover sibling occurrences on any DST
  // fall-back transition (including 30m shifts like Australia/Lord_Howe and 2h shifts like Antarctica/Troll).
  for (let half = -6; half <= 6; half += 1) {
    if (half !== 0) add(new Date(wall.getTime() + half * 1_800_000))
  }
  return [...matches.entries()].sort(([, ta], [, tb]) => ta - tb).map(([offset]) => offset)
}

/**
 * Match `sent` (which may carry `Z` or any equivalent offset) against `alternatives` by epoch
 * instant, so a restored instant like `2026-11-01T06:30:00Z` in `America/New_York` selects the
 * second occurrence (`2026-11-01T01:30:00-05:00`) rather than falling back to the first.
 */
export function selectedAlternative(sent: string | null, alternatives: readonly string[]): string {
  if (sent !== null) {
    const at = Date.parse(sent)
    if (!Number.isNaN(at)) {
      const match = alternatives.find((iso) => Date.parse(iso) === at)
      if (match !== undefined) return match
    }
  }
  return alternatives[0] ?? ""
}

const pad2 = (n: number) => String(n).padStart(2, "0")
const formatWall = (p: WallParts, offsetMinutes: number, fraction = "") =>
  `${String(p.year).padStart(4, "0")}-${pad2(p.month)}-${pad2(p.day)}T${pad2(p.hour)}:${pad2(p.minute)}:${pad2(p.second)}${fraction}${offsetText(offsetMinutes)}`

/** Extract the trailing RFC 3339 offset (`Z` or `±HH:MM`) regardless of fractional seconds. */
export const offsetSuffix = (iso: string): string => /(Z|[+-]\d{2}:\d{2})$/.exec(iso)?.[1] ?? iso.slice(19)

export type DateTimeResolution =
  | { readonly kind: "ok"; readonly text: string; readonly alternatives: readonly string[] }
  | { readonly kind: "nonexistent"; readonly text: string }
  | { readonly kind: "passthrough"; readonly text: string }

/**
 * Resolve a draft date-time value to what will be sent, detecting DST gaps and repeats while
 * preserving any sub-second fraction (e.g. `.250`).
 * - A zoneless picker value (`YYYY-MM-DDTHH:MM[:SS[.fff]]`): resolves against `wallOffsets`
 *   (default: the reader's local zone). A spring-forward gap returns `nonexistent` (leaving `text`
 *   zoneless rather than silently shifting an hour); a fall-back repeat returns both valid RFC 3339
 *   instants in `alternatives`.
 * - Text that already carries an RFC 3339 offset (a prefilled/restored instant, or an explicitly
 *   disambiguated fall-back instant): preserves its exact offset and fractional seconds in `text`
 *   and in `alternatives` so epoch comparison and occurrence switching never lose precision.
 */
export function resolveDateTime(value: string, wallOffsets: LocalWallOffsets = runtimeWallOffsets): DateTimeResolution {
  const trimmed = value.trim()
  const local = LOCAL_DATE_TIME.exec(trimmed)
  if (local !== null) {
    const parts: WallParts = {
      year: Number(local[1]), month: Number(local[2]), day: Number(local[3]),
      hour: Number(local[4]), minute: Number(local[5]), second: Number(local[6] ?? "0"),
    }
    const fraction = local[7] ?? ""
    const offsets = wallOffsets(parts)
    if (offsets.length === 0) return { kind: "nonexistent", text: value }
    const alternatives = offsets.map((offset) => formatWall(parts, offset, fraction))
    return { kind: "ok", text: alternatives[0]!, alternatives }
  }
  const zoned = RFC3339_OFFSET.exec(trimmed)
  if (zoned !== null) {
    const at = Date.parse(trimmed)
    if (!Number.isNaN(at)) {
      const asLocal = new Date(at)
      const parts: WallParts = {
        year: asLocal.getFullYear(), month: asLocal.getMonth() + 1, day: asLocal.getDate(),
        hour: asLocal.getHours(), minute: asLocal.getMinutes(), second: asLocal.getSeconds(),
      }
      const fraction = zoned[7] ?? ""
      const offsets = wallOffsets(parts)
      const alternatives = offsets.map((offset) => formatWall(parts, offset, fraction))
      return { kind: "ok", text: trimmed, alternatives }
    }
  }
  return { kind: "passthrough", text: value }
}

/**
 * The RFC 3339 text for what a date-time picker holds. A picker gives a wall time with no zone
 * (`2026-11-23T09:30`); it is sent with the offset the reader's own zone has at that wall time
 * (`2026-11-23T09:30:00+02:00`), so the instant is the one the reader meant. A nonexistent local
 * time in a spring DST gap stays as typed (never shifted to another hour) so the cell refuses it;
 * text that already carries an offset (a prefilled value) is sent as it is. `offsetAt` is the
 * zone's offset east of UTC, in minutes (for deterministic tests).
 */
export function instantText(value: string, offsetAt?: (local: Date) => number): string {
  if (offsetAt === undefined) return resolveDateTime(value).text
  const local = LOCAL_DATE_TIME.exec(value.trim())
  if (local === null) return value
  const [, y, mo, d, h, mi, sec = "00", frac = ""] = local
  const wall = new Date(Number(y), Number(mo) - 1, Number(d), Number(h), Number(mi), Number(sec))
  return `${y}-${mo}-${d}T${h}:${mi}:${sec}${frac}${offsetText(offsetAt(wall))}`
}

/** What a date-time picker shows for a draft value: a zoneless wall time as is; an instant in the reader's zone. */
export function pickerDateTime(value: string): string {
  if (value === "" || LOCAL_DATE_TIME.test(value)) return value
  const at = Date.parse(value)
  if (Number.isNaN(at)) return ""
  const local = new Date(at)
  const two = (n: number) => String(n).padStart(2, "0")
  return `${local.getFullYear()}-${two(local.getMonth() + 1)}-${two(local.getDate())}T${two(local.getHours())}:${two(local.getMinutes())}${local.getSeconds() === 0 ? "" : `:${two(local.getSeconds())}`}`
}

const NUMBER_TOKEN = /^-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?$/

function encodeLeaf(spec: Spec, value: string): string {
  switch (spec.kind) {
    case "integer":
    case "number":
      // A number token goes as typed, digit for digit; anything else goes as text for the cell to refuse at its pointer.
      return NUMBER_TOKEN.test(value.trim()) ? value.trim() : JSON.stringify(value)
    case "decimal":
      // VAL-EXACT-1: the cell decodes an exact decimal only from its text, digit for digit ("12.500").
      return JSON.stringify(value.trim())
    case "boolean": return value === "true" ? "true" : value === "false" ? "false" : JSON.stringify(value)
    case "json": try { JSON.parse(value); return value.trim() } catch { return JSON.stringify(value) }
    case "date-time": return JSON.stringify(instantText(value))
    case "time": return JSON.stringify(/^\d{2}:\d{2}$/.test(value) ? `${value}:00` : value)
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

/**
 * The members of an encoded `input` object, each as the text that was sent: a string as its text, any
 * other value (a number, `null`, a list, an object) as its JSON, byte for byte, so no digit is
 * rounded. Null when the text is not one JSON object.
 */
export function sentMembers(input: string): readonly { readonly name: string; readonly text: string }[] | null {
  let parsed: unknown
  try { parsed = JSON.parse(input) } catch { return null }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return null
  const members: { name: string; text: string }[] = []
  let at = 0
  const space = () => { while (at < input.length && " \t\n\r".includes(input[at]!)) at++ }
  // One JSON value's extent from `at`: a string, a nested list or object, or a bare token.
  const value = (): string => {
    const start = at
    if (input[at] === "\"") { string(); return input.slice(start, at) }
    if (input[at] === "{" || input[at] === "[") {
      let depth = 0
      do {
        const char = input[at]
        if (char === "\"") { string(); continue }
        if (char === "{" || char === "[") depth++
        else if (char === "}" || char === "]") depth--
        at++
      } while (depth > 0 && at < input.length)
      return input.slice(start, at)
    }
    while (at < input.length && !",}] \t\n\r".includes(input[at]!)) at++
    return input.slice(start, at)
  }
  const string = () => { at++; while (at < input.length && input[at] !== "\"") at += input[at] === "\\" ? 2 : 1; at++ }
  space(); at++ // {
  for (;;) {
    space()
    if (input[at] === "}") break
    const name = JSON.parse(value()) as string
    space(); at++ // :
    space()
    const raw = value()
    members.push({ name, text: raw.startsWith("\"") ? JSON.parse(raw) as string : raw })
    space()
    if (input[at] === ",") at++
  }
  return members
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

/** Fields of a record item re-pointed from the schema's item `0` to the item they edit. */
export const rebaseFields = (fields: readonly FormField[], from: string, to: string): readonly FormField[] =>
  fields.map((f) => ({ ...f, pointer: to + f.pointer.slice(from.length), spec: f.spec.kind === "object" ? { ...f.spec, fields: rebaseFields(f.spec.fields, from, to) } : f.spec }))

const ARRAY_INDEX = /^(0|[1-9]\d*)$/

/**
 * Whether an input pointer (`inputPointer(d)`) is actually rendered by a `FieldRow` in `fields`.
 * Mirrors `FieldRow`: an object (or array of objects) only renders diagnostics at its own exact
 * pointer, while its declared children render theirs; leaf fields (and arrays of primitives) render
 * any pointer at or below them. Unmatched nested pointers (e.g. `/location/unknown` or an
 * out-of-range array element) return `false` so they surface in the form-level refusal alert.
 */
export function isDiagnosticPlaced(at: string, fields: readonly FormField[], draft?: Draft): boolean {
  for (const field of fields) {
    if (at === field.pointer) return true
    if (field.spec.kind === "object") {
      // Nullable hides an object's child FieldRows when its value is explicitly null.
      if (draft !== undefined && draftAt(draft, field.pointer) === null) continue
      if (at.startsWith(`${field.pointer}/`) && isDiagnosticPlaced(at, field.spec.fields, draft)) return true
      continue
    }
    if (field.spec.kind === "array" && field.spec.item.kind === "object") {
      if (!at.startsWith(`${field.pointer}/`)) continue
      const rest = at.slice(field.pointer.length + 1)
      const slash = rest.indexOf("/")
      if (slash < 0) continue // array item object itself has no FieldRow; only its child fields do
      const indexText = rest.slice(0, slash)
      if (!ARRAY_INDEX.test(indexText)) continue
      if (draft !== undefined) {
        const items = draftAt(draft, field.pointer)
        if (!Array.isArray(items) || Number(indexText) >= items.length) continue
      }
      const rebased = rebaseFields(field.spec.item.fields, `${field.pointer}/0`, `${field.pointer}/${indexText}`)
      if (isDiagnosticPlaced(at, rebased, draft)) return true
      continue
    }
    if (at.startsWith(`${field.pointer}/`)) return true
  }
  return false
}

/** Diagnostics that land on no field the form shows: they read at the top of the form. */
export function unplaced(diagnostics: readonly Diagnostic[], fields: readonly FormField[], draft?: Draft): readonly Diagnostic[] {
  return diagnostics.filter((d) => {
    const at = inputPointer(d)
    return at === null || !isDiagnosticPlaced(at, fields, draft)
  })
}

/** A field's id in words, sentence-cased: `cost_currency` → "Cost currency". Used only when the schema gives no title. */
export const fieldLabel = (name: string) => { const words = name.replaceAll("_", " "); return words.charAt(0).toUpperCase() + words.slice(1) }

/** One entry of a list, in words: the items' own title, else the list's label made singular ("Lots" → "lot"). */
export function itemNoun(itemTitle: string | null, listLabel: string): string {
  if (itemTitle !== null) return itemTitle.toLowerCase()
  const label = listLabel.toLowerCase()
  if (label.endsWith("ies")) return `${label.slice(0, -3)}y`
  if (/(ss|us|is)$/.test(label)) return label
  if (label.endsWith("es") && /(sh|ch|x|z)es$/.test(label)) return label.slice(0, -2)
  return label.endsWith("s") ? label.slice(0, -1) : label
}

/** "e.g. 12.500" for an exact decimal of scale 3: an example, never a value. */
export const decimalExample = (scale: number | null) => scale === null || scale === 0 ? "e.g. 12" : `e.g. 12.${"5".padEnd(scale, "0")}`

const IRREGULAR: Readonly<Record<string, string>> = { send: "sent", make: "made", pay: "paid", set: "set", put: "put", run: "ran", take: "taken", give: "given", get: "got", hold: "held", sell: "sold", buy: "bought", split: "split", write: "written", begin: "begun", shut: "shut", cut: "cut", reset: "reset" }

/** The past tense of an action's leading verb ("Open a holding" → "opened"), for the business sentence; null when it isn't a plain verb. */
/**
 * What the record is after a committed call, from the cell's own answer: a transition's target
 * state ("is now submitted"), a first version ("created") or a later one ("updated"). Never a
 * verb guessed from the action's title.
 */
export function receiptVerb(action: ActionSummary, version: number): string {
  if (action.effect === "fire_transition") return action.toState === undefined ? "updated" : `is now ${action.toState.replaceAll("_", " ")}`
  return version === 1 ? "created" : "updated"
}

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
