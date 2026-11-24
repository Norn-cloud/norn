import canonicalize from "canonicalize"

/**
 * Tool bindings (#772): the organization's record that one gateway tool may be run from Norn, with
 * a label, fixed input values and the least role that may run it. This is all Norn stores about a
 * connector; credentials stay in the gateway. No binding, no call (default deny).
 */

export type RunRole = "member" | "admin"

/** A field the binding fixes; the person running it supplies every other field. */
export type InputMapping = Readonly<Record<string, { readonly fixed: unknown }>>

/** What a binding binds, as the canvas and the host admission identify it: never by name alone. */
export interface ConnectorRef {
  readonly gateway: "executor"
  /** The gateway origin the tool came from. */
  readonly origin: string
  readonly app: string
  readonly tool: string
  /**
   * The app version the tool was read from when the binding was last saved. A gateway that names
   * none gets `sha256-<first 12 hex of the manifest digest>`, so it is never empty.
   */
  readonly version: string
}

/** The version a connector carries: the gateway's, or one derived from the manifest digest. */
export const connectorVersion = (version: string | null, digest: string) => version ?? `sha256-${digest.replace(/^sha256:/, "").slice(0, 12)}`

/** `gateway:app/tool@version`, the connector's one-line identity. */
export const connectorString = (c: ConnectorRef) => `${c.gateway}:${c.app}/${c.tool}@${c.version}`

export interface Binding {
  readonly id: string
  readonly app: string
  readonly tool: string
  readonly label: string
  readonly inputMapping: InputMapping
  readonly runRole: RunRole
  readonly updatedBy: string
  readonly updatedAt: number
  /** `sha256:<hex>` of the tool manifest (see `toolManifest`) the binding was saved against. */
  readonly manifestDigest: string
  readonly connector: ConnectorRef
  /** The cell definition agents reference; null while this stage keeps bindings locally only. */
  readonly definition: { readonly id: string; readonly version: number } | null
  /**
   * `pending` while a save is between the cell and this row (or did not finish): such a binding
   * neither runs nor changes until reconciled against the cell. `ready` otherwise.
   */
  readonly state: "ready" | "pending"
  /** For a pending binding, which save did not finish: a create (undone by deleting) or an edit (undone by restoring). */
  readonly pendingSave: "create" | "edit" | null
  /** The definition id a pending save targets (the binding's own, studio.binding.<id>). */
  readonly pendingDefinition: string | null
  /** Unique attempt token guarding finish, abandon, and reconciliation writes against concurrent saves. */
  readonly pendingAttempt: string | null
  /** Cell environment the pending save targeted, so reconciliation reads the same environment. */
  readonly pendingEnvironmentId: string | null
  /** Epoch ms when the pending create or edit began, fencing active saves against premature abandon. */
  readonly pendingStartedAt: number | null
}

export interface BindingDraft {
  readonly app: string
  readonly tool: string
  readonly label: string
  readonly inputMapping: InputMapping
  readonly runRole: RunRole
}

interface Row { id: string; app: string; tool: string; label: string; input_mapping: string; run_role: RunRole; updated_by: string; updated_at: number; manifest_digest: string; connector: string; definition_id: string | null; definition_version: number | null; state: "ready" | "creating" | "editing"; pending_attempt: string | null; pending_environment_id: string | null; pending_started_at: number | null }
const fromRow = (row: Row): Binding => ({
  id: row.id, app: row.app, tool: row.tool, label: row.label, inputMapping: JSON.parse(row.input_mapping) as InputMapping,
  runRole: row.run_role, updatedBy: row.updated_by, updatedAt: row.updated_at,
  manifestDigest: row.manifest_digest, connector: JSON.parse(row.connector) as ConnectorRef,
  definition: row.definition_id === null || row.definition_version === null ? null : { id: row.definition_id, version: row.definition_version },
  state: row.state === "ready" ? "ready" : "pending",
  pendingSave: row.state === "creating" ? "create" : row.state === "editing" ? "edit" : null,
  pendingDefinition: row.state === "ready" ? null : row.definition_id,
  pendingAttempt: row.state === "ready" ? null : row.pending_attempt,
  pendingEnvironmentId: row.state === "ready" ? null : row.pending_environment_id,
  pendingStartedAt: row.state === "ready" ? null : row.pending_started_at ?? row.updated_at,
})
const COLUMNS = "id, app, tool, label, input_mapping, run_role, updated_by, updated_at, manifest_digest, connector, definition_id, definition_version, state, pending_attempt, pending_environment_id, pending_started_at"

export async function listBindings(db: D1Database, orgId: string): Promise<readonly Binding[]> {
  const { results } = await db.prepare(`SELECT ${COLUMNS} FROM connector_binding WHERE org_id = ? ORDER BY app, tool, label`).bind(orgId).all<Row>()
  return results.map(fromRow)
}

