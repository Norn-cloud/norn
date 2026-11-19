import { useQuery } from "@tanstack/react-query"
import { cellRead, useMode, type Fetch } from "./live-transport.ts"

/** One installed flow as `GET /v1/definitions:list?kind=flow` lists it. */
export interface FlowItem {
  readonly id: string
  readonly version?: number
  readonly title?: string | null
  readonly flow?: { readonly flowId?: string; readonly version?: string; readonly title?: string | null }
  /** Whether this caller may start it, decided by the cell with the gate `POST /v1/runs` applies; absent on a cell that does not say. */
  readonly canStart?: boolean
}

/** An installed flow's admitted payload: its flow and the artifacts it brought, among them the object type its runs are. */
interface FlowPayload {
  readonly flow?: { readonly title?: string | null; readonly steps?: readonly { readonly id?: string; readonly label?: string | null }[] }
  readonly artifacts?: readonly { readonly kind?: string; readonly payload?: { readonly objectType?: { readonly name?: string } } }[]
}

/** The installed flows, as the cell lists them. */
export const readFlows = async (fetcher?: Fetch) =>
  (await cellRead<{ readonly items: readonly FlowItem[] }>("/v1/definitions:list?kind=flow&limit=50", "Automations are", {}, fetcher)).document.items

/** The flow a run type belongs to: its installed title and each step's label, by step id. */
export interface RunFlow { readonly title: string; readonly steps: ReadonlyMap<string, string> }

/** Each flow's run type, by name, with the flow's title and step labels. */
export function runTypeFlows(payloads: readonly FlowPayload[]): ReadonlyMap<string, RunFlow> {
  return new Map(payloads.flatMap(({ flow, artifacts }) => {
    const title = flow?.title
    if (!title) return []
    const steps = new Map((flow?.steps ?? []).flatMap(({ id, label }) => id && label ? [[id, label] as const] : []))
    return (artifacts ?? []).flatMap(({ kind, payload }) => kind === "entity" && payload?.objectType?.name ? [[payload.objectType.name, { title, steps }] as const] : [])
  }))
}

const sentence = (text: string) => text.charAt(0).toUpperCase() + text.slice(1)
const plain = (name: string) => sentence(name.replaceAll("_", " "))

/**
 * A generated run state or transition in the flow's own words. The flow builder names its run
 * lifecycle by a fixed grammar (tools/norn-ts/src/builder/lower-flow.ts): `started`,
 * `awaiting_perform_<step>`, `awaiting_retry_<step>_attempt_<n>`, `retrying_<step>_attempt_<n>`,
 * `sleeping_<step>`, `completed`, `failed_<tag>`, and one transition `to_<state>` per target.
 * A step reads by its label; a name outside the grammar is undefined, so the caller keeps it.
 */
export function flowName(flow: RunFlow, name: string): string | undefined {
  const step = (id: string) => flow.steps.get(id) ?? plain(id)
  const state = (id: string): string | undefined => {
    if (id === "started") return "Started"
    if (id === "completed") return "Completed"
    let match = /^awaiting_perform_(.+)$/.exec(id)
    if (match) return `Waiting: ${step(match[1]!)}`
    match = /^awaiting_retry_(.+)_attempt_(\d+)$/.exec(id)
    if (match) return `Waiting to retry: ${step(match[1]!)} (attempt ${match[2]})`
    match = /^retrying_(.+)_attempt_(\d+)$/.exec(id)
    if (match) return `Retrying: ${step(match[1]!)} (attempt ${match[2]})`
    match = /^sleeping_(.+)$/.exec(id)
    if (match) return `Paused: ${step(match[1]!)}`
    match = /^failed_(.+)$/.exec(id)
    if (match) return plain(match[1]!)
    return undefined
  }
  if (!name.startsWith("to_")) return state(name)
  const target = name.slice("to_".length)
  // Moving into a step is that step, in the flow's words; any other move reads as where it lands.
  const into = /^awaiting_perform_(.+)$/.exec(target)
  return into ? step(into[1]!) : state(target)
}

/** Read each installed flow's definition: the flow its run type belongs to, by the run type's name. */
export async function readRunTypeFlows(fetcher?: Fetch): Promise<ReadonlyMap<string, RunFlow>> {
  const flows = await readFlows(fetcher)
  const payloads = await Promise.all(flows.map(async ({ id }) =>
    (await cellRead<{ readonly payload: FlowPayload }>(`/v1/definitions/${encodeURIComponent(id)}`, "Automations are", {}, fetcher)).document.payload))
  return runTypeFlows(payloads)
}

/** On a connected cell, the flows whose runs are object types; empty otherwise or when they cannot be read. */
export function useRunTypeFlows(): ReadonlyMap<string, RunFlow> {
  const live = useMode().data?.kind === "live"
  const flows = useQuery({ queryKey: ["flows", "run-types"], queryFn: () => readRunTypeFlows(), enabled: live, retry: false })
  return flows.data ?? EMPTY
}

const EMPTY: ReadonlyMap<string, RunFlow> = new Map()
