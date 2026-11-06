/**
 * Tool bindings (#772): the organization's record that one gateway tool may be run from Norn, with
 * a label, fixed input values and the least role that may run it. This is all Norn stores about a
 * connector; credentials stay in the gateway. No binding, no call (default deny).
 */

export type RunRole = "member" | "admin"

/** A field the binding fixes; the person running it supplies every other field. */
export type InputMapping = Readonly<Record<string, { readonly fixed: unknown }>>

export interface Binding {
  readonly id: string
  readonly app: string
  readonly tool: string
  readonly label: string
  readonly inputMapping: InputMapping
  readonly runRole: RunRole
  readonly updatedBy: string
  readonly updatedAt: number
}

export interface BindingDraft {
  readonly app: string
  readonly tool: string
  readonly label: string
  readonly inputMapping: InputMapping
  readonly runRole: RunRole
}

interface Row { id: string; app: string; tool: string; label: string; input_mapping: string; run_role: RunRole; updated_by: string; updated_at: number }
const fromRow = (row: Row): Binding => ({
  id: row.id, app: row.app, tool: row.tool, label: row.label, inputMapping: JSON.parse(row.input_mapping) as InputMapping,
  runRole: row.run_role, updatedBy: row.updated_by, updatedAt: row.updated_at,
})
const COLUMNS = "id, app, tool, label, input_mapping, run_role, updated_by, updated_at"

export async function listBindings(db: D1Database, orgId: string): Promise<readonly Binding[]> {
  const { results } = await db.prepare(`SELECT ${COLUMNS} FROM connector_binding WHERE org_id = ? ORDER BY app, tool, label`).bind(orgId).all<Row>()
  return results.map(fromRow)
}

export async function getBinding(db: D1Database, orgId: string, id: string): Promise<Binding | null> {
  const { results } = await db.prepare(`SELECT ${COLUMNS} FROM connector_binding WHERE org_id = ? AND id = ?`).bind(orgId, id).all<Row>()
  const row = results[0]
  return row === undefined ? null : fromRow(row)
}

export async function saveBinding(db: D1Database, orgId: string, actor: string, draft: BindingDraft, id: string = crypto.randomUUID()): Promise<Binding> {
  const now = Date.now()
  await db.prepare(
    "INSERT INTO connector_binding (id, org_id, app, tool, label, input_mapping, run_role, created_by, created_at, updated_by, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) " +
    "ON CONFLICT (id) DO UPDATE SET label = excluded.label, input_mapping = excluded.input_mapping, run_role = excluded.run_role, updated_by = excluded.updated_by, updated_at = excluded.updated_at",
  ).bind(id, orgId, draft.app, draft.tool, draft.label, JSON.stringify(draft.inputMapping), draft.runRole, actor, now, actor, now).run()
  return { id, ...draft, updatedBy: actor, updatedAt: now }
}

export async function removeBinding(db: D1Database, orgId: string, id: string): Promise<boolean> {
  const before = await getBinding(db, orgId, id)
  if (before === null) return false
  await db.prepare("DELETE FROM connector_binding WHERE org_id = ? AND id = ?").bind(orgId, id).run()
  return true
}

const NAME = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,199}$/

/** A refusal naming the field it is about, for `connectors.binding_invalid`. */
export interface BindingProblem { readonly field: string; readonly error: string }

const jsonType = (value: unknown) => (value === null ? "null" : Array.isArray(value) ? "array" : Number.isInteger(value) ? "integer" : typeof value)

/** Whether a fixed value fits a property's declared JSON type or string enum (the gateway checks the whole input again). */
function fits(schema: Record<string, unknown>, value: unknown): boolean {
  if (Array.isArray(schema.enum)) return schema.enum.includes(value)
  const types = Array.isArray(schema.type) ? schema.type : [schema.type]
  const actual = jsonType(value)
  return types.some((type) => type === actual || (type === "number" && actual === "integer"))
}

/**
 * Read a binding draft from a request body and check its mapping against the tool's input schema:
 * every fixed field must be a declared property, and its value must fit that property's type.
 */
export function readDraft(body: unknown, inputSchema: unknown, fallback?: Binding): BindingDraft | BindingProblem {
  const raw = (typeof body === "object" && body !== null ? body : {}) as Record<string, unknown>
  const app = fallback?.app ?? raw.app
  const tool = fallback?.tool ?? raw.tool
  if (typeof app !== "string" || !NAME.test(app)) return { field: "app", error: "app must be a gateway app id." }
  if (typeof tool !== "string" || !NAME.test(tool)) return { field: "tool", error: "tool must be a tool name." }
  const label = raw.label ?? fallback?.label ?? tool
  if (typeof label !== "string" || label.trim() === "" || label.length > 120) return { field: "label", error: "label must be 1–120 characters." }
  const runRole = raw.runRole ?? fallback?.runRole ?? "member"
  if (runRole !== "member" && runRole !== "admin") return { field: "runRole", error: "runRole must be member or admin." }
  const mapping = raw.inputMapping ?? fallback?.inputMapping ?? {}
  if (typeof mapping !== "object" || mapping === null || Array.isArray(mapping)) return { field: "inputMapping", error: "inputMapping must be {field: {fixed: value}}." }
  const properties = ((inputSchema as { properties?: unknown } | null)?.properties ?? {}) as Record<string, Record<string, unknown>>
  for (const [field, entry] of Object.entries(mapping)) {
    const property = properties[field]
    if (property === undefined) return { field: `inputMapping.${field}`, error: `${field} is not an input of ${app}.${tool}.` }
    if (typeof entry !== "object" || entry === null || !("fixed" in entry)) return { field: `inputMapping.${field}`, error: `${field} must be {fixed: value}.` }
    if (!fits(property, (entry as { fixed: unknown }).fixed)) return { field: `inputMapping.${field}`, error: `The fixed value of ${field} does not fit its declared type.` }
  }
  return { app, tool, label: label.trim(), inputMapping: mapping as InputMapping, runRole }
}

/** The call input: the person's values plus the binding's fixed ones; a person may not override a fixed field. */
export function runInput(binding: Binding, input: Record<string, unknown>): { readonly ok: true; readonly input: Record<string, unknown> } | { readonly ok: false; readonly problem: BindingProblem } {
  const overridden = Object.keys(input).find((field) => field in binding.inputMapping)
  if (overridden !== undefined) return { ok: false, problem: { field: `input.${overridden}`, error: `${overridden} is fixed by this binding.` } }
  return { ok: true, input: { ...input, ...Object.fromEntries(Object.entries(binding.inputMapping).map(([field, { fixed }]) => [field, fixed])) } }
}
