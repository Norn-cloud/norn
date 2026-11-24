import type { FormField } from "./actions.ts"
import type { Field, Scalar } from "./catalog.ts"
import type { Fetch } from "./cell-read.ts"
import { CellUnavailable, readStudioScope, scopedHostFetch, type Finding, type StudioScope } from "./definitions.ts"
import type { Risk } from "./decisions.ts"

/** Trigger condition for a server-driven automation (`AUTOMATION-1`). */
export type AutomationWhen =
  | { readonly kind: "event"; readonly event: "created" | "updated" | "action"; readonly actionId?: string }
  | { readonly kind: "lifecycle_entered"; readonly property: string; readonly state: string }
  | { readonly kind: "stale"; readonly afterMs: number; readonly property?: string; readonly state?: string }

/** Optional ZEN guard expression checked by `norn-checker` against `self` of `objectType`. */
export interface AutomationGuard {
  readonly expression: string
}

export interface AutomationDecider {
  readonly roles: readonly string[]
  readonly law?: string
}

/** Action taken when an automation triggers and its guard holds. */
export type AutomationThen =
  | {
      readonly kind: "propose"
      readonly actionId: string
      readonly input?: Readonly<Record<string, unknown>>
      readonly question: string
      readonly risk: Risk
      readonly decider: AutomationDecider
      readonly expiresInMs?: number
    }
  | {
      readonly kind: "run"
      readonly actionId: string
      readonly input?: Readonly<Record<string, unknown>>
    }

/** One retained automation definition (`kind: "automation"`). */
export interface AutomationDefinition {
  readonly title: string
  readonly description?: string
  readonly enabled: boolean
  readonly objectType: string
  readonly when: AutomationWhen
  readonly if?: AutomationGuard
  readonly then: AutomationThen
}

export interface StoredAutomation {
  readonly id: string
  readonly version: number
  readonly updatedAtMs?: number
  readonly automation: AutomationDefinition
}

export type AutomationSaved =
  | { readonly saved: true; readonly id: string; readonly version: number }
  | { readonly saved: false; readonly findings: readonly Finding[] }
  | { readonly saved: false; readonly conflict: true }

export type AutomationAdmission =
  | { readonly admitted: true; readonly restatement: string | null }
  | { readonly admitted: false; readonly findings: readonly Finding[] }

export interface AutomationPreviewMatch {
  readonly objectType: string
  readonly id: string
  readonly version: number
  readonly values: Readonly<Record<string, unknown>>
  readonly reason: string
  readonly staleForMs?: number
  readonly proposedAction: { readonly actionId: string; readonly input: Readonly<Record<string, unknown>> }
}

export interface AutomationPreviewResponse {
  readonly objectType: string
  readonly scannedCount: number
  readonly matchedCount: number
  readonly approximate: boolean
  readonly restatement: string
  readonly matches: readonly AutomationPreviewMatch[]
}

export type AutomationRunOutcome = "proposed" | "committed" | "refused" | "skipped"

export interface AutomationRunRecord {
  readonly id: string
  readonly automationId: string
  readonly automationRevision: number
  readonly dedupeKey: string
  readonly trigger: string
  readonly objectType: string
  readonly objectId: string
  readonly objectVersion: number
  readonly actionId: string
  readonly outcome: AutomationRunOutcome
  readonly decisionId?: string
  readonly commitSeq?: number
  readonly refusalCode?: string
  readonly refusalMessage?: string
  readonly recordedAtMs: number
}

export interface AutomationRunResponse {
  readonly id: string
  readonly version: number
  readonly enabled: boolean
  readonly scannedCount: number
  readonly matchedCount: number
  readonly proposed: number
  readonly committed: number
  readonly refused: number
  readonly skipped: number
  readonly runs: readonly AutomationRunRecord[]
}

export interface AutomationRunsPage {
  readonly id: string
  readonly items: readonly AutomationRunRecord[]
}

export const AUTOMATION_ROLES = ["admin", "member", "owner", "viewer"] as const

const KIND = "automation"

