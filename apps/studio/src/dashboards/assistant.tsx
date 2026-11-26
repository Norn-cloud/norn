import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Link, useNavigate } from "@tanstack/react-router"
import { ArrowUp, Check, CircleCheck, CircleX, CornerDownRight, X } from "lucide-react"
import { useState } from "react"
import { ValidationPanel } from "@/components/editor"
import { Well } from "@/components/system/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Textarea } from "@/components/ui/textarea"
import { CELL_FETCH_TIMEOUT_MS, dashboardField, defaultFetch, fetchWithTimeout, getDisplayedScope, scopeHeaders, type Dashboard, type StudioScope, type Tile } from "@/lib/definitions"
import { usePinnedScope } from "@/lib/session"
import { plainMetricLabel } from "@/lib/plain-words"
import { useView } from "@/lib/view"
import { SafePrompt, type GuardDocument } from "@/components/safe-prompt"
import type { QuestionCatalog } from "../builder.tsx"
import { conceptTitle } from "../lib/catalog.ts"
import { Mascot } from "@/components/system/mascot"

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
  readonly verdict: { readonly admitted: boolean; readonly findings: readonly { readonly code: string; readonly message: string; readonly pointer: string | null }[]; readonly restatement: string | null; readonly restatementSha256: string | null; readonly note: string; readonly guard?: GuardDocument }
  readonly state: ProposalState
  readonly model: string
  readonly installed: { readonly id: string; readonly version: number } | null
  readonly costMicroUsd: number
  readonly createdAtMs: number
}
interface Listing { readonly proposals: readonly Proposal[]; readonly spentMicroUsd: number; readonly capMicroUsd: number; readonly model: string | null }

const BASE = "/api/agent/dashboards"
async function call<T>(path: string, init?: RequestInit, boundScope?: StudioScope | null): Promise<T> {
  const method = (init?.method ?? "GET").toUpperCase()
  const write = method !== "GET" && method !== "HEAD" && method !== "OPTIONS"
  const scope = boundScope !== undefined ? boundScope : getDisplayedScope()
  const requestInit: RequestInit = {
    ...init,
    credentials: "same-origin",
    headers: {
      accept: "application/json",
      ...init?.body ? { "content-type": "application/json" } : {},
      ...scopeHeaders(scope),
    },
  }
  const response = write
    ? await defaultFetch(`${BASE}${path}`, requestInit)
    : await fetchWithTimeout(defaultFetch, `${BASE}${path}`, requestInit, CELL_FETCH_TIMEOUT_MS)
  const body = await response.json().catch(() => null) as (T & { error?: string; message?: string }) | null
  if (!response.ok || body === null) throw new Error(body?.error ?? body?.message ?? (response.status === 401 ? "Sign in again." : response.status === 404 ? "The assistant isn't served here." : `The server did not answer (${response.status}).`))
  return body
}

const STATES: Record<ProposalState, { readonly label: string; readonly variant: "secondary" | "outline" | "destructive" }> = {
  pending: { label: "Admitted, awaiting you", variant: "secondary" },
  refused: { label: "Refused by the checker", variant: "destructive" },
  accepted: { label: "Accepted", variant: "outline" },
  declined: { label: "Declined", variant: "outline" },
}
export const dollars = (micro: number) => micro < 10_000 ? "under $0.01" : `$${(micro / 1_000_000).toFixed(2)}`

/** A catalog label in words: an object type's id inside it (`canon.Holding`) reads as its title (`Holding`). */
export const inWords = (label: string): string => label.replace(/\b(?:[a-z][a-z0-9_]*\.)+[A-Z][A-Za-z0-9]*\b/g, (id) => conceptTitle(id))

/** How a request names a metric: the server's display words, else its label with type ids as titles. */
const spoken = (metric: QuestionCatalog["metrics"][number]) => metric.display ?? inWords(metric.label)

/** Three requests the catalog can answer, worded from the server's own names: never a name it does not declare. */
export function examples(catalog: QuestionCatalog): readonly string[] {
  const [first, second] = catalog.metrics.filter(({ type }) => type !== "ratio").map(spoken)
  const found = catalog.metrics.find(({ type }) => type === "ratio")
  const ratio = found && spoken(found)
  const category = catalog.properties.find(({ kind }) => kind === "text" || kind === "enum")?.name.replaceAll("_", " ")
  return [
    first && `Weekly ${first}${second ? ` and ${second}` : ""}`,
    ratio && category && `${ratio[0]?.toUpperCase() ?? ""}${ratio.slice(1)} by ${category}, week by week`,
    first && category && `Which ${category} has the most ${first}?`,
  ].filter((text): text is string => typeof text === "string")
}

function viewWords(view: Tile["view"]): string {
  if (view.kind === "number") return "single number"
  if (view.kind === "table") return "table"
  return view.chart ? `${view.chart} chart` : "chart"
}

