import { useQuery } from "@tanstack/react-query"
import { useRouter } from "@tanstack/react-router"
import { ChevronRight, CircleAlert, Inbox as InboxIcon, Plug, ShieldAlert, Wand2, type LucideIcon } from "lucide-react"
import type { MouseEvent, ReactNode } from "react"
import { EmptyState } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Skeleton } from "@/components/ui/skeleton"
import { conceptTitle } from "./lib/catalog.ts"
import { cellRead, NotServed, useMode } from "./lib/live-transport.ts"
import type { Schemas } from "./lib/transport.ts"

type Inbox = Schemas["NornInbox"]
type Item = Schemas["NornInboxItem"]
type Group = Extract<Item, { kind: "held_back" }>
type Decision = Extract<Item, { kind: "decision" }>
type Source = Extract<Item, { kind: "source_error" }>

/** A server-given Studio route (it may carry a query), followed in the app; a modified click keeps the browser's own. */
function RouteLink({ to, children, className }: { readonly to: string; readonly children: ReactNode; readonly className?: string }) {
  const router = useRouter()
  const follow = (event: MouseEvent<HTMLAnchorElement>) => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return
    event.preventDefault()
    router.history.push(to)
  }
  return <a href={to} onClick={follow} className={className}>{children}</a>
}

const text = (value: unknown) => typeof value === "string" && value !== "" ? value : undefined

/** The cell's work queue for this caller (`GET /v1/inbox`): what needs them, each item one click from where to act. */
export function Inbox() {
  const live = useMode().data?.kind === "live"
  const inbox = useQuery({ queryKey: ["inbox"], queryFn: () => cellRead<Inbox>("/v1/inbox", "The inbox is"), retry: false, enabled: live, staleTime: 30_000 })
  if (!live) return null
  const document = inbox.data?.document
  const groups = document?.items.filter((item): item is Group => item.kind === "held_back") ?? []
  const decisions = document?.items.filter((item): item is Decision => item.kind === "decision") ?? []
  const sources = document?.items.filter((item): item is Source => item.kind === "source_error") ?? []
  const section = (kind: Schemas["NornInboxKind"]) => document?.sections.find((entry) => entry.kind === kind)
  const total = (document?.sections ?? []).reduce((sum, entry) => sum + entry.count, 0)
  return (
    <section aria-labelledby="needs-attention" className="space-y-3">
      <div className="flex items-baseline gap-2">
        <h2 id="needs-attention" className="text-section font-medium">Needs attention</h2>
        {document && total > 0 && <span className="text-meta text-muted-foreground tabular-nums">{total}{document.sections.some((entry) => entry.approximate) ? "+" : ""}</span>}
      </div>
      {inbox.isPending && <Skeleton className="h-24 rounded-lg" />}
      {inbox.isError && (inbox.error instanceof NotServed
        ? <EmptyState icon={InboxIcon} title="Not served yet">This cell does not serve a work queue yet.</EmptyState>
        : <p role="alert" className="text-sm text-destructive">{inbox.error.message}</p>)}
      {document && total === 0 && <div className="rounded-lg border"><EmptyState icon={InboxIcon} title="Nothing needs you">No record is held back, no decision waits on you, and every source synced.</EmptyState></div>}
      {/* One frame, ruled into groups: each group's head names what is wrong, its rows say where. */}
      {document && total > 0 && (
        <div className="divide-y overflow-hidden rounded-lg border bg-card">
          {groups.map((group) => <HeldBack key={`${group.ruleRef.kind}:${group.title}:${group.objectType}`} group={group} />)}
          {decisions.length > 0 && (
            <div>
              <GroupHead icon={CircleAlert} title="Decisions waiting on you" count={section("decision")?.count ?? decisions.length} />
              <div className="pb-1">
                {decisions.map(({ decision, target }) => (
                  <RouteLink key={decision.id} to={target} className={rowLink}>
                    <span className="min-w-0 flex-1 truncate">{decision.question}</span>
                    <span className="shrink-0 text-xs text-muted-foreground">{decision.flow.title ?? decision.step.label ?? ""}</span>
                    <ChevronRight aria-hidden className="size-4 shrink-0 text-muted-foreground" />
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
                    <span className="min-w-0 max-w-[50%] truncate text-xs text-muted-foreground">{text(source.values.last_error_message) ?? text(source.values.status) ?? ""}</span>
                    <ChevronRight aria-hidden className="size-4 shrink-0 text-muted-foreground" />
                  </RouteLink>
                ))}
              </div>
            </div>
          )}
        </div>
      )}
    </section>
  )
}