export const newAutomationId = (
  title: string,
  random: () => string = () => crypto.randomUUID().replaceAll("-", "").slice(0, 8),
) =>
  `studio.automation.${title.trim().toLowerCase().replaceAll(/[^a-z0-9]+/g, "_").replaceAll(/^_+|_+$/g, "").slice(0, 44) || "rule"}_${random()}`

export function duplicateAutomation(def: AutomationDefinition): AutomationDefinition {
  return { ...structuredClone(def), title: `${def.title} (copy)` }
}

/** Map a server JSON pointer into an automation payload onto its editor `data-field`. */
export function automationField(pointer: string | null): string | null {
  const parts = (pointer ?? "").split("/").slice(1)
  const at = parts[0] === "payload" ? parts.slice(1) : parts
  const head = at[0]
  if (head === "title" || head === "description" || head === "objectType" || head === "when" || head === "if" || head === "then") {
    return head
  }
  return null
}

/** Strip empty optional values so the server sees a clean payload. */
export function cleanAutomation(def: AutomationDefinition): AutomationDefinition {
  const description = def.description?.trim()
  const expr = def.if?.expression.trim()
  const when: AutomationWhen = def.when.kind === "event"
    ? {
        kind: "event",
        event: def.when.event,
        ...(def.when.event === "action" && def.when.actionId ? { actionId: def.when.actionId } : {}),
      }
    : def.when.kind === "stale"
      ? {
          kind: "stale",
          afterMs: def.when.afterMs,
          ...(def.when.property && def.when.state ? { property: def.when.property, state: def.when.state } : {}),
        }
      : def.when
  const cleanedInput = (input: Readonly<Record<string, unknown>> | undefined) => {
    if (!input) return undefined
    const entries = Object.entries(input).filter(([, v]) => v !== "" && v !== undefined)
    return entries.length > 0 ? Object.fromEntries(entries) : undefined
  }
  const input = cleanedInput(def.then.input)
  const then: AutomationThen = def.then.kind === "propose"
    ? {
        kind: "propose",
        actionId: def.then.actionId,
        ...(input ? { input } : {}),
        question: def.then.question.trim(),
        risk: def.then.risk,
        decider: {
          roles: def.then.decider.roles,
          ...(def.then.decider.law?.trim() ? { law: def.then.decider.law.trim() } : {}),
        },
        ...(def.then.expiresInMs ? { expiresInMs: def.then.expiresInMs } : {}),
      }
    : {
        kind: "run",
        actionId: def.then.actionId,
        ...(input ? { input } : {}),
      }
  return {
    title: def.title.trim(),
    ...(description ? { description } : {}),
    enabled: def.enabled,
    objectType: def.objectType,
    when,
    ...(expr ? { if: { expression: expr } } : {}),
    then,
  }
}

// ---- Visual guard conditions <-> ZEN expression ---------------------------------------------

export type GuardOp = "eq" | "ne" | "lt" | "lte" | "gt" | "gte"

export interface GuardClause {
  readonly property: string
  readonly op: GuardOp
  readonly value: string
}

export const GUARD_OP_LABEL: Readonly<Record<GuardOp, string>> = {
  eq: "is",
  ne: "is not",
  lt: "is less than",
  lte: "is at most",
  gt: "is more than",
  gte: "is at least",
}

const GUARD_SYMBOL: Readonly<Record<GuardOp, string>> = {
  eq: "==",
  ne: "!=",
  lt: "<",
  lte: "<=",
  gt: ">",
  gte: ">=",
}

const SYMBOL_TO_OP: Readonly<Record<string, GuardOp>> = {
  "==": "eq",
  "!=": "ne",
  "<": "lt",
  "<=": "lte",
  ">": "gt",
  ">=": "gte",
}

export function guardOpsFor(scalar: Scalar): readonly GuardOp[] {
  switch (scalar.kind) {
    case "integer":
    case "decimal":
      return ["lt", "lte", "gt", "gte", "eq", "ne"]
    case "enum":
    case "text":
      return ["eq", "ne"]
    case "boolean":
      return ["eq"]
    default:
      return []
  }
}

export const guardableFields = (fields: readonly Field[]) => fields.filter((f) => guardOpsFor(f.scalar).length > 0)

