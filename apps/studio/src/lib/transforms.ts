/**
 * Typed transforms (ViewIR v1, #341): the installed transforms from the catalog and the
 * cell's three reads of one (its rows, one row's lineage, its admitted plan). Every figure is
 * the cell's; a cell without the routes reads "not served", never blank.
 */
import { useQuery } from "@tanstack/react-query"
import type { Read } from "./exact.ts"
import { cellRead } from "./live-transport.ts"
import { day } from "./format.ts"
import { atScale } from "./objects.ts"
import type { View } from "./view.ts"
import type { Artifact } from "./transport.ts"

/** One typed column of a transform's row. */
export interface TransformColumn {
  readonly name: string
  readonly type: string
  readonly nullable: boolean
  readonly additivity: string
  readonly identity: string | null
  readonly origins: readonly { readonly input: string; readonly property: string }[]
  readonly markings: readonly string[]
  /** The column of the same row holding this quantity's currency (VIEW-AGG-1); absent from an older cell. */
  readonly currencyFrom?: string | null
  /** The columns of the same row whose values are this quantity's unit (METRIC-UNIT-PARTITION-1). */
  readonly unitFrom?: readonly string[]
  /** What readers call the column, as its definition labels it; absent or null when it has only its name. */
  readonly label?: string | null
}

/** A column's heading: its definition's label for Business readers, its name (the id queries use) for Technical. */
export const columnTitle = (column: Pick<TransformColumn, "name" | "label">, view: View) =>
  view === "business" ? (column.label ?? column.name) : column.name

export interface TransformRow {
  readonly rowId: string
  readonly values: Readonly<Record<string, unknown>>
  readonly memberCount: number
}

export interface TransformPage {
  readonly transform: string
  readonly version: string
  readonly engine: string
  readonly commits: number
  readonly columns: readonly TransformColumn[]
  readonly grain: readonly string[]
  readonly rows: readonly TransformRow[]
  readonly totalRows: number
  readonly withheld: Readonly<Record<string, number>>
  readonly excluded: number
  readonly complete: boolean
  /** Rows each plan node produced for this read; absent from a cell that does not report node volumes. */
  readonly nodeRows?: Readonly<Record<string, number>>
  readonly nextAfter: string | null
}

export interface TransformLineage {
  readonly transform: string
  readonly rowId: string
  readonly commits: number
  readonly values: Readonly<Record<string, unknown>>
  readonly members: readonly { readonly input: string; readonly objectType: string; readonly id: string; readonly version: number }[]
}

export interface TransformNode {
  readonly id: string
  readonly op: string
  readonly inputs: readonly string[]
  readonly grain: readonly string[]
  readonly columns: readonly TransformColumn[]
}

export interface TransformPlan {
  readonly transform: string
  readonly version: string
  readonly viewId: string
  readonly inputs: readonly { readonly alias: string; readonly objectType: string; readonly artifactId: string; readonly version: string }[]
  readonly nodes: readonly TransformNode[]
  readonly output: string
}

/** An installed transform as the catalog lists it. */
export interface TransformEntry {
  readonly id: string
  readonly title: string
  readonly version: string
  readonly inputs: readonly string[]
  readonly output: string
}

/** The relational `derived` artifacts of a catalog: the installed transforms, by id. */
export function transformsOf(artifacts: readonly Artifact[]): readonly TransformEntry[] {
  return artifacts.flatMap((artifact) => {
    const envelope = artifact as unknown as { kind?: string; artifactId?: string; name?: string; version?: string; payload?: { calc?: { kind?: string; output?: string }; inputs?: { artifactId?: string }[] } }
    if (envelope.kind !== "derived" || envelope.payload?.calc?.kind !== "relational" || typeof envelope.artifactId !== "string") return []
    return [{
      id: envelope.artifactId,
      title: envelope.name ?? envelope.artifactId,
      version: envelope.version ?? "",
      inputs: (envelope.payload.inputs ?? []).flatMap(({ artifactId }) => typeof artifactId === "string" ? [artifactId.replace(/\.entity$/, "")] : []),
      output: envelope.payload.calc.output ?? "",
    }]
  }).sort((a, b) => a.id.localeCompare(b.id))
}

/** One node's definition as the installed artifact declares it, pretty-printed; undefined when absent. */
export function nodeDefinition(artifacts: readonly Artifact[], transform: string, node: string | undefined): string | undefined {
  if (node === undefined) return undefined
  const envelope = artifacts.find((artifact) => (artifact as unknown as { artifactId?: string }).artifactId === transform) as unknown as
    { payload?: { calc?: { nodes?: Readonly<Record<string, unknown>> } } } | undefined
  const declared = envelope?.payload?.calc?.nodes?.[node]
  return declared === undefined ? undefined : JSON.stringify(declared, null, 2)
}