export async function getBinding(db: D1Database, orgId: string, id: string): Promise<Binding | null> {
  const { results } = await db.prepare(`SELECT ${COLUMNS} FROM connector_binding WHERE org_id = ? AND id = ?`).bind(orgId, id).all<Row>()
  const row = results[0]
  return row === undefined ? null : fromRow(row)
}

/** The manifest a binding is pinned to: the tool's identity and its input and output contract. */
export interface ToolManifest {
  readonly format: "norn.connector-tool-manifest/1"
  readonly gateway: "executor"
  readonly app: string
  readonly tool: string
  readonly inputSchema: unknown
  readonly outputSchema: unknown
}

export const toolManifest = (tool: { readonly app: string; readonly name: string; readonly inputSchema: unknown; readonly outputSchema: unknown }): ToolManifest =>
  ({ format: "norn.connector-tool-manifest/1", gateway: "executor", app: tool.app, tool: tool.name, inputSchema: tool.inputSchema ?? null, outputSchema: tool.outputSchema ?? null })

/**
 * RFC 8785 (JCS) canonical JSON: keys sorted by UTF-16 code units, ECMAScript number spelling,
 * no whitespace. The cell recomputes the digest the same way; the shared golden vectors are
 * tools/norn-ts/src/builder/fixtures/binding-manifest-digest.vectors.json.
 */
export function canonicalJson(value: unknown): string {
  const text = canonicalize(value)
  if (text === undefined) throw new Error("A manifest must be JSON")
  return text
}

/** `sha256:<hex>` of the manifest's canonical JSON. */
export async function manifestDigest(manifest: ToolManifest): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(canonicalJson(manifest))))
  return `sha256:${[...bytes].map((b) => b.toString(16).padStart(2, "0")).join("")}`
}

/** What the live tool contributes to a save: its manifest digest and connector reference. */
export interface Pin { readonly manifestDigest: string; readonly connector: ConnectorRef; readonly definition: Binding["definition"] }

/**
 * A save in two steps, so a failure between the cell and this row is never half-done:
 * `beginCreate` / `beginEdit` record the intent (state pending), the cell install runs, then
 * `finishSave` writes the result (state ready) or `abandonSave` undoes the intent. A pending row
 * left by a failure is settled by `adoptCell` or `abandonSave` when reconciled against the cell.
 */
export async function beginCreate(
  db: D1Database,
  orgId: string,
  actor: string,
  draft: BindingDraft,
  pin: Omit<Pin, "definition">,
  id: string,
  definitionId: string,
  attemptId: string,
  environmentId: string | null,
  now = Date.now(),
): Promise<void> {
  await db.prepare(
    "INSERT INTO connector_binding (id, org_id, app, tool, label, input_mapping, run_role, created_by, created_at, updated_by, updated_at, manifest_digest, connector, definition_id, definition_version, state, pending_attempt, pending_environment_id, pending_started_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, 'creating', ?, ?, ?)",
  ).bind(id, orgId, draft.app, draft.tool, draft.label, JSON.stringify(draft.inputMapping), draft.runRole, actor, now, actor, now, pin.manifestDigest, JSON.stringify(pin.connector), definitionId, attemptId, environmentId, now).run()
}

/** Mark an existing ready binding as being saved; returns false if another save already claimed it. */
export async function beginEdit(
  db: D1Database,
  orgId: string,
  id: string,
  definitionId: string,
  attemptId: string,
  environmentId: string | null,
  expectedVersion: number | null,
  now = Date.now(),
): Promise<boolean> {
  const { results } = await db.prepare(
    "UPDATE connector_binding SET state = 'editing', definition_id = ?, pending_attempt = ?, pending_environment_id = ?, pending_started_at = ? WHERE org_id = ? AND id = ? AND state = 'ready' AND definition_version IS ? RETURNING id",
  ).bind(definitionId, attemptId, environmentId, now, orgId, id, expectedVersion).all<{ id: string }>()
  return results.length > 0
}

/**
 * Commit a binding save only when the row still carries this exact pending attempt and prior
 * version. Returns `null` if the attempt was abandoned or superseded so callers never report
 * stale state as a completed save.
 */
export async function finishSave(
  db: D1Database,
  orgId: string,
  actor: string,
  id: string,
  draft: BindingDraft,
  pin: Pin,
  attemptId: string,
  expectedPreviousVersion: number | null,
  now = Date.now(),
): Promise<Binding | null> {
  const { results } = await db.prepare(
    `UPDATE connector_binding SET label = ?, input_mapping = ?, run_role = ?, updated_by = ?, updated_at = ?, manifest_digest = ?, connector = ?, definition_id = ?, definition_version = ?, state = 'ready', pending_attempt = NULL, pending_environment_id = NULL, pending_started_at = NULL WHERE org_id = ? AND id = ? AND pending_attempt = ? AND state IN ('creating', 'editing') AND definition_version IS ? RETURNING ${COLUMNS}`,
  ).bind(draft.label, JSON.stringify(draft.inputMapping), draft.runRole, actor, now, pin.manifestDigest, JSON.stringify(pin.connector), pin.definition?.id ?? null, pin.definition?.version ?? null, orgId, id, attemptId, expectedPreviousVersion).all<Row>()
  const row = results[0]
  return row === undefined ? null : fromRow(row)
}

