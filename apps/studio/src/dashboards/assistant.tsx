import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Link, useNavigate } from "@tanstack/react-router"
import { Check, CircleCheck, CircleX, Sparkles, X } from "lucide-react"
import { useState } from "react"
import { ValidationPanel } from "@/components/editor"
import { Well } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Textarea } from "@/components/ui/textarea"
import { dashboardField, type Dashboard } from "@/lib/definitions"
import type { QuestionCatalog } from "../builder.tsx"

/**
 * The dashboard assistant in Studio. The person says what they want; the
 * server proposes a typed dashboard that the cell's checker has already
 * decided, and the person accepts it (installed as them, signing the
 * checker's words) or declines it. Studio shows only what the server says.
 */

export type ProposalState = "pending" | "refused" | "accepted" | "declined"
export interface Proposal {
  readonly id: string
  readonly request: string
  readonly targetId: string
  readonly targetVersion: number | null
  readonly definition: Dashboard
  readonly verdict: { readonly admitted: boolean; readonly findings: readonly { readonly code: string; readonly message: string; readonly pointer: string | null }[]; readonly restatement: string | null; readonly restatementSha256: string | null; readonly note: string }
  readonly state: ProposalState
  readonly model: string
  readonly installed: { readonly id: string; readonly version: number } | null
  readonly costMicroUsd: number
  readonly createdAtMs: number
}
interface Listing { readonly proposals: readonly Proposal[]; readonly spentMicroUsd: number; readonly capMicroUsd: number; readonly model: string | null }

