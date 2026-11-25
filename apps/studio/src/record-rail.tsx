import { useQuery } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { Eye, Fingerprint, History as HistoryIcon, Scale, ShieldCheck, Waypoints, Zap, type LucideIcon } from "lucide-react"
import type { ReactNode } from "react"
import { Chip } from "@/components/chip"
import { HiddenWriter } from "@/components/hidden-writer"
import { Panel, Well } from "@/components/panel"
import { cn } from "@/lib/utils"
import { lawHeadline, systemName, type Law } from "./lib/catalog.ts"
import { useStudioScope } from "./lib/definitions.ts"
import { readableTime } from "./lib/format.ts"
import { liveTypeAccess, type RoleAccess, type TypeAccess } from "./lib/live.ts"
import { changedProperties, historyQueryKey, historyUnavailable, readHistory } from "./evidence/review.ts"

/** The cell's per-role decisions over one collection (`GET /v1/access?objectType=`), shared by the rail's parts. */
export function useTypeAccess(objectType: string, live: boolean) {
  return useQuery({ queryKey: ["type-access", objectType], queryFn: () => liveTypeAccess(objectType), enabled: live, retry: false, staleTime: 60_000 })
}

type Decision = "permit" | "deny" | "conditional" | "partial"

const DECISION_WORDS: Readonly<Record<Decision, string>> = {
  permit: "allowed for the collection",
  deny: "not allowed",
  conditional: "depends on the record or on who calls; the cell decides at each call",
  partial: "sees which records exist, not their values",
}

/**
 * How each decision is drawn. Every ink is a full text token, never faded, so a refused verb stays
 * readable (4.5:1, measured in `test/contrast.test.ts`); the strike and the dashed outline are the
 * difference, and the legend under the roles says what they mean.
 */
export const VERB_STYLE: Readonly<Record<Decision, string>> = {
  permit: "bg-foreground/[0.06] text-foreground",
  partial: "bg-foreground/[0.04] text-muted-foreground",
  conditional: "border border-dashed border-foreground/40 text-foreground",
  deny: "text-muted-foreground line-through decoration-muted-foreground",
}

/** One verb a role may or may not use here: allowed reads dark, refused reads muted and struck, "depends" reads dashed. */
function Verb({ icon: Icon, label, decision, verb }: { readonly icon: LucideIcon; readonly label: string; readonly decision: Decision; readonly verb: string }) {
  return (
    <span data-verb={verb} data-decision={decision} title={`${label}: ${DECISION_WORDS[decision]}`} aria-label={`${label}: ${DECISION_WORDS[decision]}`}
      className={cn("inline-flex h-6 max-w-full items-center gap-1 rounded-(--radius-chip) px-1.5 text-meta whitespace-nowrap max-sm:min-h-[28px]", VERB_STYLE[decision])}>
      <Icon aria-hidden className="size-3 shrink-0" /><span className="truncate">{label}</span>
    </span>
  )
}

/** One line saying what the marks mean, shown only for the marks this list uses. */
function VerbLegend({ decisions }: { readonly decisions: ReadonlySet<Decision> }) {
  const marks = [
    decisions.has("deny") && <span key="deny" className="inline-flex items-center gap-1.5"><span aria-hidden className={cn("rounded-(--radius-chip) px-1", VERB_STYLE.deny)}>Struck</span> not allowed</span>,
    decisions.has("conditional") && <span key="conditional" className="inline-flex items-center gap-1.5"><span aria-hidden className={cn("rounded-(--radius-chip) px-1", VERB_STYLE.conditional)}>Dashed</span> the cell decides at each call</span>,
  ].filter(Boolean)
  if (marks.length === 0) return null
  return <p data-verb-legend="" className="flex flex-wrap gap-x-4 gap-y-1 pt-2 text-meta text-muted-foreground">{marks}</p>
}

function RoleRow({ entry, you }: { readonly entry: RoleAccess; readonly you: boolean }) {
  // The cell's own answer, over the collection: a marked fact is still masked or withheld per record.
  const see = entry.see.decision
  return (
    <li data-role={entry.role} className="flex flex-col gap-1.5 py-2 first:pt-0 last:pb-0">
      <div className="flex items-center gap-2 text-body">
        <span className="min-w-0 flex-1 truncate capitalize">{entry.role}</span>
        {you && <Chip tone="blue">You</Chip>}
      </div>
      <div className="flex flex-wrap gap-1">
        <Verb verb="see" icon={Eye} label={see === "partial" ? "Names only" : "See"} decision={see} />
        <Verb verb="sources" icon={Waypoints} label="Sources" decision={entry.read.sources.decision} />
        {entry.actions.map((action) => <Verb key={action.actionId} verb={`act:${action.actionId}`} icon={Zap} label={action.title} decision={action.decision} />)}
      </div>
    </li>
  )
}

const note = (text: ReactNode) => <p className="px-2 py-2 text-meta text-muted-foreground">{text}</p>

/**
 * Who may see this type's records, see where they came from, and run each of its actions, by the
 * roles the cell declares. Every mark is the cell's own PDP decision for that role alone; this page
 * decides nothing. "Depends" marks an action a rule grants only on the record's values or the
 * caller's identity, which the cell decides at each call.
 */