export function blankGuardClause(field: Field): GuardClause {
  const ops = guardOpsFor(field.scalar)
  const value = field.scalar.kind === "boolean"
    ? "true"
    : field.scalar.kind === "enum"
      ? (field.scalar.values[0] ?? "")
      : ""
  return { property: field.id, op: ops[0] ?? "eq", value }
}

const INT_LITERAL = /^-?\d+$/
const NUM_LITERAL = /^-?\d+(\.\d+)?$/

/** Validate every visual guard clause; empty clause list is valid (no guard). */
export function validateGuardClauses(clauses: readonly GuardClause[], fields: readonly Field[]): readonly string[] {
  const errors: string[] = []
  clauses.forEach((clause, index) => {
    const label = `Condition ${index + 1}`
    const field = fields.find(({ id }) => id === clause.property)
    if (!field) {
      errors.push(`${label}: choose a record field.`)
      return
    }
    const name = field.id.replaceAll("_", " ")
    if (!guardOpsFor(field.scalar).includes(clause.op)) {
      errors.push(`${label} (${name}): operator is not supported for this field type.`)
      return
    }
    switch (field.scalar.kind) {
      case "integer": {
        const trimmed = clause.value.trim()
        if (trimmed === "") errors.push(`${label} (${name}): enter a whole number.`)
        else if (!INT_LITERAL.test(trimmed)) errors.push(`${label} (${name}): “${trimmed}” is not a whole number.`)
        break
      }
      case "decimal": {
        const trimmed = clause.value.trim()
        if (trimmed === "") errors.push(`${label} (${name}): enter a number.`)
        else if (!NUM_LITERAL.test(trimmed)) errors.push(`${label} (${name}): “${trimmed}” is not a valid number.`)
        break
      }
      case "boolean":
        if (clause.value !== "true" && clause.value !== "false") {
          errors.push(`${label} (${name}): choose Yes or No.`)
        }
        break
      case "enum":
        if (clause.value === "" || !field.scalar.values.includes(clause.value)) {
          errors.push(`${label} (${name}): choose one of the declared values.`)
        }
        break
      case "text":
        if (clause.value === "") errors.push(`${label} (${name}): enter a value.`)
        break
      default:
        errors.push(`${label} (${name}): field type cannot be compared in visual mode.`)
    }
  })
  return errors
}

/** Whether parsed clauses can be losslessly represented by the visual guard controls for `fields`. */
export function canRenderGuardVisually(clauses: readonly GuardClause[] | null, fields: readonly Field[]): boolean {
  if (clauses === null) return false
  if (clauses.length === 0) return true
  return validateGuardClauses(clauses, fields).length === 0
}

function formatLiteral(value: string, scalar: Scalar | undefined): string | null {
  if (scalar?.kind === "boolean") return value === "false" ? "false" : value === "true" ? "true" : null
  if (scalar?.kind === "integer") {
    const trimmed = value.trim()
    return INT_LITERAL.test(trimmed) ? trimmed : null
  }
  if (scalar?.kind === "decimal") {
    const trimmed = value.trim()
    return NUM_LITERAL.test(trimmed) ? trimmed : null
  }
  // Preserve text and enum operands verbatim (including leading/trailing whitespace).
  if (value === "") return null
  return `'${value.replaceAll("\\", "\\\\").replaceAll("'", "\\'")}'`
}

/**
 * Compile visual guard clauses into a ZEN boolean expression joined with `and`.
 * Returns `undefined` when `clauses` is empty or when any clause is incomplete/invalid
 * (callers must check `validateGuardClauses` before preview or save).
 */
export function compileGuard(clauses: readonly GuardClause[], fields: readonly Field[]): string | undefined {
  if (clauses.length === 0) return undefined
  if (validateGuardClauses(clauses, fields).length > 0) return undefined
  const parts: string[] = []
  for (const clause of clauses) {
    const field = fields.find(({ id }) => id === clause.property)
    const literal = formatLiteral(clause.value, field?.scalar)
    if (literal === null) return undefined
    parts.push(`self.${clause.property} ${GUARD_SYMBOL[clause.op]} ${literal}`)
  }
  return parts.length === 0 ? undefined : parts.join(" and ")
}

