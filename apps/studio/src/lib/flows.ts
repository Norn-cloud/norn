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
  readonly flow?: { readonly title?: string | null }
  readonly artifacts?: readonly { readonly kind?: string; readonly payload?: { readonly objectType?: { readonly name?: string } } }[]
}

/** The installed flows, as the cell lists them. */
export const readFlows = async (fetcher?: Fetch) =>
  (await cellRead<{ readonly items: readonly FlowItem[] }>("/v1/definitions:list?kind=flow&limit=50", "Automations are", {}, fetcher)).document.items

/** Each flow's run type, by name, with the title the flow was installed under. */
export function runTypeTitles(payloads: readonly FlowPayload[]): ReadonlyMap<string, string> {
  return new Map(payloads.flatMap(({ flow, artifacts }) => {
    const title = flow?.title
    if (!title) return []
    return (artifacts ?? []).flatMap(({ kind, payload }) => kind === "entity" && payload?.objectType?.name ? [[payload.objectType.name, title] as const] : [])
  }))
}

/** Read each installed flow's definition and name its run type by the flow's title. */
export async function readRunTypeTitles(fetcher?: Fetch): Promise<ReadonlyMap<string, string>> {
  const flows = await readFlows(fetcher)
  const payloads = await Promise.all(flows.map(async ({ id }) =>
    (await cellRead<{ readonly payload: FlowPayload }>(`/v1/definitions/${encodeURIComponent(id)}`, "Automations are", {}, fetcher)).document.payload))
  return runTypeTitles(payloads)
}

/** On a connected cell, the titles of the flows whose runs are object types; empty otherwise or when they cannot be read. */
export function useRunTypeTitles(): ReadonlyMap<string, string> {
  const live = useMode().data?.kind === "live"
  const titles = useQuery({ queryKey: ["flows", "run-type-titles"], queryFn: () => readRunTypeTitles(), enabled: live, retry: false })
  return titles.data ?? EMPTY
}

const EMPTY: ReadonlyMap<string, string> = new Map()