type Declared = Readonly<Record<string, unknown>>
const declaredNodes = (artifacts: readonly Artifact[], transform: string): Readonly<Record<string, Declared>> => {
  const envelope = artifacts.find((artifact) => (artifact as unknown as { artifactId?: string }).artifactId === transform) as unknown as
    { payload?: { calc?: { nodes?: Readonly<Record<string, Declared>> } } } | undefined
  return envelope?.payload?.calc?.nodes ?? {}
}

/** The calendar grain each bucketed group key is in (`week`, `month`, …), by column name, as the definition declares it. */
export function grainsOf(artifacts: readonly Artifact[], transform: string): ReadonlyMap<string, string> {
  const grains = new Map<string, string>()
  for (const node of Object.values(declaredNodes(artifacts, transform))) {
    if (node.op !== "aggregate" || !Array.isArray(node.groupBy)) continue
    for (const key of node.groupBy as readonly { column?: unknown; grain?: unknown }[]) {
      if (typeof key.column === "string" && typeof key.grain === "string") grains.set(key.column, key.grain)
    }
  }
  return grains
}

const monthYear = new Intl.DateTimeFormat("en-GB", { month: "short", year: "numeric", timeZone: "UTC" })

/** A bucket's first day in words: `2026-10-26` at `week` → `Week of 26 Oct 2026`; month `Oct 2026`; quarter `Q4 2026`; year `2026`. */
export function periodOf(text: string, grain: string): string {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(text)) return text
  const at = `${text}T00:00:00Z`
  if (Number.isNaN(Date.parse(at))) return text
  const [year = "", month = "1"] = text.split("-")
  switch (grain) {
    case "week": return `Week of ${day(at)}`
    case "month": return monthYear.format(new Date(at))
    case "quarter": return `Q${Math.floor((Number(month) - 1) / 3) + 1} ${year}`
    case "year": return year
    default: return day(at)
  }
}

const arrow = (direction: unknown) => direction === "desc" ? "↓" : "↑"
const list = (items: readonly string[]) => items.length <= 1 ? (items[0] ?? "") : `${items.slice(0, -1).join(", ")} and ${items.at(-1)}`
const strings = (value: unknown): readonly string[] => Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : []
const named = (value: unknown): readonly Declared[] => Array.isArray(value) ? value.filter((item): item is Declared => typeof item === "object" && item !== null) : []
const spaced = (name: unknown) => typeof name === "string" ? name.replaceAll("_", " ") : "?"
const FUNCTIONS: Readonly<Record<string, string>> = {
  sum: "sum of", count: "count", count_distinct: "count of distinct", min: "smallest", max: "largest",
  row_number: "row number", rank: "rank", dense_rank: "dense rank", lag: "previous", lead: "next", running_sum: "running sum of",
}

/**
 * One operator in words, from its declaration, for the Business view: `Left join customer on
 * order.customer = customer.id`; `Window over weekly, per region, ordered by placed at ↑`.
 * `inputs` names each scanned alias's object type. Technical readers see the declaration itself.
 */
export function describeStep(step: Declared, inputs: ReadonlyMap<string, string>): string {
  const measure = (item: Declared) => {
    const fn = typeof item.fn === "string" ? (FUNCTIONS[item.fn] ?? item.fn) : "?"
    const column = typeof item.column === "string" ? ` ${spaced(item.column)}` : ""
    return `${spaced(item.name)} = ${fn}${column}`
  }
  switch (step.op) {
    case "scan": {
      const alias = typeof step.input === "string" ? step.input : "?"
      return `Every ${inputs.get(alias) ?? alias} record`
    }
    case "filter":
      return `Keep the rows of ${spaced(step.input)} where ${typeof step.predicate === "string" ? step.predicate.replaceAll("input.", "") : "?"}`
    case "project":
      return `From ${spaced(step.input)}: ${list(named(step.columns).map((column) => spaced(column.name)))}`
    case "join": {
      const on = (step.on ?? {}) as Declared
      const kind = step.kind === "left" ? "Left join" : "Join"
      const link = on.link as Declared | undefined
      const key = on.key as Declared | undefined
      const right = typeof step.right === "string" ? step.right : "?"
      const left = typeof step.left === "string" ? step.left : "?"
      if (link) return `${kind} ${right} on ${left}.${typeof link.link === "string" ? link.link : "?"} = ${right}.id`
      if (key) return `${kind} ${right} on ${left}.${strings(key.left).join(" + ")} = ${right}'s ${spaced(key.right)}`
      return `${kind} ${right}`
    }
    case "aggregate": {
      const keys = named(step.groupBy).map((key) => typeof key.grain === "string" ? `${spaced(key.column)} by ${key.grain}` : spaced(key.column))
      const per = keys.length === 0 ? "in total" : `per ${list(keys)}`
      return `Totals of ${spaced(step.input)} ${per}: ${list(named(step.measures).map(measure))}`
    }
    case "window": {
      const partition = strings(step.partitionBy).map(spaced)
      const order = named(step.orderBy).map((key) => `${spaced(key.column)} ${arrow(key.direction)}`)
      const per = partition.length === 0 ? "" : `, per ${list(partition)}`
      return `Window over ${spaced(step.input)}${per}, ordered by ${list(order)}: ${list(named(step.columns).map(measure))}`
    }
    case "union":
      return `Every row of ${list(strings(step.inputs).map(spaced))}, named in ${spaced(step.branch)}`
    case "dedupe": {
      const order = named(step.orderBy).map((key) => `${spaced(key.column)} ${arrow(key.direction)}`)
      return `One row per ${list(strings(step.key).map(spaced))} of ${spaced(step.input)}, keeping the ${step.keep === "last" ? "last" : "first"} by ${list(order)}`
    }
    default:
      return typeof step.op === "string" ? step.op : "?"
  }
}