/** Encode typed static `then.input` drafts against an action's declared input schema. */
export function encodeStaticActionInputs(
  fields: readonly FormField[],
  drafts: Readonly<Record<string, string>>,
): {
  readonly input: Readonly<Record<string, unknown>> | undefined
  readonly errors: readonly string[]
} {
  const out: Record<string, unknown> = {}
  const errors: string[] = []
  for (const field of fields) {
    const raw = drafts[field.name] ?? ""
    const trimmed = raw.trim()
    if (trimmed === "") continue
    switch (field.spec.kind) {
      case "integer":
        if (!INT_LITERAL.test(trimmed)) {
          errors.push(`${field.label}: “${trimmed}” must be a whole number.`)
        } else {
          const n = Number(trimmed)
          if (!Number.isSafeInteger(n)) errors.push(`${field.label}: whole number is out of range.`)
          else out[field.name] = n
        }
        break
      case "number":
        if (!NUM_LITERAL.test(trimmed)) {
          errors.push(`${field.label}: “${trimmed}” must be a valid number.`)
        } else {
          const n = Number(trimmed)
          if (!Number.isFinite(n)) errors.push(`${field.label}: number is out of range.`)
          else out[field.name] = n
        }
        break
      case "boolean":
        if (raw === "true" || raw === "false") out[field.name] = raw === "true"
        else errors.push(`${field.label}: choose Yes or No.`)
        break
      case "choice":
        if (field.spec.values.includes(raw)) out[field.name] = raw
        else errors.push(`${field.label}: choose one of the declared values.`)
        break
      default:
        out[field.name] = raw
        break
    }
  }
  return { input: Object.keys(out).length > 0 ? out : undefined, errors }
}

const SIMPLE_CLAUSE = /^self\.([A-Za-z_][A-Za-z0-9_]*)\s*(==|!=|<=|>=|<|>)\s*(.+)$/

/** Parse a simple `and`-joined ZEN guard back into visual clauses, or `null` if it uses custom syntax. */
export function parseGuard(expression: string | undefined): readonly GuardClause[] | null {
  const text = expression?.trim() ?? ""
  if (text === "") return []
  if (/\bor\b|\(|\)/.test(text)) return null
  const segments = text.split(/\s+and\s+/)
  const out: GuardClause[] = []
  for (const segment of segments) {
    const match = SIMPLE_CLAUSE.exec(segment.trim())
    if (!match) return null
    const property = match[1]!
    const op = SYMBOL_TO_OP[match[2]!]
    const raw = match[3]!.trim()
    if (!op) return null
    let value: string
    if (raw.startsWith("'") && raw.endsWith("'") && raw.length >= 2) {
      value = raw.slice(1, -1).replaceAll("\\'", "'").replaceAll("\\\\", "\\")
    } else if (raw === "true" || raw === "false" || /^-?\d+(\.\d+)?$/.test(raw)) {
      value = raw
    } else {
      return null
    }
    out.push({ property, op, value })
  }
  return out
}

// ---- Plain-language helpers -----------------------------------------------------------------

export type DurationUnit = "minutes" | "hours" | "days"

const UNIT_MS: Readonly<Record<DurationUnit, number>> = {
  minutes: 60_000,
  hours: 3_600_000,
  days: 86_400_000,
}

export function durationParts(ms: number): { readonly amount: number; readonly unit: DurationUnit } {
  if (ms >= UNIT_MS.days && ms % UNIT_MS.days === 0) return { amount: ms / UNIT_MS.days, unit: "days" }
  if (ms >= UNIT_MS.hours && ms % UNIT_MS.hours === 0) return { amount: ms / UNIT_MS.hours, unit: "hours" }
  return { amount: Math.max(1, Math.round(ms / UNIT_MS.minutes)), unit: "minutes" }
}

export const msFromDuration = (amount: number, unit: DurationUnit) =>
  Math.max(60_000, Math.round(Math.max(1, amount) * UNIT_MS[unit]))

export function describeDuration(ms: number): string {
  const { amount, unit } = durationParts(ms)
  const singular = unit.slice(0, -1)
  return `${amount} ${amount === 1 ? singular : unit}`
}