const rowLink = "flex items-center gap-3 px-4 py-1.5 ps-10 text-sm hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"

/** A group's head row: a small muted icon, what the group is, its count, and an optional action at the end. */
function GroupHead({ icon: Icon, title, count, description, children }: { readonly icon: LucideIcon; readonly title: ReactNode; readonly count?: number; readonly description?: ReactNode; readonly children?: ReactNode }) {
  return (
    <div className="flex items-start gap-3 px-4 pt-3 pb-1.5">
      <Icon aria-hidden className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
      <div className="min-w-0 flex-1">
        <h3 className="text-sm font-medium">{title}{count !== undefined && <span className="ms-2 font-normal text-muted-foreground tabular-nums">{count}</span>}</h3>
        {description && <p className="text-meta text-muted-foreground">{description}</p>}
      </div>
      {children && <div className="flex shrink-0 items-center gap-1">{children}</div>}
    </div>
  )
}

/** One law and the records it holds back: open it for each record, its finding in words and the fix the server suggests. */
function HeldBack({ group }: { readonly group: Group }) {
  return (
    <Collapsible defaultOpen={group.records.length <= 3}>
      <GroupHead icon={ShieldAlert} title={group.title}
        description={<>{group.count}{group.approximate ? "+" : ""} {conceptTitle(group.objectType).toLowerCase()} {group.count === 1 ? "record" : "records"} held back</>}>
        <Button asChild size="sm" variant="outline"><RouteLink to={group.target}>Review</RouteLink></Button>
        <CollapsibleTrigger asChild><Button size="icon-sm" variant="ghost" aria-label="Show the records" className="data-[state=open]:rotate-90"><ChevronRight /></Button></CollapsibleTrigger>
      </GroupHead>
      <CollapsibleContent className="pb-1.5">
        {group.records.map((record) => {
          const name = text(record.values?.label) ?? text(record.values?.display_name) ?? text(record.values?.title) ?? record.id
          // The record's other open findings, or laws a fix would break: the server offers no fix then.
          const others = (record as { readonly otherFindings?: readonly Schemas["NornInboxFinding"][] }).otherFindings ?? []
          return (
            <div key={`${record.objectType}:${record.id}`} className="flex min-h-9 flex-wrap items-center gap-x-3 gap-y-1 px-4 py-1 ps-11 text-sm">
              <RouteLink to={record.target} className="font-medium underline-offset-4 hover:underline">{name}</RouteLink>
              {record.finding && <span className="min-w-0 flex-1 text-muted-foreground tabular-nums">{record.finding.words}</span>}
              {others.length > 0 && (
                <div className="basis-full space-y-0.5 text-xs text-muted-foreground">
                  <Badge variant="outline">Needs review: {others.length + 1} laws disagree</Badge>
                  {others.map((other) => <p key={other.id}>{other.words}</p>)}
                </div>
              )}
              {record.suggestedFix && (
                <Button asChild size="xs" variant="secondary" className="ms-auto" title={record.suggestedFix.title}>
                  <RouteLink to={record.target}><Wand2 aria-hidden />{record.suggestedFix.label}</RouteLink>
                </Button>
              )}
            </div>
          )
        })}
      </CollapsibleContent>
    </Collapsible>
  )
}
