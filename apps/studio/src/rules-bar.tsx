import { Link } from "@tanstack/react-router"
import { TickBar } from "@/components/system/tick-bar"
import { useMode } from "@/lib/live-transport"
import { useCatalog } from "@/lib/use-catalog"
import { lawHeadline } from "./lib/catalog.ts"
import { heldCountText, uniqueLaws } from "./lib/guarantees.ts"
import { rulesSummary } from "./lib/rules-summary.ts"
import { useInbox, type Group } from "./inbox.tsx"

/**
 * The top bar's rules tick bar (DESIGN.md 9.1 and 12.4) on a live cell, from the server only: the
 * laws the catalog lists, and what the inbox says each is holding back. The ticks fill in the held
 * tone for the share of rules holding records back; the rest stay hairlines, because the cell sends
 * no "satisfied" verdict and Studio never infers one. With no live cell, no laws, or the catalog
 * still on its way, it renders nothing.
 */
export function RulesTickBar({ className }: { readonly className?: string }) {
  const live = useMode().data?.kind === "live"
  const { catalog } = useCatalog()
  const inbox = useInbox()
  if (!live || catalog === undefined) return null
  const laws = uniqueLaws(catalog.concepts)
  if (laws.length === 0) return null
  const document = inbox.data?.document
  const groups = document?.items.filter((item): item is Group => item.kind === "held_back")
  const section = document?.sections.find((entry) => entry.kind === "held_back")
  const summary = rulesSummary(laws, groups, section)
  const total = laws.length
  const holding = heldCountText(summary.holding, summary.partial)
  const state = inbox.isError
    ? "what they hold back could not be read"
    : document === undefined ? "reading what they hold back"
      : summary.holding > 0 ? `${summary.partial ? "at least " : ""}${summary.holding} holding records back`
        : summary.clear ? "nothing held back" : "none listed holding records back"
  const note = inbox.isError
    ? "What the rules hold back could not be read. The rules still apply; open Rules to retry."
    : document === undefined ? "Reading what the rules hold back…"
      : summary.partial ? "The cell stopped at its scan bound, so these counts are lower bounds." : undefined
  return (
    <TickBar
      className={className}
      label="Rules"
      name={`Rules: ${total}, ${state}`}
      total={total}
      marked={summary.holding}
      tone="held"
      count={summary.holding > 0 ? `${holding}/${total}` : total}
      heading={`${total} ${total === 1 ? "rule" : "rules"} in this workspace`}
      note={note}
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