export function describeWhen(when: AutomationWhen): string {
  switch (when.kind) {
    case "lifecycle_entered":
      return `${when.property.replaceAll("_", " ")} enters ${when.state.replaceAll("_", " ")}`
    case "stale": {
      const base = `unchanged for ${describeDuration(when.afterMs)}`
      return when.property && when.state
        ? `${base} in ${when.property.replaceAll("_", " ")} = ${when.state.replaceAll("_", " ")}`
        : base
    }
    case "event":
      if (when.event === "created") return "a record is created"
      if (when.event === "updated") return "a record is updated"
      return `action ${when.actionId ?? "(any)"} commits`
  }
}

export function describeThen(then: AutomationThen, actionTitle?: string): string {
  const name = actionTitle ?? then.actionId
  if (then.kind === "propose") {
    return `propose ${name} to Inbox (${then.decider.roles.join(", ")})`
  }
  return `run ${name} automatically`
}

// ---- HTTP client ----------------------------------------------------------------------------

const findingsOf = (body: Record<string, unknown> | null): readonly Finding[] =>
  Array.isArray(body?.diagnostics)
    ? (body.diagnostics as { code?: unknown; message?: unknown; law?: unknown; span?: { pointer?: unknown } | null }[]).map((d) => ({
      code: String(d.code ?? ""),
      message: String(d.message ?? ""),
      law: typeof d.law === "string" ? d.law : null,
      pointer: typeof d.span?.pointer === "string" ? d.span.pointer : null,
    }))
    : []

const isAutomation = (value: unknown): value is AutomationDefinition =>
  value !== null
  && typeof value === "object"
  && typeof (value as AutomationDefinition).title === "string"
  && typeof (value as AutomationDefinition).objectType === "string"
  && typeof (value as AutomationDefinition).when === "object"
  && typeof (value as AutomationDefinition).then === "object"

