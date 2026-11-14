import { useQuery } from "@tanstack/react-query"
import { useRouter } from "@tanstack/react-router"
import { ChevronRight, CircleAlert, Inbox as InboxIcon, Plug, ShieldAlert, Wand2 } from "lucide-react"
import type { MouseEvent, ReactNode } from "react"
import { EmptyState } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
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
        {document && total > 0 && <Badge variant="secondary" className="tabular-nums">{total}{document.sections.some((entry) => entry.approximate) ? "+" : ""}</Badge>}
      </div>
      {inbox.isPending && <Skeleton className="h-24 rounded-xl" />}
      {inbox.isError && (inbox.error instanceof NotServed
        ? <EmptyState icon={InboxIcon} title="Not served yet">This cell does not serve a work queue yet.</EmptyState>
        : <p role="alert" className="text-sm text-destructive">{inbox.error.message}</p>)}
      {document && total === 0 && <EmptyState icon={InboxIcon} title="Nothing needs you">No record is held back, no decision waits on you, and every source synced.</EmptyState>}
      {groups.map((group) => <HeldBack key={`${group.ruleRef.kind}:${group.title}:${group.objectType}`} group={group} />)}
      {decisions.length > 0 && (
        <Card size="sm">
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-sm font-medium"><CircleAlert aria-hidden className="size-4 text-muted-foreground" />Decisions waiting on you</CardTitle>
            <CardAction><Badge variant="outline" className="tabular-nums">{section("decision")?.count ?? decisions.length}</Badge></CardAction>
          </CardHeader>
          <CardContent className="divide-y">
            {decisions.map(({ decision, target }) => (
              <RouteLink key={decision.id} to={target} className="flex items-center gap-3 py-2 text-sm hover:text-foreground">
                <span className="min-w-0 flex-1 truncate">{decision.question}</span>
                <span className="shrink-0 text-xs text-muted-foreground">{decision.flow.title ?? decision.step.label ?? ""}</span>
                <ChevronRight aria-hidden className="size-4 shrink-0 text-muted-foreground" />
              </RouteLink>
            ))}
          </CardContent>
        </Card>
      )}
      {sources.length > 0 && (
        <Card size="sm">
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-sm font-medium"><Plug aria-hidden className="size-4 text-muted-foreground" />Sources that did not sync</CardTitle>
            <CardAction><Badge variant="outline" className="tabular-nums">{section("source_error")?.count ?? sources.length}</Badge></CardAction>
          </CardHeader>
          <CardContent className="divide-y">
            {sources.map((source) => (
              <RouteLink key={source.id} to={source.target} className="flex items-center gap-3 py-2 text-sm">
                <span className="min-w-0 flex-1 truncate">{text(source.values.label) ?? source.id}</span>
                <span className="min-w-0 max-w-[50%] truncate text-xs text-muted-foreground">{text(source.values.last_error_message) ?? text(source.values.status) ?? ""}</span>
                <ChevronRight aria-hidden className="size-4 shrink-0 text-muted-foreground" />
              </RouteLink>
            ))}
          </CardContent>
        </Card>
      )}
    </section>
  )
}

/** One law and the records it holds back: open it for each record, its finding in words and the fix the server suggests. */
function HeldBack({ group }: { readonly group: Group }) {
  return (
    <Collapsible asChild defaultOpen={group.records.length <= 3}>
      <Card size="sm">
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-sm font-medium"><ShieldAlert aria-hidden className="size-4 text-muted-foreground" />{group.title}</CardTitle>
          <CardDescription>{group.count}{group.approximate ? "+" : ""} {conceptTitle(group.objectType).toLowerCase()} {group.count === 1 ? "record" : "records"} held back</CardDescription>
          <CardAction className="flex items-center gap-1">
            <Button asChild size="sm" variant="outline"><RouteLink to={group.target}>Review</RouteLink></Button>
            <CollapsibleTrigger asChild><Button size="icon-sm" variant="ghost" aria-label="Show the records" className="data-[state=open]:rotate-90"><ChevronRight /></Button></CollapsibleTrigger>
          </CardAction>
        </CardHeader>
        <CollapsibleContent asChild>
          <CardContent className="divide-y">
            {group.records.map((record) => {
              const name = text(record.values?.doc_number) ?? text(record.values?.label) ?? record.id
              return (
                <div key={`${record.objectType}:${record.id}`} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2 text-sm">
                  <RouteLink to={record.target} className="font-medium underline-offset-4 hover:underline">{name}</RouteLink>
                  {record.finding && <span className="min-w-0 flex-1 text-muted-foreground">{record.finding.words}</span>}
                  {record.suggestedFix && (
                    <Button asChild size="sm" variant="secondary" className="ms-auto" title={record.suggestedFix.title}>
                      <RouteLink to={record.target}><Wand2 aria-hidden />{record.suggestedFix.label}</RouteLink>
                    </Button>
                  )}
                </div>
              )
            })}
          </CardContent>
        </CollapsibleContent>
      </Card>
    </Collapsible>
  )
}