/**
 * Undo a save's intent guarded by `attemptId` and expected version: a create that never reached the
 * cell is deleted; an edit returns to ready with its previous fields and definition.
 */
export async function abandonSave(
  db: D1Database,
  orgId: string,
  id: string,
  save: "create" | "edit",
  previous: Binding["definition"],
  attemptId: string | null,
): Promise<"deleted" | "restored" | "unchanged"> {
  if (save === "create") {
    const { results } = await db.prepare(
      "DELETE FROM connector_binding WHERE org_id = ? AND id = ? AND state = 'creating' AND pending_attempt IS ? RETURNING id",
    ).bind(orgId, id, attemptId).all<{ id: string }>()
    return results.length > 0 ? "deleted" : "unchanged"
  }
  const { results } = await db.prepare(
    "UPDATE connector_binding SET state = 'ready', definition_id = ?, pending_attempt = NULL, pending_environment_id = NULL, pending_started_at = NULL WHERE org_id = ? AND id = ? AND state = 'editing' AND pending_attempt IS ? AND definition_version IS ? RETURNING id",
  ).bind(previous?.id ?? null, orgId, id, attemptId, previous?.version ?? null).all<{ id: string }>()
  return results.length > 0 ? "restored" : "unchanged"
}

/**
 * Settle a pending binding from what the cell holds, guarded by `attemptId` and
 * `expectedPreviousVersion` so a newer completed save can never be overwritten by an earlier snapshot.
 */
export async function adoptCell(
  db: D1Database,
  orgId: string,
  id: string,
  definitionId: string,
  version: number,
  payload: Record<string, unknown>,
  attemptId: string | null,
  expectedPreviousVersion: number | null,
): Promise<boolean> {
  const label = typeof payload.label === "string" ? payload.label : null
  const mapping = typeof payload.inputMapping === "object" && payload.inputMapping !== null ? JSON.stringify(payload.inputMapping) : null
  const role = payload.runRole === "member" || payload.runRole === "admin" ? payload.runRole : null
  const digest = typeof payload.manifestDigest === "string" ? payload.manifestDigest : null
  const connector = typeof payload.connector === "object" && payload.connector !== null ? JSON.stringify(payload.connector) : null
  const { results } = await db.prepare(
    "UPDATE connector_binding SET label = COALESCE(?, label), input_mapping = COALESCE(?, input_mapping), run_role = COALESCE(?, run_role), manifest_digest = COALESCE(?, manifest_digest), connector = COALESCE(?, connector), definition_id = ?, definition_version = ?, state = 'ready', pending_attempt = NULL, pending_environment_id = NULL, pending_started_at = NULL WHERE org_id = ? AND id = ? AND pending_attempt IS ? AND state IN ('creating', 'editing') AND definition_version IS ? RETURNING id",
  ).bind(label, mapping, role, digest, connector, definitionId, version, orgId, id, attemptId, expectedPreviousVersion).all<{ id: string }>()
  return results.length > 0
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
  // No `type` (e.g. `{}`, anyOf, oneOf, $ref): nothing to check here; the gateway validates the whole input.
  if (schema.type === undefined) return true
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
    const property = Object.hasOwn(properties, field) ? properties[field] : undefined
    if (typeof property !== "object" || property === null) return { field: `inputMapping.${field}`, error: `${field} is not an input of ${app}.${tool}.` }
    if (typeof entry !== "object" || entry === null || !("fixed" in entry)) return { field: `inputMapping.${field}`, error: `${field} must be {fixed: value}.` }
    if (!fits(property, (entry as { fixed: unknown }).fixed)) return { field: `inputMapping.${field}`, error: `The fixed value of ${field} does not fit its declared type.` }
  }
  return { app, tool, label: label.trim(), inputMapping: mapping as InputMapping, runRole }
}

/** The call input: the person's values plus the binding's fixed ones; a person may not override a fixed field. */
export function runInput(binding: Binding, input: Record<string, unknown>): { readonly ok: true; readonly input: Record<string, unknown> } | { readonly ok: false; readonly problem: BindingProblem } {
  const overridden = Object.keys(input).find((field) => Object.hasOwn(binding.inputMapping, field))
  if (overridden !== undefined) return { ok: false, problem: { field: `input.${overridden}`, error: `${overridden} is fixed by this binding.` } }
  return { ok: true, input: { ...input, ...Object.fromEntries(Object.entries(binding.inputMapping).map(([field, { fixed }]) => [field, fixed])) } }
}