function tileMetrics(tile: Tile, catalog: QuestionCatalog | undefined): readonly string[] {
  const aggregate = (tile.question as { readonly aggregate?: unknown }).aggregate
  if (!Array.isArray(aggregate)) return []
  return aggregate.flatMap((entry) => {
    const id = typeof (entry as { readonly metric?: unknown } | null)?.metric === "string"
      ? (entry as { readonly metric: string }).metric
      : null
    if (id === null) return []
    const metric = catalog?.metrics.find((candidate) => candidate.name === id)
    if (!metric) return [id]
    return [metric.display ?? plainMetricLabel(inWords(metric.label))]
  })
}

function Verdict({ proposal, catalog }: { readonly proposal: Proposal; readonly catalog?: QuestionCatalog }) {
  const { view } = useView()
  const { verdict } = proposal
  if (!verdict.admitted) return <ValidationPanel title="The cell's checker refused this proposal" issues={verdict.findings.map(({ code, message, pointer }) => ({ code, message, field: dashboardField(pointer) }))} />
  const canonical = verdict.restatement
  const tiles = proposal.definition.tiles
  return (
    <div className="space-y-2 rounded-lg border bg-card p-3 text-body shadow-elevation">
      <div className="flex items-center gap-2"><CircleCheck className="size-4 text-success" />The cell's checker admitted this dashboard</div>
      {view === "business" && tiles.length > 0 && (
        <div className="space-y-1 rounded-md border border-border/60 bg-muted/40 px-3 py-2 text-meta" data-proposal-summary>
          <p className="text-foreground">Summary (for reading — accepting signs the checker's exact words below)</p>
          <ul className="list-disc space-y-0.5 ps-4 text-muted-foreground">
            {tiles.map((tile) => {
              const metrics = tileMetrics(tile, catalog)
              return (
                <li key={tile.id}>
                  <span className="text-foreground">{tile.title}</span> — {viewWords(tile.view)}
                  {metrics.length > 0 ? ` (${metrics.join(", ")})` : ""}
                </li>
              )
            })}
          </ul>
        </div>
      )}
      {canonical && (
        <>
          <p className="text-meta text-muted-foreground">In the checker's own words{proposal.state === "pending" ? "; accepting signs exactly these words" : ""}:</p>
          <Well className="px-3 py-2"><pre className="font-sans text-meta leading-relaxed whitespace-pre-wrap" data-restatement>{canonical}</pre></Well>
        </>
      )}
    </div>
  )
}

/** One proposal: what was asked, the checker's verdict, a live preview of what accepting installs, and the person's decision. */
export function ProposalCard({ proposal, catalog, preview }: { readonly proposal: Proposal; readonly catalog?: QuestionCatalog; readonly preview: (dashboard: Dashboard) => React.ReactNode }) {
  const client = useQueryClient()
  const navigate = useNavigate()
  const pinnedScope = usePinnedScope(proposal.id)
  const refresh = () => client.invalidateQueries({ queryKey: ["dashboard-proposals"] })
  const accept = useMutation({
    mutationFn: () => call<Proposal>(`/${proposal.id}/accept`, { method: "POST", body: "{}" }, pinnedScope),
    onSuccess: (decided) => {
      void refresh(); void client.invalidateQueries({ queryKey: ["dashboards"] })
      if (decided.installed) void navigate({ to: "/dashboards/$id", params: { id: decided.installed.id } })
    },
  })
  const decline = useMutation({ mutationFn: () => call<Proposal>(`/${proposal.id}/decline`, { method: "POST", body: "{}" }, pinnedScope), onSuccess: () => void refresh() })
  const state = STATES[proposal.state]
  return (
    <article className="space-y-3 rounded-xl border bg-card p-4 shadow-elevation" data-proposal={proposal.id} data-state={proposal.state}>
      <header className="flex flex-wrap items-start gap-2">
        <Mascot size={20} state={proposal.state === "refused" ? "unavailable" : proposal.state === "pending" ? "asking" : "answered"} className="mt-0.5" />
        <div className="min-w-0 flex-1 space-y-1">
          <h3>{proposal.definition.title}</h3>
          <p className="text-meta text-muted-foreground">You asked: “{proposal.request}”</p>
        </div>
        <Badge variant={state.variant}>{state.label}</Badge>
      </header>
      {proposal.verdict.guard
        ? <SafePrompt guard={proposal.verdict.guard} />
        : proposal.verdict.note && <p className="text-body">{proposal.verdict.note}</p>}
      <Verdict proposal={proposal} catalog={catalog} />
      {proposal.verdict.admitted && proposal.state !== "declined" && preview(proposal.definition)}
      {accept.isError && <p role="alert" className="text-body text-destructive-text">Not accepted. {accept.error.message}</p>}
      {decline.isError && <p role="alert" className="text-body text-destructive-text">Not declined. {decline.error.message}</p>}
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
  const pinnedScope = usePinnedScope(dashboardId ?? "new")
  const listing = useQuery({ queryKey: ["dashboard-proposals"], queryFn: ({ signal }) => call<Listing>("", { signal }, pinnedScope), retry: false })
  const ask = useMutation({
    mutationFn: (text: string) => call<Proposal>("", { method: "POST", body: JSON.stringify({ request: text, ...dashboardId ? { dashboardId } : {} }) }, pinnedScope),
    onSuccess: () => { setRequest(""); void client.invalidateQueries({ queryKey: ["dashboard-proposals"] }) },
  })
  // New dashboards here; on a board, only the changes proposed to it.
  const proposals = (listing.data?.proposals ?? []).filter((proposal) => dashboardId === undefined ? proposal.targetVersion === null : proposal.targetVersion !== null && proposal.targetId === dashboardId)
  const [latest, ...earlier] = proposals
  const capped = listing.data !== undefined && listing.data.spentMicroUsd >= listing.data.capMicroUsd
  return (
    <section className="space-y-4" aria-label="Dashboard assistant">
      <div className="space-y-3 rounded-2xl border bg-card p-3 shadow-elevation">
        <form className="group/prompt rounded-xl border bg-background transition-shadow focus-within:border-foreground/25 focus-within:shadow-sm"
          onSubmit={(event) => { event.preventDefault(); if (request.trim() !== "" && !capped) ask.mutate(request.trim()) }}>
          <p aria-hidden className="flex items-center gap-2 px-3 pt-2.5 text-body">
            <Mascot tone="ink" size={16} className="text-muted-foreground" />{dashboardId ? "Ask the assistant to change this dashboard" : "Ask for a dashboard"}
          </p>
          <Textarea value={request} maxLength={2000} rows={2} aria-label={dashboardId ? "Ask the assistant to change this dashboard" : "Ask for a dashboard"} placeholder={dashboardId ? "e.g. Show the trend as bars and add a total" : "Describe what you want to see"} onChange={(event) => setRequest(event.target.value)}
            onKeyDown={(event) => { if (event.key === "Enter" && (event.metaKey || event.ctrlKey) && request.trim() !== "") ask.mutate(request.trim()) }}
            className="min-h-16 resize-none border-0 bg-transparent px-3 py-2 shadow-none focus-visible:ring-0 dark:bg-transparent" />
          <div className="flex items-center gap-2 px-3 pb-2.5">
            <p className="text-meta text-muted-foreground">The server proposes; its checker decides; nothing is saved until you accept.</p>
            <span className="ms-auto hidden text-meta text-muted-foreground sm:inline">⌘↵</span>
            <Button type="submit" size="sm" className="rounded-full" disabled={ask.isPending || request.trim() === "" || capped}>{ask.isPending ? <Mascot tone="ink" size={16} state="thinking" /> : <ArrowUp />}{ask.isPending ? "Proposing…" : "Propose"}</Button>
          </div>
        </form>
        {!dashboardId && examples(catalog).length > 0 && (
          <div className="space-y-1 px-1">
            <p className="px-2 text-meta text-muted-foreground">Try asking</p>
            <ul className="grid gap-0.5">
              {examples(catalog).map((example) => (
                <li key={example}>
                  <button type="button" onClick={() => setRequest(example)}
                    className="flex w-full items-start gap-2 rounded-lg px-2 py-1.5 text-start text-body text-muted-foreground transition-colors outline-none hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring">
                    <CornerDownRight aria-hidden className="mt-0.5 size-3.5 shrink-0" /><span className="min-w-0 text-pretty">{example}</span>
                  </button>
                </li>
              ))}
            </ul>
          </div>
        )}
        {capped && <p role="status" className="text-body text-muted-foreground">The assistant has reached its spending cap.</p>}
        {ask.isError && <p role="alert" className="text-body text-destructive-text">{ask.error.message}</p>}
        {listing.isError && <p role="status" className="text-body text-muted-foreground">{listing.error.message}</p>}
      </div>
      {ask.isPending && <div className="h-24 animate-pulse motion-reduce:animate-none rounded-xl border bg-muted/40" aria-label="Proposing" />}
      {latest && <ProposalCard key={latest.id} proposal={latest} catalog={catalog} preview={preview} />}
      {earlier.length > 0 && (
        <details className="rounded-xl border bg-card p-3 text-body">
          <summary className="cursor-pointer">Earlier proposals ({earlier.length})</summary>
          <div className="mt-3 space-y-3">{earlier.map((proposal) => <ProposalCard key={proposal.id} proposal={proposal} catalog={catalog} preview={() => null} />)}</div>
        </details>
      )}
    </section>
  )
}