export function WhoCanAct({ objectType, live }: { readonly objectType: string; readonly live: boolean }) {
  const read = useTypeAccess(objectType, live)
  const data = read.data
  const yours = data?.kind === "read" ? new Set(data.read.document.principal.roles) : new Set<string>()
  return (
    <Panel icon={Fingerprint} label="Who can see or act" action={data?.kind === "read" ? "Decided by the cell" : undefined}>
      <Well className="px-3.5 pb-3">
        {!live ? note("The recorded demo has no sign-in, so the cell decided no role's access.")
          : read.isPending ? note("Reading the cell's decisions…")
          : data?.kind === "refused" ? note(data.refusal.message)
          : data?.kind !== "read" ? note("This cell does not publish who may read or act on this type yet.")
          : <RoleList access={data.read.document} yours={yours} />}
      </Well>
    </Panel>
  )
}

function RoleList({ access, yours }: { readonly access: TypeAccess; readonly yours: ReadonlySet<string> }) {
  if (access.roles.length === 0) return note("The cell declares no role.")
  return (
    <>
      <ul className="divide-y divide-border/70" aria-label="Roles">
        {access.roles.map((entry) => <RoleRow key={entry.role} entry={entry} you={yours.has(entry.role)} />)}
      </ul>
      <VerbLegend decisions={new Set(access.roles.flatMap((entry) => [entry.see.decision, entry.read.sources.decision, ...entry.actions.map(({ decision }) => decision)]))} />
      {access.roles.every(({ actions }) => actions.length === 0) && <p className="pt-2 text-meta text-muted-foreground">No action changes records of this type.</p>}
      {access.roles.some(({ scope }) => scope === "collection") && <p data-access-scope="collection" className="pt-2 text-meta text-pretty text-muted-foreground">Each answer is for every record of this type, not this one: a marked fact can still be masked or withheld for a role that sees values.</p>}
    </>
  )
}

/** The rules declared over this type, leading with each rule's authored name. */
export function RulesKept({ rules, laws = [], title }: { readonly rules: readonly string[]; readonly laws?: readonly Law[]; readonly title: string }) {
  const invariants = rules.filter((rule) => !laws.some(({ rationale }) => rationale === rule))
  const items = [
    ...laws.map((law) => ({ key: law.id, text: lawHeadline(law), lang: law.titleLang })),
    ...invariants.map((rule) => ({ key: rule, text: rule, lang: undefined })),
  ]
  return (
    <Panel icon={Scale} label="Rules it keeps" action={<Link to="/rules" className="underline-offset-4 hover:underline">All rules</Link>}>
      <Well className="px-3.5 pb-3">
        {items.length === 0 ? note(`No rule is declared over ${title.toLowerCase()} records.`) : (
          <ul className="space-y-2">
            {items.map((item) => <li key={item.key} lang={item.lang} className="flex gap-2 text-body leading-snug text-pretty"><ShieldCheck aria-hidden className="mt-0.5 size-3.5 shrink-0 text-tone-green" />{item.text}</li>)}
          </ul>
        )}
      </Well>
    </Panel>
  )
}

/** Changes the rail lists before "See every change". */
const RECENT = 3

/**
 * The record's newest changes, from its history: what wrote each (an action and its verified actor,
 * or an import from a source system), when the cell committed it, and what it changed. No "why" is
 * shown: the cell lists no call input, and authorizes no reason field of its own yet. The full
 * history is on the page.
 */
export function RecentChanges({ objectType, id, actionTitles, onAll }: {
  readonly objectType: string
  readonly id: string
  readonly actionTitles: ReadonlyMap<string, string>
  readonly onAll: () => void
}) {
  const scope = useStudioScope()
  const boundScope = scope.data ?? null
  const read = useQuery({ queryKey: historyQueryKey(objectType, id, boundScope), queryFn: () => readHistory(objectType, id, boundScope), enabled: scope.isSuccess, retry: false })
  const history = read.data
  const versions = history?.document.versions ?? []
  return (
    <Panel icon={HistoryIcon} label={versions.length === 1 ? "Last change" : `Last ${Math.min(RECENT, versions.length) || RECENT} changes`}
      action={versions.length > 0 ? <button type="button" className="underline-offset-4 hover:underline" onClick={onAll}>Every change</button> : undefined}>
      <Well className="px-3.5 pb-3">
        {read.isPending ? note("Reading the history…")
          : read.isError || history === undefined ? <p data-history-unavailable="" className="px-2 py-2 text-meta text-muted-foreground">{historyUnavailable(read.error)}</p>
          : versions.length === 0 ? note("No version of this record is stored.")
          : (
            <ol className="space-y-3" aria-label="Recent changes">
              {versions.slice(0, RECENT).map((version, index) => {
                const last = index === versions.length - 1 && !history.document.truncated
                const changed = last ? [] : changedProperties(history, index)
                const { writer } = version
                // An action by the title the cell declares; with none (its access read was refused), plainly an
                // action: its id is in the full history's details, never prettified here.
                const what = writer.kind === "action" ? actionTitles.get(writer.actionId) ?? "Ran an action"
                  : writer.kind === "mapping" ? `Imported from ${systemName(writer.lineage.sourceRecordRef.system)}`
                  : <HiddenWriter />
                const when = readableTime(new Date(version.logicalTime).toISOString())
                return (
                  <li key={version.version} data-version={version.version} className="text-body leading-snug">
                    <div className="[overflow-wrap:anywhere]">{what}</div>
                    <div className="text-meta text-muted-foreground [overflow-wrap:anywhere]">
                      {writer.kind === "action" && <>{writer.actor} · </>}{when}
                      {version.values === null ? " · deleted" : last ? " · created" : changed.length > 0 ? ` · changed ${changed.slice(0, 2).map((property) => property.replaceAll("_", " ")).join(", ")}${changed.length > 2 ? ` and ${changed.length - 2} more` : ""}` : " · no value changed"}
                    </div>
                  </li>
                )
              })}
            </ol>
          )}
      </Well>
    </Panel>
  )
}
