import { useQuery, useQueryClient } from "@tanstack/react-query"
import { Link, useRouter } from "@tanstack/react-router"
import { ChevronRight, CircleAlert, Inbox as InboxIcon, Plug, ShieldAlert, type LucideIcon } from "lucide-react"
import { Fragment, type ComponentProps, type MouseEvent, type ReactNode } from "react"
import { LoadState } from "@/components/load-state"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { conceptTitle } from "./lib/catalog.ts"
import { heldRuleTitle } from "./lib/guarantees.ts"
import { useLocale } from "./lib/locale.tsx"
import { cellRead, retryWorkspace, useMode } from "./lib/live-transport.ts"
import { recordName } from "./lib/record-name.ts"
import { formatLocale } from "./lib/timeline.ts"
import type { Schemas } from "./lib/transport.ts"

export type Inbox = Schemas["NornInbox"]
type Item = Schemas["NornInboxItem"]
export type Group = Extract<Item, { kind: "held_back" }>
type Decision = Extract<Item, { kind: "decision" }>
type Source = Extract<Item, { kind: "source_error" }>

/** A server-given Studio route (it may carry a query), followed in the app; a modified click keeps the browser's own. */
export function RouteLink({ to, children, className, ...rest }: {
  readonly to: string
  readonly children: ReactNode
  readonly className?: string
} & Omit<ComponentProps<"a">, "href" | "onClick" | "children" | "className">) {
  const router = useRouter()
  const follow = (event: MouseEvent<HTMLAnchorElement>) => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return
    event.preventDefault()
    router.history.push(to)
  }
  return <a {...rest} href={to} onClick={follow} className={className}>{children}</a>
}

const text = (value: unknown) => typeof value === "string" && value !== "" ? value : undefined

/** The cell's scoped inbox (`GET /v1/inbox`), shared by Home, Inbox and Rules. */
export function useInbox() {
  const live = useMode().data?.kind === "live"
  return useQuery({ queryKey: ["inbox"], queryFn: () => cellRead<Inbox>("/v1/inbox", "The inbox is"), retry: false, enabled: live, staleTime: 30_000 })
}

/** The cell's work queue for this caller (`GET /v1/inbox`): what needs them, each item one click from where to act. */
export function Inbox() {
  const queries = useQueryClient()
  const live = useMode().data?.kind === "live"
  const inbox = useInbox()
  if (!live) return null
  const document = inbox.data?.document
  const groups = document?.items.filter((item): item is Group => item.kind === "held_back") ?? []
  const decisions = document?.items.filter((item): item is Decision => item.kind === "decision") ?? []
  const sources = document?.items.filter((item): item is Source => item.kind === "source_error") ?? []
  const section = (kind: Schemas["NornInboxKind"]) => document?.sections.find((entry) => entry.kind === kind)
  const total = (document?.sections ?? []).reduce((sum, entry) => sum + entry.count, 0)
  const anyApproximate = document?.sections.some((entry) => entry.approximate) ?? false
  return (
    <section aria-labelledby="needs-attention" className="space-y-3">
      <div className="flex flex-wrap items-baseline gap-2">
        <h2 id="needs-attention" className="text-heading font-medium">Needs attention</h2>
        {document && (total > 0 || anyApproximate) && <span className="text-meta text-muted-foreground tabular-nums">{total}{anyApproximate ? "+" : ""}</span>}
        <Link to="/rules" search={{ view: "held" } as never} className="ms-auto text-meta text-muted-foreground underline-offset-4 hover:text-foreground hover:underline">Held by rules</Link>
      </div>
      <LoadState
        status={inbox.status}
        data={document}
        error={inbox.error}
        isFetching={inbox.isFetching}
        onRetry={() => void retryWorkspace(queries).then((ok) => { if (ok) void inbox.refetch({ cancelRefetch: false }) })}
        bone="home-inbox"
        skeleton="list"
        loadingLabel="Reading what needs attention"
        isEmpty={() => total === 0}
        empty={anyApproximate
          ? {
              icon: ShieldAlert,
              title: "None found within the scan bound",
              description: `No held-back record, waiting decision or sync error was found before the scan bound${document?.scanLimit ? ` (${document.scanLimit.toLocaleString("en-US")} rows)` : ""} was reached. More records were not scanned.`,
            }
          : {
              icon: InboxIcon,
              title: "Nothing needs you",
              description: "No record is held back, no decision waits on you, and every source synced.",
              next: <Button asChild size="sm" variant="outline"><RouteLink to="/sources">Browse sources and records</RouteLink></Button>,
            }}
        notServed={{ icon: InboxIcon, title: "Not served yet", description: "This cell does not serve a work queue yet." }}
      >
        {() => (
          /* One frame, ruled into groups: each group's head names what is wrong, its rows say where. */
          <div className="divide-y overflow-hidden rounded-lg border bg-card">
            {groups.map((group) => <HeldBack key={`${group.ruleRef.kind}:${group.title}:${group.objectType}`} group={group} />)}
            {decisions.length > 0 && (
              <div>
                <GroupHead icon={CircleAlert} title="Decisions waiting on you" count={section("decision")?.count ?? decisions.length} />
                <div className="pb-1">
                  {decisions.map(({ decision, target }) => (
                    <RouteLink key={decision.id} to={target} className={rowLink}>
                      <span className="min-w-0 flex-1 truncate">{decision.question}</span>
                      <span className="shrink-0 text-meta text-muted-foreground">{decision.flow.title ?? decision.step.label ?? ""}</span>
                      <ChevronRight aria-hidden className="size-4 shrink-0 text-muted-foreground rtl:rotate-180" />
                    </RouteLink>
                  ))}
                </div>
              </div>
            )}
            {sources.length > 0 && (
              <div>
                <GroupHead icon={Plug} title="Sources that did not sync" count={section("source_error")?.count ?? sources.length} />
                <div className="pb-1">
                  {sources.map((source) => (
                    <RouteLink key={source.id} to={source.target} className={rowLink}>
                      <span className="min-w-0 flex-1 truncate">{text(source.values.label) ?? source.id}</span>
                      <span className="min-w-0 max-w-[50%] truncate text-meta text-muted-foreground">{text(source.values.last_error_message) ?? text(source.values.status) ?? ""}</span>
                      <ChevronRight aria-hidden className="size-4 shrink-0 text-muted-foreground rtl:rotate-180" />
                    </RouteLink>
                  ))}
                </div>
              </div>
            )}
          </div>
        )}
      </LoadState>
    </section>
  )
}

