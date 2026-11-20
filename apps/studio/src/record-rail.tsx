import { useQuery } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { Eye, Fingerprint, History as HistoryIcon, Scale, ShieldCheck, Waypoints, Zap, type LucideIcon } from "lucide-react"
import type { ReactNode } from "react"
import { Chip } from "@/components/chip"
import { Panel, Well } from "@/components/panel"
import { cn } from "@/lib/utils"
import { systemName } from "./lib/catalog.ts"
import { readableTime } from "./lib/format.ts"
import { liveTypeAccess, type RoleAccess, type TypeAccess } from "./lib/live.ts"
import { changedProperties, readHistory, type ObjectVersion } from "./evidence/review.ts"

/** The cell's per-role decisions over one collection (`GET /v1/access?objectType=`), shared by the rail's parts. */
export function useTypeAccess(objectType: string, live: boolean) {
  return useQuery({ queryKey: ["type-access", objectType], queryFn: () => liveTypeAccess(objectType), enabled: live, retry: false, staleTime: 60_000 })
}

type Decision = "permit" | "deny" | "conditional" | "partial"

const DECISION_WORDS: Readonly<Record<Decision, string>> = {
  permit: "allowed",
  deny: "not allowed",
  conditional: "depends on the record or on who calls; the cell decides at each call",
  partial: "sees which records exist, not their values",
}

/** One verb a role may or may not use here: allowed reads dark, refused reads faded and struck, "depends" reads dashed. */
function Verb({ icon: Icon, label, decision, verb }: { readonly icon: LucideIcon; readonly label: string; readonly decision: Decision; readonly verb: string }) {
  return (
    <span data-verb={verb} data-decision={decision} title={`${label}: ${DECISION_WORDS[decision]}`} aria-label={`${label}: ${DECISION_WORDS[decision]}`}
      className={cn("inline-flex h-6 max-w-full items-center gap-1 rounded-md px-1.5 text-xs font-medium whitespace-nowrap",
        decision === "permit" && "bg-foreground/[0.06] text-foreground",
        decision === "partial" && "bg-foreground/[0.04] text-muted-foreground",
        decision === "conditional" && "border border-dashed border-foreground/30 text-foreground",
        decision === "deny" && "text-muted-foreground/55 line-through decoration-muted-foreground/40")}>
      <Icon aria-hidden className="size-3 shrink-0" /><span className="truncate">{label}</span>
    </span>
  )
}

function RoleRow({ entry, you }: { readonly entry: RoleAccess; readonly you: boolean }) {
  const see: Decision = entry.read.values.decision === "permit" ? "permit" : entry.read.population.decision === "permit" ? "partial" : "deny"
  return (
    <li data-role={entry.role} className="flex flex-col gap-1.5 py-2 first:pt-0 last:pb-0">
      <div className="flex items-center gap-2 text-sm">
        <span className="min-w-0 flex-1 truncate font-medium capitalize">{entry.role}</span>
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
      {access.roles.every(({ actions }) => actions.length === 0) && <p className="pt-2 text-meta text-muted-foreground">No action changes records of this type.</p>}
    </>
  )
}

/** The laws declared over this type, in their own sentences. */
export function RulesKept({ rules, title }: { readonly rules: readonly string[]; readonly title: string }) {
  return (
    <Panel icon={Scale} label="Rules it keeps" action={<Link to="/guarantees" className="underline-offset-4 hover:underline">All rules</Link>}>
      <Well className="px-3.5 pb-3">
        {rules.length === 0 ? note(`No rule is declared over ${title.toLowerCase()} records.`) : (
          <ul className="space-y-2">
            {rules.map((rule) => <li key={rule} className="flex gap-2 text-sm leading-snug text-pretty"><ShieldCheck aria-hidden className="mt-0.5 size-3.5 shrink-0 text-tone-green" />{rule}</li>)}
          </ul>
        )}
      </Well>
    </Panel>
  )
}

/** Changes the rail lists before "See every change". */
const RECENT = 3

/** A short why from an action's recorded input, when it carries one under a plain name. */
const why = (writer: ObjectVersion["writer"]): string | null => {
  if (writer.kind !== "action") return null
  for (const key of ["reason", "why", "note", "comment"]) {
    const value = writer.input[key]
    if (typeof value === "string" && value.trim() !== "") return value.trim()
  }
  return null
}

const humanize = (id: string): string => {
  const text = id.slice(id.lastIndexOf(".") + 1).replaceAll("_", " ")
  return text.charAt(0).toUpperCase() + text.slice(1)
}

/**
 * The record's newest changes, from its history: what wrote each (an action and its verified actor,
 * or an import from a source system), when the cell committed it, what it changed and, when the
 * action's input says, why. The full history is on the page.
 */
export function RecentChanges({ objectType, id, actionTitles, onAll }: {
  readonly objectType: string
  readonly id: string
  readonly actionTitles: ReadonlyMap<string, string>
  readonly onAll: () => void
}) {
  const read = useQuery({ queryKey: ["history", objectType, id], queryFn: () => readHistory(objectType, id), retry: false })
  const history = read.data
  const versions = history?.document.versions ?? []
  return (
    <Panel icon={HistoryIcon} label={`Last ${Math.min(RECENT, versions.length) || RECENT} changes`}
      action={versions.length > 0 ? <button type="button" className="underline-offset-4 hover:underline" onClick={onAll}>Every change</button> : undefined}>
      <Well className="px-3.5 pb-3">
        {read.isPending ? note("Reading the history…")
          : read.isError || history === undefined ? note("The history could not be read.")
          : versions.length === 0 ? note("No version of this record is stored.")
          : (
            <ol className="space-y-3" aria-label="Recent changes">
              {versions.slice(0, RECENT).map((version, index) => {
                const last = index === versions.length - 1 && !history.document.truncated
                const changed = last ? [] : changedProperties(history, index)
                const { writer } = version
                const what = writer.kind === "action" ? actionTitles.get(writer.actionId) ?? humanize(writer.actionId)
                  : writer.kind === "mapping" ? `Imported from ${systemName(writer.lineage.sourceRecordRef.system)}`
                  : "Written by a rule or an import"
                const reason = why(writer)
                const when = readableTime(new Date(version.logicalTime).toISOString())
                return (
                  <li key={version.version} data-version={version.version} className="text-sm leading-snug">
                    <div className="font-medium [overflow-wrap:anywhere]">{what}</div>
                    <div className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
                      {writer.kind === "action" && <>{writer.actor} · </>}{when}
                      {last ? " · created" : changed.length > 0 ? ` · changed ${changed.slice(0, 2).map((property) => property.replaceAll("_", " ")).join(", ")}${changed.length > 2 ? ` and ${changed.length - 2} more` : ""}` : " · no value changed"}
                    </div>
                    {reason && <div className="pt-0.5 text-xs text-pretty"><span className="text-muted-foreground">Why: </span>{reason}</div>}
                  </li>
                )
              })}
            </ol>
          )}
      </Well>
    </Panel>
  )
}
