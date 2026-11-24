import { useQuery } from "@tanstack/react-query"
import type { ReactNode } from "react"
import { Scale, ShieldAlert, ShieldCheck } from "lucide-react"
import { Chip } from "@/components/chip"
import { EmptyState, Panel, Well } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { useMode } from "@/lib/live-transport"
import { useStudioScope } from "@/lib/definitions"
import { readProfile } from "@/lib/profile"
import { useView } from "@/lib/view"
import { conceptTitle, lawHeadline, type Concept, type Law } from "./lib/catalog.ts"
import { heldCountText, heldGroupKey, heldRuleTechnicalId, heldRuleTitle } from "./lib/guarantees.ts"
import { recordName } from "./lib/objects.ts"
import { RouteLink, useInbox, type Group } from "./inbox.tsx"

export { heldCountText, heldGroupKey, heldRuleTechnicalId, heldRuleTitle }

/** How the cell holds every governing law: enforcement is decided by the cell regardless of reporting severity. */
export const ENFORCED = "The cell checks governing rules on every write regardless of reporting severity: a change that would break one is refused, naming the rule. A record loaded from a connected system that breaks one is held in quarantine instead of being served."

/** How many example held-back records a rule row shows before linking out to review all of them. */
export const HELD_EXAMPLES = 3

const FORM: Record<string, string> = { always: "must always hold", never: "must never hold", eventually: "must eventually hold", until: "must hold until" }

/**
 * The held-back group the inbox reports for `law` over `objectType`: the server groups by rule and
 * type, so an inherited law (one declared on an interface) holds each implementer's records apart.
 */
export const heldBy = (groups: readonly Group[], law: Law, objectType: string): Group | undefined =>
  groups.find(({ ruleRef, objectType: held }) =>
    held === objectType
    && ruleRef.kind === "law"
    && ((law.artifactId !== undefined && ruleRef.artifactId === law.artifactId) || ruleRef.lawId === law.id))

/** The cell's held-back groups and section summary for this reader, live only (the same inbox read Home makes). */
export function useHeldBack() {
  const inbox = useInbox()
  const document = inbox.data?.document
  const groups = document?.items.filter((item): item is Group => item.kind === "held_back") ?? []
  const section = document?.sections.find((entry) => entry.kind === "held_back")
  return { groups, section, scanLimit: document?.scanLimit, inbox }
}

/** The held-back count chip, preserving the server's lower-bound semantics when `approximate` is true. */
function HeldCountChip({ held, scanLimit }: { readonly held: Group; readonly scanLimit?: number }) {
  const count = heldCountText(held.count, held.approximate)
  return (
    <Chip
      tone="amber"
      dot
      data-held={held.count}
      data-approximate={held.approximate ? "true" : undefined}
      title={held.approximate
        ? `At least ${held.count} held back: the cell reached its scan bound${scanLimit ? ` of ${scanLimit.toLocaleString("en-US")} rows` : ""}`
        : undefined}
    >
      {count} held back
    </Chip>
  )
}

/**
 * The first held-back records the server returned for one rule, each with its finding in words (when
 * disclosed to this reader), disagreement note (when multiple laws hold it) and a review link when the server suggests
 * a fix, followed by the link into the type's held-back review flow.
 */
