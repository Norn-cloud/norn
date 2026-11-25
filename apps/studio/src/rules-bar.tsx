import type { UseQueryResult } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { TickBar } from "@/components/system/tick-bar"
import { readStateOf } from "@/lib/cell-query"
import { matchReadState, matchWorkspaceState, type ReadState } from "@/lib/state"
import { useCatalog } from "@/lib/use-catalog"
import { useWorkspace } from "@/lib/workspace"
import { lawHeadline } from "./lib/catalog.ts"
import { heldCountText, uniqueLaws } from "./lib/guarantees.ts"
import { rulesSummary, type RulesSummary } from "./lib/rules-summary.ts"
import { useInbox, type Group } from "./inbox.tsx"

/** One query as the read state a page shows (`lib/state`): pending, its error's state, or its answer. */
function stateOf<Q, T>(query: Pick<UseQueryResult<Q, Error>, "status" | "data" | "error">, document: (data: Q) => T): ReadState<T> {
  if (query.status === "error" && query.error !== null) return readStateOf(query.error)
  if (query.status === "pending" || query.data === undefined) return { kind: "pending" }
  return { kind: "ok", document: document(query.data), freshness: "snapshot" }
}

/**
 * The top bar's rules tick bar (DESIGN.md 9.1 and 12.4), on a live workspace only: the recording and an
 * undecided or unavailable workspace show none (`matchWorkspaceState`).
 */
export function RulesTickBar({ className }: { readonly className?: string }) {
  return matchWorkspaceState(useWorkspace(), {
    deciding: () => null,
    unavailable: () => null,
    recorded: () => null,
    live: () => <LiveRulesTickBar className={className} />,
  })
}

/** What the inbox read says about the rules, in the bar's name and in a note above the list. */
interface InboxWords { readonly state: string; readonly note?: string; readonly summary: RulesSummary }

/**
 * From the server only: the laws the catalog lists, and what the inbox says each is holding back. The
 * ticks fill in the held tone for the share of rules holding records back; the rest stay hairlines,
 * because the cell sends no "satisfied" verdict and Studio never infers one. Until the catalog has
 * answered, or when it lists no laws, there is no bar (the catalog's own pages say why).
 */
function LiveRulesTickBar({ className }: { readonly className?: string }) {
  const { read, catalog } = useCatalog()
  const inbox = useInbox()
  const laws = matchReadState(stateOf(read, () => catalog), {
    pending: () => null,
    ok: ({ document }) => document === undefined ? null : uniqueLaws(document.concepts),
    refused: () => null,
    not_served: () => null,
    unavailable: () => null,
    answer_moved: () => null,
    timed_out: () => null,
  })
  if (laws === null || laws.length === 0) return null
  const unread = rulesSummary(laws, undefined, undefined)
  const words: InboxWords = matchReadState(stateOf(inbox, (data) => data.document), {
    pending: () => ({ state: "reading what they hold back", note: "Reading what the rules hold back…", summary: unread }),
    ok: ({ document }) => {
      const summary = rulesSummary(laws, document.items.filter((item): item is Group => item.kind === "held_back"), document.sections.find((entry) => entry.kind === "held_back"))
      return {
        summary,
        state: summary.holding > 0 ? `${summary.partial ? "at least " : ""}${summary.holding} holding records back`
          : summary.clear ? "nothing held back" : "none listed holding records back",
        note: summary.partial ? "The cell stopped at its scan bound, so these counts are lower bounds." : undefined,
      }
    },
    refused: ({ refusal }) => ({ state: "what they hold back could not be read", note: `What the rules hold back could not be read: the cell refused it (${refusal.message}). The rules still apply.`, summary: unread }),
    not_served: () => ({ state: "what they hold back is not served", note: "This cell does not serve what its rules hold back yet. The rules still apply.", summary: unread }),
    unavailable: () => ({ state: "what they hold back could not be read", note: "What the rules hold back could not be read. The rules still apply; open Rules to retry.", summary: unread }),
    answer_moved: () => ({ state: "what they hold back could not be read", note: "What the rules hold back could not be read. The rules still apply; open Rules to retry.", summary: unread }),
    timed_out: () => ({ state: "what they hold back could not be read", note: "What the rules hold back did not answer in time. The rules still apply; open Rules to retry.", summary: unread }),
  })
  const { summary } = words
  const total = laws.length
  return (
    <TickBar
      className={className}
      label="Rules"
      name={`Rules: ${total}, ${words.state}`}
      total={total}
      marked={summary.holding}
      tone="held"
      count={summary.holding > 0 ? `${heldCountText(summary.holding, summary.partial)}/${total}` : total}
      heading={`${total} ${total === 1 ? "rule" : "rules"} in this workspace`}
      note={words.note}
      items={summary.rules.map(({ law, held, approximate }) => ({
        id: law.id,
        title: lawHeadline(law),
        lang: law.titleLang,
        state: held > 0 ? { tone: "held" as const, word: `${heldCountText(held, approximate)} held back` } : undefined,
      }))}
      footer={<Link to="/rules" className="rounded-sm text-link underline-offset-4 outline-none hover:underline focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring">Open Rules</Link>}
    />
  )
}