/** Server-driven automations client through the Studio Worker's scoped host proxy. */
export function cellAutomations(fetcher: Fetch = (input, init) => fetch(input, init)) {
  const callWith = (scopedFetch: Fetch) => async (path: string, init?: RequestInit) => {
    const response = await scopedFetch(path, {
      ...init,
      headers: { accept: "application/json", ...init?.body === undefined ? {} : { "content-type": "application/json" } },
    })
    return { status: response.status, body: await response.json().catch(() => null) as Record<string, unknown> | null }
  }
  const call = callWith(scopedHostFetch(fetcher))
  const saved = ({ status, body }: { status: number; body: Record<string, unknown> | null }): AutomationSaved => {
    if (status === 200 && body?.installed === true && typeof body.id === "string" && typeof body.version === "number") {
      return { saved: true, id: body.id, version: body.version }
    }
    if (status === 409 && body?.code === "definition.version_conflict") return { saved: false, conflict: true }
    if (status === 422 && Array.isArray(body?.diagnostics)) return { saved: false, findings: findingsOf(body) }
    throw new CellUnavailable(status, typeof body?.message === "string" ? body.message : undefined)
  }

  const getWith = (read: typeof call) => async (id: string): Promise<StoredAutomation | undefined> => {
    const { status, body } = await read(`/v1/definitions/${encodeURIComponent(id)}`)
    if (status === 404 && body?.code === "definition.not_found") return undefined
    if (status !== 200 || typeof body?.version !== "number" || !isAutomation(body.payload)) {
      throw new CellUnavailable(status, typeof body?.message === "string" ? body.message : undefined)
    }
    return {
      id: typeof body.id === "string" ? body.id : id,
      version: body.version,
      ...(typeof body.updatedAtMs === "number" ? { updatedAtMs: body.updatedAtMs } : {}),
      automation: body.payload,
    }
  }
  const get = getWith(call)

  return {
    list: async (pinnedScope?: StudioScope | null): Promise<readonly StoredAutomation[]> => {
      // Pin one workspace scope across list + per-id GETs so workspace switches cannot mix scopes.
      const active = pinnedScope !== undefined ? pinnedScope : await readStudioScope(fetcher)
      if (active === null) return []
      const pinnedCall = callWith(scopedHostFetch(fetcher, "/api/host", async () => active))
      const pinnedGet = getWith(pinnedCall)
      const ids: { readonly id: string; readonly updatedAtMs?: number }[] = []
      let cursor: string | null = null
      for (let page = 0; page < 10; page += 1) {
        const params = new URLSearchParams({ kind: KIND, limit: "50", ...(cursor ? { cursor } : {}) })
        const { status, body } = await pinnedCall(`/v1/definitions:list?${params}`)
        const items = Array.isArray(body?.items) ? body.items as { id?: unknown; updatedAtMs?: unknown }[] : undefined
        if (status !== 200 || items === undefined) {
          throw new CellUnavailable(status, typeof body?.message === "string" ? body.message : undefined)
        }
        for (const item of items) {
          if (typeof item.id === "string") {
            ids.push({ id: item.id, ...(typeof item.updatedAtMs === "number" ? { updatedAtMs: item.updatedAtMs } : {}) })
          }
        }
        cursor = typeof body?.cursor === "string" ? body.cursor : null
        if (cursor === null) break
      }
      const read = await Promise.all(ids.map(async ({ id, updatedAtMs }) => {
        // Explicit 404 definition.not_found returns undefined; 5xx/transport errors propagate.
        const stored = await pinnedGet(id)
        return stored === undefined ? [] : [{ ...stored, ...(updatedAtMs !== undefined && stored.updatedAtMs === undefined ? { updatedAtMs } : {}) }]
      }))
      return read.flat()
    },
    get,
    admit: async (id: string, def: AutomationDefinition): Promise<AutomationAdmission> => {
      const { status, body } = await call("/v1/definitions:admit", {
        method: "POST",
        body: JSON.stringify({ kind: KIND, id, payload: cleanAutomation(def) }),
      })
      if (status !== 200 || typeof body?.admitted !== "boolean") {
        throw new CellUnavailable(status, typeof body?.message === "string" ? body.message : undefined)
      }
      return body.admitted
        ? { admitted: true, restatement: typeof body.restatement === "string" ? body.restatement : null }
        : { admitted: false, findings: findingsOf(body) }
    },
    save: async (id: string, def: AutomationDefinition, expectedVersion?: number): Promise<AutomationSaved> =>
      saved(await call("/v1/definitions:install", {
        method: "POST",
        body: JSON.stringify({ kind: KIND, id, payload: cleanAutomation(def), ...expectedVersion === undefined ? {} : { expectedVersion } }),
      })),
    retire: async (id: string, expectedVersion: number): Promise<AutomationSaved> =>
      saved(await call(`/v1/definitions/${encodeURIComponent(id)}:retire`, {
        method: "POST",
        body: JSON.stringify({ expectedVersion }),
      })),
    preview: async (request: { readonly id: string } | { readonly payload: AutomationDefinition }): Promise<AutomationPreviewResponse> => {
      const body = "payload" in request ? { payload: cleanAutomation(request.payload) } : request
      const { status, body: resp } = await call("/v1/automations:preview", {
        method: "POST",
        body: JSON.stringify(body),
      })
      if (status === 422 && Array.isArray(resp?.diagnostics)) {
        const msg = findingsOf(resp).map((f) => f.message).join(" ")
        throw new Error(msg || "The cell refused this automation.")
      }
      if (status !== 200 || resp === null || !Array.isArray(resp.matches)) {
        throw new CellUnavailable(status, typeof resp?.message === "string" ? resp.message : undefined)
      }
      return resp as unknown as AutomationPreviewResponse
    },
    run: async (id: string): Promise<AutomationRunResponse> => {
      const { status, body } = await call(`/v1/automations/${encodeURIComponent(id)}:run`, {
        method: "POST",
        body: "{}",
      })
      if (status !== 200 || body === null || !Array.isArray(body.runs)) {
        throw new CellUnavailable(status, typeof body?.message === "string" ? body.message : undefined)
      }
      return body as unknown as AutomationRunResponse
    },
    runs: async (id: string, limit = 50): Promise<AutomationRunsPage> => {
      const params = new URLSearchParams({ limit: String(limit) })
      const { status, body } = await call(`/v1/automations/${encodeURIComponent(id)}/runs?${params}`)
      if (status !== 200 || body === null || !Array.isArray(body.items)) {
        throw new CellUnavailable(status, typeof body?.message === "string" ? body.message : undefined)
      }
      return body as unknown as AutomationRunsPage
    },
  }
}