const BASE = "/api/agent/dashboards"
async function call<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${BASE}${path}`, { ...init, headers: { accept: "application/json", ...init?.body ? { "content-type": "application/json" } : {} } })
  const body = await response.json().catch(() => null) as (T & { error?: string }) | null
  if (!response.ok || body === null) throw new Error(body?.error ?? (response.status === 401 ? "Sign in again." : response.status === 404 ? "The assistant isn't served here." : `The server did not answer (${response.status}).`))
  return body
}

const STATES: Record<ProposalState, { readonly label: string; readonly variant: "secondary" | "outline" | "destructive" }> = {
  pending: { label: "Admitted, awaiting you", variant: "secondary" },
  refused: { label: "Refused by the checker", variant: "destructive" },
  accepted: { label: "Accepted", variant: "outline" },
  declined: { label: "Declined", variant: "outline" },
}
export const dollars = (micro: number) => micro < 10_000 ? "under $0.01" : `$${(micro / 1_000_000).toFixed(2)}`

/** Three requests the catalog can answer, worded from its own labels: never a name it does not declare. */
export function examples(catalog: QuestionCatalog): readonly string[] {
  const [first, second] = catalog.metrics.filter(({ type }) => type !== "ratio")
  const ratio = catalog.metrics.find(({ type }) => type === "ratio")
  const category = catalog.properties.find(({ kind }) => kind === "text" || kind === "enum")?.name.replaceAll("_", " ")
  return [
    first && `A weekly overview of ${first.label}${second ? ` and ${second.label}` : ""}, with a headline total for each`,
    ratio && category && `${ratio.label} by ${category}, and how it moved week by week`,
    first && category && `Which ${category} contributes most to ${first.label}?`,
  ].filter((text): text is string => typeof text === "string")
}

function Verdict({ proposal }: { readonly proposal: Proposal }) {
  const { verdict } = proposal
  if (!verdict.admitted) return <ValidationPanel title="The cell's checker refused this proposal" issues={verdict.findings.map(({ code, message, pointer }) => ({ code, message, field: dashboardField(pointer) }))} />
  return (
    <div className="space-y-2 rounded-lg border bg-card p-3 text-sm shadow-elevation">
      <div className="flex items-center gap-2 font-medium"><CircleCheck className="size-4 text-success" />The cell's checker admitted this dashboard</div>
      {verdict.restatement && (
        <>
          <p className="text-meta text-muted-foreground">In the checker's own words{proposal.state === "pending" ? "; accepting signs exactly these words" : ""}:</p>
          <Well className="px-3 py-2"><pre className="text-meta leading-relaxed whitespace-pre-wrap" data-restatement>{verdict.restatement}</pre></Well>
        </>
      )}
    </div>
  )
}

/** One proposal: what was asked, the checker's verdict, a live preview of what accepting installs, and the person's decision. */
export function ProposalCard({ proposal, preview }: { readonly proposal: Proposal; readonly preview: (dashboard: Dashboard) => React.ReactNode }) {
  const client = useQueryClient()
  const navigate = useNavigate()
  const refresh = () => client.invalidateQueries({ queryKey: ["dashboard-proposals"] })
  const accept = useMutation({
    mutationFn: () => call<Proposal>(`/${proposal.id}/accept`, { method: "POST", body: "{}" }),
    onSuccess: (decided) => {
      void refresh(); void client.invalidateQueries({ queryKey: ["dashboards"] })
      if (decided.installed) void navigate({ to: "/dashboards/$id", params: { id: decided.installed.id } })
    },
  })
  const decline = useMutation({ mutationFn: () => call<Proposal>(`/${proposal.id}/decline`, { method: "POST", body: "{}" }), onSuccess: () => void refresh() })
  const state = STATES[proposal.state]
  return (
    <article className="space-y-3 rounded-xl border bg-card p-4 shadow-elevation" data-proposal={proposal.id} data-state={proposal.state}>
      <header className="flex flex-wrap items-start gap-2">
        <Sparkles aria-hidden className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
        <div className="min-w-0 flex-1 space-y-1">
          <h3 className="font-medium">{proposal.definition.title}</h3>
          <p className="text-meta text-muted-foreground">You asked: “{proposal.request}”</p>
        </div>
        <Badge variant={state.variant}>{state.label}</Badge>
      </header>
      {proposal.verdict.note && <p className="text-sm">{proposal.verdict.note}</p>}
      <Verdict proposal={proposal} />
      {proposal.verdict.admitted && proposal.state !== "declined" && preview(proposal.definition)}
      {accept.isError && <p role="alert" className="text-sm text-destructive">Not accepted. {accept.error.message}</p>}
      {decline.isError && <p role="alert" className="text-sm text-destructive">Not declined. {decline.error.message}</p>}
      <footer className="flex flex-wrap items-center gap-2 border-t pt-3">
        <span className="text-meta text-muted-foreground">{proposal.model} · {dollars(proposal.costMicroUsd)}</span>
        {proposal.state === "accepted" && proposal.installed && <Button asChild size="sm" variant="outline" className="ms-auto"><Link to="/dashboards/$id" params={{ id: proposal.installed.id }}>Open version {proposal.installed.version}</Link></Button>}
        {proposal.state === "pending" && (
          <div className="ms-auto flex gap-2">
            <Button size="sm" variant="ghost" disabled={decline.isPending || accept.isPending} onClick={() => decline.mutate()}><X />Decline</Button>
            <Button size="sm" disabled={accept.isPending || decline.isPending} onClick={() => accept.mutate()}><Check />{accept.isPending ? "Saving…" : proposal.targetVersion === null ? "Accept and save" : "Accept the change"}</Button>
          </div>
        )}
        {proposal.state === "refused" && <span className="ms-auto flex items-center gap-1 text-meta text-muted-foreground"><CircleX className="size-3.5" />Nothing was saved. Rephrase and ask again.</span>}
      </footer>
    </article>
  )
}

/**
 * Ask for a dashboard, or (with `dashboardId`) for a change to one: the box,
 * examples drawn from the catalog, the newest proposal in full and the
 * earlier ones below it.
 */
export function DashboardAssistant({ catalog, dashboardId, preview }: { readonly catalog: QuestionCatalog; readonly dashboardId?: string; readonly preview: (dashboard: Dashboard) => React.ReactNode }) {
  const client = useQueryClient()
  const [request, setRequest] = useState("")
  const listing = useQuery({ queryKey: ["dashboard-proposals"], queryFn: () => call<Listing>(""), retry: false })
  const ask = useMutation({
    mutationFn: (text: string) => call<Proposal>("", { method: "POST", body: JSON.stringify({ request: text, ...dashboardId ? { dashboardId } : {} }) }),
    onSuccess: () => { setRequest(""); void client.invalidateQueries({ queryKey: ["dashboard-proposals"] }) },
  })
  // New dashboards here; on a board, only the changes proposed to it.
  const proposals = (listing.data?.proposals ?? []).filter((proposal) => dashboardId === undefined ? proposal.targetVersion === null : proposal.targetVersion !== null && proposal.targetId === dashboardId)
  const [latest, ...earlier] = proposals
  const capped = listing.data !== undefined && listing.data.spentMicroUsd >= listing.data.capMicroUsd
  return (
    <section className="space-y-4" aria-label="Dashboard assistant">
      <div className="space-y-3 rounded-xl border bg-card p-4 shadow-elevation">
        <label className="grid gap-2 text-sm">
          <span className="flex items-center gap-2 font-medium"><Sparkles aria-hidden className="size-4 text-muted-foreground" />{dashboardId ? "Ask the assistant to change this dashboard" : "Ask for a dashboard"}</span>
          <Textarea value={request} maxLength={2000} placeholder={dashboardId ? "e.g. Show the trend as bars and add a total" : "Describe what you want to see"} onChange={(event) => setRequest(event.target.value)}
            onKeyDown={(event) => { if (event.key === "Enter" && (event.metaKey || event.ctrlKey) && request.trim() !== "") ask.mutate(request.trim()) }} />
        </label>
        {!dashboardId && (
          <div className="flex flex-wrap gap-2">
            {examples(catalog).map((example) => <Button key={example} type="button" size="sm" variant="outline" className="h-auto py-1.5 text-start whitespace-normal" onClick={() => setRequest(example)}>{example}</Button>)}
          </div>
        )}
        <div className="flex flex-wrap items-center gap-2">
          <p className="text-meta text-muted-foreground">The server proposes; its checker decides; nothing is saved until you accept.</p>
          <Button className="ms-auto" size="sm" disabled={ask.isPending || request.trim() === "" || capped} onClick={() => ask.mutate(request.trim())}><Sparkles />{ask.isPending ? "Proposing…" : "Propose"}</Button>
        </div>
        {capped && <p role="status" className="text-sm text-muted-foreground">The assistant has reached its spending cap.</p>}
        {ask.isError && <p role="alert" className="text-sm text-destructive">{ask.error.message}</p>}
        {listing.isError && <p role="status" className="text-sm text-muted-foreground">{listing.error.message}</p>}
      </div>
      {ask.isPending && <div className="h-24 animate-pulse rounded-xl border bg-muted/40" aria-label="Proposing" />}
      {latest && <ProposalCard key={latest.id} proposal={latest} preview={preview} />}
      {earlier.length > 0 && (
        <details className="rounded-xl border bg-card p-3 text-sm">
          <summary className="cursor-pointer font-medium">Earlier proposals ({earlier.length})</summary>
          <div className="mt-3 space-y-3">{earlier.map((proposal) => <ProposalCard key={proposal.id} proposal={proposal} preview={() => null} />)}</div>
        </details>
      )}
    </section>
  )
}