/** One node's declaration, as the installed artifact holds it. */
export function nodeDeclaration(artifacts: readonly Artifact[], transform: string, node: string | undefined): Declared | undefined {
  return node === undefined ? undefined : declaredNodes(artifacts, transform)[node]
}

const path = (id: string, leaf: string) => `/v1/transforms/${encodeURIComponent(id)}/${leaf}`

/** One page of a transform's rows; `after` is the previous page's `nextAfter`. */
export const useTransformRows = (id: string, after: string | undefined, limit = 50) =>
  useQuery<Read<TransformPage>>({
    queryKey: ["transform-rows", id, after ?? "", limit],
    queryFn: () => cellRead<TransformPage>(`${path(id, "rows")}?limit=${limit}${after === undefined ? "" : `&after=${encodeURIComponent(after)}`}`, "Transform rows are"),
    retry: false,
  })

/** The input rows one transform row derives from. */
export const useTransformLineage = (id: string, rowId: string | undefined) =>
  useQuery<Read<TransformLineage>>({
    queryKey: ["transform-lineage", id, rowId ?? ""],
    queryFn: () => cellRead<TransformLineage>(path(id, `rows/${encodeURIComponent(rowId ?? "")}/lineage`), "Transform lineage is"),
    enabled: rowId !== undefined,
    retry: false,
  })

/** The transform's admitted plan: the definition graph. */
export const useTransformPlan = (id: string) =>
  useQuery<Read<TransformPlan>>({
    queryKey: ["transform-plan", id],
    queryFn: () => cellRead<TransformPlan>(path(id, "plan"), "Transform plans are"),
    retry: false,
  })

/** One checker finding on a checked query: the registered code is the contract, the message says it. */
export interface TransformDiagnostic {
  readonly code: string
  readonly severity: string
  readonly message: string
  readonly law?: string
  readonly span: { readonly pointer: string } | null
}

/** A query in the transform dialect as the cell's checker sees it: the admitted plan, or why not. */
export interface CompiledTransform {
  readonly admitted: boolean
  readonly plan: TransformPlan | null
  readonly diagnostics: readonly TransformDiagnostic[]
}

/** The request Studio sends to check a query: the query is lowered and typed, never run or installed. */
export const checkRequest = (sql: string) => ({ id: "studio.checked_query", title: "Checked query", version: "0.1.0", sql })

/** Check a query in the transform dialect (VIEW-SQL-1) against the cell's installed definitions. */
export const checkQuery = (sql: string) =>
  cellRead<CompiledTransform>("/v1/transforms:compile", "Query checks are", { method: "POST", body: JSON.stringify(checkRequest(sql)) })

/** The declared scale of a `decimal<N>` column type, as the plan names it. */
export function scaleOf(type: string): number | undefined {
  const declared = /^decimal<(\d+)>$/.exec(type)
  return declared === null ? undefined : Number(declared[1])
}

/** A value as the cell sent it: numbers by their exact text, decimals written to their declared scale as elsewhere in Studio. */
export function exactValue(read: Read<unknown>, pointer: string, value: unknown, scale?: number): string {
  if (value === null || value === undefined) return "—"
  if (typeof value === "number") return atScale(read.numbers.get(pointer) ?? String(value), scale)
  if (typeof value === "string") return value
  return JSON.stringify(value)
}