const rowLink = "flex items-center gap-3 px-4 py-1.5 ps-10 text-body max-sm:min-h-[44px] hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"

/**
 * A group's head row: a small muted icon, what the group is (one title line that tells it apart from
 * its neighbours), its count, and its actions. On a phone the actions sit under the title, so a 44px
 * pill never squeezes the title into a column.
 */
function GroupHead({ icon: Icon, title, count, description, children }: { readonly icon: LucideIcon; readonly title: ReactNode; readonly count?: ReactNode; readonly description?: ReactNode; readonly children?: ReactNode }) {
  return (
    <div className="flex items-start gap-3 px-4 pt-3 pb-1.5">
      <Icon aria-hidden strokeWidth={1.6} className="mt-[3px] size-4 shrink-0 text-muted-foreground" />
      <div className="flex min-w-0 flex-1 flex-col gap-2 sm:flex-row sm:items-start sm:gap-3">
        <div className="min-w-0 flex-1">
          <h3 className="text-body text-foreground text-pretty">{title}{count !== undefined && <span className="ms-2 text-muted-foreground tabular-nums">{count}</span>}</h3>
          {description && <p className="text-meta text-muted-foreground">{description}</p>}
        </div>
        {children && <div className="flex shrink-0 items-center gap-1">{children}</div>}
      </div>
    </div>
  )
}

/**
 * A finding's words with each side of its comparison kept whole: a line breaks before the operator,
 * never inside "ledger entry 140.00". Each side is an inline block, so it moves to the next line as
 * one piece and wraps inside only when it is wider than the line itself.
 */
export function ComparisonWords({ words }: { readonly words: string }) {
  const sides = words.split(/\s(?=[≠=<>≤≥]\s)/)
  return <>{sides.map((side, index) => <Fragment key={index}>{index > 0 && " "}<span className="inline-block">{side}</span></Fragment>)}</>
}

/**
 * One law and the records it holds back. Two laws can share a title, so the title line also names the
 * record type and how many records it holds. Each record is one row of a three-column grid: its name,
 * its finding in words (one column for every record, so findings start at one x), then its fix.
 */
function HeldBack({ group }: { readonly group: Group }) {
  const { locale } = useLocale()
  const held = `${group.count.toLocaleString(formatLocale(locale))}${group.approximate ? "+" : ""}`
  return (
    <Collapsible defaultOpen={group.records.length <= 3}>
      <GroupHead icon={ShieldAlert}
        title={<><span>{heldRuleTitle(group)}</span><span data-held-type="" className="text-muted-foreground"> · {conceptTitle(group.objectType)} · <span className="whitespace-nowrap tabular-nums">{held} held back</span></span></>}>
        <Button asChild size="sm" variant="outline"><RouteLink to={group.target}>Review</RouteLink></Button>
        <CollapsibleTrigger asChild><Button size="icon-sm" variant="ghost" aria-label="Show the records" className="data-open:rotate-90 max-sm:size-[44px]"><ChevronRight /></Button></CollapsibleTrigger>
      </GroupHead>
      <CollapsibleContent className="pb-1.5">
        {group.records.map((record) => {
          const name = recordName(record)
          // The record's other open findings, or laws a fix would break: the server offers no fix then.
          const others = record.otherFindings ?? []
          return (
            <div key={`${record.objectType}:${record.id}`} data-held-record={record.id}
              className="grid min-h-9 gap-x-4 gap-y-1 px-4 py-1.5 ps-11 text-body sm:grid-cols-[minmax(0,14rem)_minmax(0,1fr)_auto] sm:items-baseline">
              <RouteLink to={record.target} title={name} className="min-w-0 truncate text-foreground underline-offset-4 hover:underline">{name}</RouteLink>
              <div className="min-w-0 space-y-1 text-muted-foreground tabular-nums">
                {record.finding && <p data-finding=""><ComparisonWords words={record.finding.words} /></p>}
                {others.length > 0 && <>
                  <Badge variant="warning" className="h-auto text-body">Needs review: {others.length + 1} laws disagree</Badge>
                  {others.map((other) => <p key={other.id}><ComparisonWords words={other.words} /></p>)}
                </>}
              </div>
              {record.suggestedFix
                ? (
                  <Button asChild size="xs" variant="secondary" className="justify-self-start sm:justify-self-end max-sm:min-h-[44px]">
                    <RouteLink to={record.target}>Review fix</RouteLink>
                  </Button>
                )
                : <span aria-hidden className="max-sm:hidden" />}
            </div>
          )
        })}
      </CollapsibleContent>
    </Collapsible>
  )
}