function HeldRecords({ held }: { readonly held: Group }) {
  const shown = held.records.slice(0, HELD_EXAMPLES)
  const more = held.count > shown.length || held.approximate
  return (
    <div className="mt-1.5 divide-y divide-border/70 overflow-hidden rounded-md border border-border/70 bg-muted/25">
      {shown.map((record) => {
        const others = record.otherFindings ?? []
        return (
          <div key={`${record.objectType}:${record.id}`} data-held-record={record.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 text-sm">
            <RouteLink to={record.target} className="font-medium underline-offset-4 hover:underline">{recordName(record)}</RouteLink>
            {record.finding && <span className="min-w-0 flex-1 text-muted-foreground tabular-nums">{record.finding.words}</span>}
            {record.suggestedFix && (
              <Button asChild size="xs" variant="secondary" className="ms-auto">
                <RouteLink to={record.target}>Review fix</RouteLink>
              </Button>
            )}
            {others.length > 0 && (
              <div className="basis-full space-y-0.5 text-xs text-muted-foreground">
                <Badge variant="outline">Needs review: {others.length + 1} laws disagree</Badge>
                {others.map((other) => <p key={other.id} className="tabular-nums">{other.words}</p>)}
              </div>
            )}
          </div>
        )
      })}
      <div className="flex items-center px-1 py-1">
        <Button asChild size="xs" variant="ghost">
          <RouteLink to={held.target}>{more ? `Review all ${heldCountText(held.count, held.approximate)}` : "Review in records"}</RouteLink>
        </Button>
      </div>
    </div>
  )
}

/**
 * One law: its name in plain words (the artifact's name, or its `law:` label in the reader's
 * language), why it exists, how the cell holds it and, live, the records it is holding back right
 * now. Everything here is the server's: Studio judges nothing.
 */
function LawRow({ law, held, scanLimit }: { readonly law: Law; readonly held: Group | undefined; readonly scanLimit?: number }) {
  const { view } = useView()
  const headline = lawHeadline(law)
  return (
    <div className="flex gap-3 px-4 py-3 text-sm" data-law={law.id}>
      <span className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-lg bg-tone-green/10 text-tone-green dark:bg-tone-green/15"><ShieldCheck aria-hidden className="size-4" /></span>
      <div className="min-w-0 flex-1 space-y-1.5">
        <p lang={law.titleLang} className="font-medium text-pretty">{headline}</p>
        {headline !== law.rationale && <p className="text-meta text-pretty text-muted-foreground">{law.rationale}</p>}
        <div className="flex flex-wrap items-center gap-1.5">
          <Chip tone={law.severity === "error" ? "red" : "amber"} icon={law.severity === "error" ? ShieldAlert : undefined} dot={law.severity !== "error"}
            title="Declared reporting severity; does not change how the cell enforces this rule">
            {law.severity === "error" ? "Error severity" : "Warning severity"}
          </Chip>
          <Chip tone="gray" title={law.response ? `as a ${law.response}` : undefined}>{FORM[law.modality] ?? law.modality}</Chip>
          {law.inheritedFrom && <Chip tone="gray" title="Declared on a kind of record this one is">from {conceptTitle(law.inheritedFrom)}</Chip>}
          {held && <HeldCountChip held={held} scanLimit={scanLimit} />}
          {held && held.records.length === 0 && (
            <RouteLink to={held.target} className="text-meta text-muted-foreground underline-offset-4 hover:text-foreground hover:underline">
              Review {heldCountText(held.count, held.approximate)}
            </RouteLink>
          )}
          {view === "technical" && (
            <>
              <Chip tone="violet" title="The pack or module that owns this rule">{law.owner}</Chip>
              <code className="font-mono text-xs text-muted-foreground">{law.id}</code>
            </>
          )}
        </div>
        {held && held.records.length > 0 && <HeldRecords held={held} />}
      </div>
    </div>
  )
}

/** A held-back group reported by the server for a rule without a matching catalog law (e.g. a built-in rule). */
export function HeldRuleRow({ held, scanLimit }: { readonly held: Group; readonly scanLimit?: number }) {
  const { view } = useView()
  const technicalId = heldRuleTechnicalId(held.ruleRef)
  const title = heldRuleTitle(held)
  return (
    <div className="flex gap-3 px-4 py-3 text-sm" data-rule={heldGroupKey(held)}>
      <span className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-lg bg-tone-amber/12 text-tone-amber dark:bg-tone-amber/15"><ShieldAlert aria-hidden className="size-4" /></span>
      <div className="min-w-0 flex-1 space-y-1.5">
        <p className="font-medium text-pretty">{title}</p>
        {held.rationale && held.rationale !== title && held.rationale !== technicalId && <p className="text-meta text-pretty text-muted-foreground">{held.rationale}</p>}
        <div className="flex flex-wrap items-center gap-1.5">
          {held.ruleRef.kind === "builtin" && <Chip tone="gray">Built-in</Chip>}
          {held.ruleRef.kind === "invariant" && <Chip tone="gray">Invariant</Chip>}
          <HeldCountChip held={held} scanLimit={scanLimit} />
          {held.records.length === 0 && (
            <RouteLink to={held.target} className="text-meta text-muted-foreground underline-offset-4 hover:text-foreground hover:underline">
              Review {heldCountText(held.count, held.approximate)}
            </RouteLink>
          )}
          {view === "technical" && <code className="font-mono text-xs text-muted-foreground">{technicalId}</code>}
        </div>
        {held.records.length > 0 && <HeldRecords held={held} />}
      </div>
    </div>
  )
}

/** A type's records the cell holds back, from the cell's own profile of what this reader may see. */
function Quarantined({ objectType }: { readonly objectType: string }) {
  const scope = useStudioScope()
  const scopeKey = scope.data ? `${scope.data.orgId}:${scope.data.environmentId}` : ""
  const profile = useQuery({ queryKey: ["profile", scopeKey, objectType], queryFn: () => readProfile(objectType, undefined, scope.data), enabled: !scope.isPending, retry: false })
  if (!profile.data) return null
  const { quarantinedRows, rows, approximate, scanLimit } = profile.data.document
  const bound = scanLimit > 0 ? ` of ${scanLimit.toLocaleString("en-US")} rows` : ""
  const quarantinedText = heldCountText(quarantinedRows, approximate)
  const servedText = heldCountText(rows, approximate)
  return (
    <div className="flex flex-wrap items-center gap-1.5 bg-surface-subtle px-4 py-2.5 text-meta text-muted-foreground tabular-nums" data-profile-summary={objectType} data-approximate={approximate ? "true" : undefined}>
      <Chip tone={quarantinedRows > 0 ? "amber" : "gray"} dot data-profile-quarantined={quarantinedRows} data-approximate={approximate ? "true" : undefined}
        title={approximate ? `At least ${quarantinedRows} in quarantine: the cell reached its scan bound${bound}` : undefined}>
        {quarantinedText} in quarantine
      </Chip>
      <Chip tone="teal" dot data-profile-served={rows} data-approximate={approximate ? "true" : undefined}
        title={approximate ? `At least ${rows} served: the cell reached its scan bound${bound}` : undefined}>
        {servedText} served
      </Chip>
      {approximate && (
        <span className="text-meta text-muted-foreground">Lower bound · scan bound{bound} reached</span>
      )}
      <span className="sr-only">
        {approximate
          ? `At least ${quarantinedRows} ${quarantinedRows === 1 ? "record is" : "records are"} in quarantine and at least ${rows} served (scan bound${bound} reached).`
          : `${quarantinedRows} ${quarantinedRows === 1 ? "record is" : "records are"} in quarantine now, beside ${rows} served.`}
      </span>
    </div>
  )
}

/** The rules a type must keep, how the cell holds them, and (live) the records each rule holds back. */
export function LawsPanel({ concept, label = "Rules it must keep", enforced = true, heldOnly = false, heldGroups, scanLimit }: {
  readonly concept: Concept
  readonly label?: ReactNode
  readonly enforced?: boolean
  readonly heldOnly?: boolean
  readonly heldGroups?: readonly Group[]
  readonly scanLimit?: number
}) {
  const live = useMode().data?.kind === "live"
  const read = useHeldBack()
  const groups = heldGroups ?? read.groups
  const bound = scanLimit ?? read.scanLimit
  const matchedLaws = concept.laws.map((law) => ({ law, held: heldBy(groups, law, concept.name) }))
  const matchedGroups = new Set(matchedLaws.flatMap(({ held }) => held ? [held] : []))
  const extraHeld = groups.filter((group) => group.objectType === concept.name && !matchedGroups.has(group))
  const shownLaws = heldOnly ? matchedLaws.filter(({ held }) => held !== undefined) : matchedLaws
  const invariants = heldOnly ? [] : concept.rules.filter((rule) => !concept.laws.some(({ rationale }) => rationale === rule))
  const empty = shownLaws.length === 0 && invariants.length === 0 && extraHeld.length === 0
  return (
    <Panel icon={Scale} label={label}>
      <Well className="divide-y">
        {empty && <EmptyState icon={Scale}>{heldOnly ? "No rule is holding back records of this type." : "No rule is declared on this object type itself."}</EmptyState>}
        {shownLaws.map(({ law, held }) => <LawRow key={law.id} law={law} held={held} scanLimit={bound} />)}
        {extraHeld.map((group) => <HeldRuleRow key={heldGroupKey(group)} held={group} scanLimit={bound} />)}
        {invariants.map((rule) => <div key={rule} className="flex gap-3 px-4 py-3 text-sm"><span className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-lg bg-tone-green/10 text-tone-green dark:bg-tone-green/15"><ShieldCheck aria-hidden className="size-4" /></span><p className="min-w-0 flex-1 font-medium text-pretty">{rule}</p></div>)}
        {enforced && !empty && <p className="px-4 py-3 text-meta text-muted-foreground">{ENFORCED}</p>}
        {live && !heldOnly && concept.definition === "object type" && <Quarantined objectType={concept.name} />}
      </Well>
    </Panel>
  )
}
