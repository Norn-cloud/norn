import { useQuery } from "@tanstack/react-query"
import { Bookmark, Box, Layers } from "lucide-react"
import { useMemo } from "react"
import { useLocale } from "@/lib/locale"
import type { MenuEntry } from "./nav-model.ts"
import { catalogOf, conceptTitle } from "./catalog.ts"
import { badgeText, countDecisions } from "./decisions.ts"
import { useStudioScope } from "./definitions.ts"
import { useRunTypeFlows } from "./flows.ts"
import { headline } from "./format.ts"
import { GUARANTEE_TITLES } from "./guarantees.ts"
import { fetchImportTriage, IMPORT_TRIAGE_QUERY_KEY, triageSummary } from "./import-triage.ts"
import { useLiveRefresh } from "./live-reads.ts"
import { useMode } from "./live-transport.ts"
import type { Total } from "./records-query.ts"
import { transport, type QuestionAnswer } from "./transport.ts"
import { useCatalog } from "./use-catalog.ts"
import { cellViews } from "./views.ts"
import { workflowAnchor, workflows, workflowTitle } from "./workflows.ts"

/**
 * Lightweight sidebar menu and badge hooks shared by the shell (`main.tsx`) and their respective pages.
 * Keeping these hooks out of the route page modules lets Vite code-split every route cleanly without
 * pulling charts, flow graphs, decision sheets, or editors into the initial entry chunk.
 */

export { IMPORT_TRIAGE_QUERY_KEY }

const views = cellViews()

/** A menu entry's count: exact, or the server's lower bound as text ("500+"); none when the cell sent none. */
export const menuCount = (total: Total | undefined): number | string | undefined =>
  total === undefined ? undefined : total.approximate ? `${total.count.toLocaleString("en-US")}+` : total.count

/** The shell's badge: how many decisions wait on this actor, as the cell counts them ("500+" when capped). */
export function useDecisionBadge(): string | undefined {
  const count = useQuery({ queryKey: ["decisions", "count"], queryFn: () => countDecisions(), retry: false })
  // The cell says when the count changes (UI-LIVE-1); nothing polls.
  useLiveRefresh("/v1/decisions?state=pending&limit=0", ["decisions", "count"])
  return count.data?.kind === "ok" && count.data.value.total > 0 ? badgeText(count.data.value) : undefined
}

/** Sidebar rail badge showing total open triage items awaiting operator review. */
export function useImportTriageBadge(): number | undefined {
  const read = useQuery({
    queryKey: IMPORT_TRIAGE_QUERY_KEY,
    queryFn: () => fetchImportTriage(),
    refetchInterval: 60_000,
    retry: false,
  })
  if (read.data?.kind !== "ok") return undefined
  const total = triageSummary(read.data.value).totalAttention
  return total > 0 ? total : undefined
}

/** The object types and shared interfaces, for the submenu island. */
export function useSourcesMenu(): readonly MenuEntry[] | undefined {
  const read = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const { locale } = useLocale()
  const catalog = useMemo(() => read.data && catalogOf(read.data.document, locale), [read.data, locale])
  // The cell counts each type for this caller; a type it may not list has no count, and the recording has none.
  const counts = useQuery({ queryKey: ["catalog", "counts"], queryFn: transport.objectCounts }).data
  return catalog && (["object type", "interface"] as const).flatMap((definition) => catalog.concepts.filter((concept) => concept.definition === definition)
    .map((concept): MenuEntry => ({
      href: `/sources/${concept.name}`,
      label: concept.title,
      group: definition === "object type" ? "Object types" : "Shared interfaces",
      icon: definition === "object type" ? Box : Layers,
      kind: "objectType",
      count: menuCount(counts?.[concept.name]),
    })))
}

/** The operator's view of the same model: each object type's records (its grid), with the count the cell gave this caller. */
export function useRecordsMenu(): readonly MenuEntry[] | undefined {
  const { catalog } = useCatalog()
  const counts = useQuery({ queryKey: ["catalog", "counts"], queryFn: transport.objectCounts }).data
  return catalog?.concepts.filter((concept) => concept.definition === "object type")
    .map((concept): MenuEntry => ({
      href: `/objects/${concept.name}`,
      also: `/sources/${concept.name}`,
      label: concept.title,
      kind: "objectType",
      count: menuCount(counts?.[concept.name]),
    }))
}

/**
 * Saved views for the Workspace sidebar. Pinned views carry `group: space`, so the shell places
 * them directly inside their Space; unpinned views sit under the Views destination.
 */
export function useViewsMenu(): readonly MenuEntry[] {
  const live = useMode().data?.kind === "live"
  const scope = useStudioScope()
  const enabled = live && scope.isSuccess && scope.data !== null
  const activeScope = scope.data ?? null
  const list = useQuery({
    queryKey: ["views", "all", activeScope?.orgId, activeScope?.environmentId],
    queryFn: () => views.listAll(activeScope),
    enabled,
    retry: false,
    staleTime: 30_000,
  })
  if (!enabled || !list.data) return []
  return list.data.map((stored) => ({
    href: `/views/${encodeURIComponent(stored.id)}`,
    label: stored.view.title || stored.id,
    icon: Bookmark,
    ...(stored.view.space ? { group: stored.view.space } : {}),
  }))
}

/** The object type a question counts, which is how the list groups questions. */
const questionSubject = ({ objectType }: QuestionAnswer) => conceptTitle(objectType.document.name)

/** The questions on the board, for the submenu island. */
export function useQuestionsMenu(): readonly MenuEntry[] | undefined {
  const board = useQuery({ queryKey: ["question-board"], queryFn: transport.questionBoard })
  return board.data && [
    ...board.data.map((read) => ({ href: `/metrics/${read.question.document.name}`, label: headline(read.question.document), group: questionSubject(read) })),
    { href: "#corrections", label: "Corrections", group: "Corrections" },
  ]
}

/** The admitted lifecycles, for the submenu island. */
export function useWorkflowsMenu(): readonly MenuEntry[] | undefined {
  const definitions = useQuery({ queryKey: ["definitions"], queryFn: transport.definitions })
  const flows = useRunTypeFlows()
  return definitions.data && workflows(definitions.data.map(({ document }) => document))
    .map((workflow) => ({ href: `#${workflowAnchor(workflow)}`, label: workflowTitle(workflow, flows) }))
}

/** The recorded proof documents, for the sidebar submenu when no cell is connected. */
export function useRulesMenu(): readonly MenuEntry[] | undefined {
  const recorded = useMode().data?.kind === "recorded"
  const read = useQuery({ queryKey: ["guarantees"], queryFn: transport.guarantees, enabled: recorded })
  return read.data?.map(({ name }) => ({ href: `#guarantee-${name}`, label: GUARANTEE_TITLES[name] ?? name }))
}
