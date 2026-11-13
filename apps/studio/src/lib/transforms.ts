/**
 * Typed transforms (ViewIR v1, #341): the installed transforms from the catalog and the
 * cell's three reads of one (its rows, one row's lineage, its admitted plan). Every figure is
 * the cell's; a cell without the routes reads "not served", never blank.
 */
import { useQuery } from "@tanstack/react-query"
import type { Read } from "./exact.ts"
import { cellRead } from "./live-transport.ts"
import { atScale } from "./objects.ts"
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
}

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
