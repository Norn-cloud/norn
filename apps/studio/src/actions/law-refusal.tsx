import { useQueries, useQuery } from "@tanstack/react-query"
import { useLocale } from "@/lib/locale"
import { useView } from "@/lib/view"
import { catalogOf, systemName } from "../lib/catalog.ts"
import { atScale, factDisplay } from "../lib/objects.ts"
import { readFindings, readHistory, versionValue } from "../evidence/review.ts"
import { Hand, ShieldCheck, ShieldX, type LucideIcon } from "lucide-react"
import type { ReactNode } from "react"
import { Chip, FactChip } from "@/components/chip"
import { SourceChip } from "@/components/source-chip"
import { liveAccess } from "@/lib/live"
import { Button } from "@/components/ui/button"
import { transport, type Artifact } from "@/lib/transport"
import type { RefusalReason } from "../lib/actions.ts"
import { compared, isList, roleOf } from "../lib/law-values.ts"

/** The law the cell pinned, from the catalog: its own sentence, as declared. */
function rationaleOf(artifacts: readonly Artifact[], artifactId: string | undefined): string | undefined {
  const law = artifacts.find((artifact) => artifact.kind === "law" && artifact.artifactId === artifactId)
  const payload = law?.payload as { rationale?: unknown } | undefined
  return typeof payload?.rationale === "string" ? payload.rationale : undefined
}

/** A checked value's path in words, when no input field declares it: `ledger_entry.amount` → "ledger entry amount". */
const words = (path: string) => path.replaceAll(".", " ").replaceAll("_", " ")

/** An action input field as its schema declares it: the label it is asked by, and a decimal's scale. */
export interface InputField { readonly title?: string; readonly nornType?: { readonly kind?: string; readonly scale?: number } }

/** A checked value at its declared scale (exact text, padded, never rounded); a value without one as it came. */
function atDeclared(value: unknown, scale: number | undefined): string {
  const text = String(value)
  if (!/^-?\d+(\.\d+)?$/.test(text)) return text
  const places = scale ?? 2
  const [whole, fraction = ""] = text.split(".")
  if (places <= 0 || fraction.length >= places || places > 6) return text
  return `${whole}.${fraction.padEnd(places, "0")}`
}

/** What was asked: the action, the record it targets, and the values this form sent. */
export interface RefusalRequest {
  readonly action: string
  readonly objectType: string
  readonly objectId: string | undefined
  readonly values: readonly { readonly label: string; readonly value: string }[]
}

/** One step of a refusal: a small icon and its name over its content. */
function Step({ icon: Icon, label, children, last = false }: { readonly icon: LucideIcon; readonly label: string; readonly children: ReactNode; readonly last?: boolean }) {
  return (
    <li className="relative grid grid-cols-[1.75rem_minmax(0,1fr)] gap-x-3">
      {!last && <span aria-hidden className="absolute start-[0.8125rem] top-8 bottom-0 w-px bg-border" />}
      <span aria-hidden className="flex size-7 items-center justify-center rounded-full bg-muted text-muted-foreground"><Icon className="size-3.5" /></span>
      <section aria-label={label} className="grid min-w-0 gap-2 pb-5">
        <h3 className="pt-1 text-meta font-medium text-muted-foreground">{label}</h3>
        {children}
      </section>
    </li>
  )
}

/** A two-column list of labelled values: the request, or who · when · why. */
function Rows({ rows, label }: { readonly rows: readonly (readonly [string, ReactNode])[]; readonly label: string }) {
  return (
    <dl aria-label={label} className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1 rounded-lg bg-muted/60 px-3 py-2 text-sm">
      {rows.map(([key, value]) => <div key={key} className="contents"><dt className="text-muted-foreground">{key}</dt><dd className="min-w-0 font-medium [overflow-wrap:anywhere]">{value}</dd></div>)}
    </dl>
  )
}

/** The verified caller, as the cell named it (`GET /v1/access`); "You" when it is not read. */
function useCaller(): string {
  const access = useQuery({ queryKey: ["live-access"], queryFn: () => liveAccess(), retry: false, staleTime: 60_000 })
  const principal = access.data?.kind === "read" ? access.data.read.document.principal : undefined
  return principal ? `${principal.subjectId}${principal.roles.length > 0 ? ` · ${principal.roles.join(", ")}` : ""}` : "You"
}

/**
 * A refused action as three steps: the request (what was asked, by whom), the cell's check (what it
 * disclosed to this reader: the rule and the values it compared with the system each was read from,
 * or, without the disclosure grant, that it does not say), and the result with who, when and why.
 * The cell decided all of it; this only lays out its refusal.
 */
function RefusalSteps({ variant, request, check, why, at: answered }: { readonly variant: "disclosed" | "redacted"; readonly request: RefusalRequest | undefined; readonly check: ReactNode; readonly why: string
  /** When the cell's answer reached this page: the refusal carries no time of its own, and no receipt is kept for it. */
  readonly at: Date }) {
  const caller = useCaller()
  return (
    <div role="alert" data-refusal={variant} className="overflow-hidden rounded-xl border border-border bg-card text-sm">
      <div className="flex flex-wrap items-center gap-2 border-b border-border/70 px-3 py-2">
        <Chip tone="red" icon={ShieldX}>Refused</Chip>
        <span className="font-medium">{variant === "redacted" ? "Refused by a law. Nothing was written." : "This change breaks a rule, so nothing was saved"}</span>
      </div>
      <ol className="px-3 pt-3">
        {request && (
          <Step icon={Hand} label="The request">
            <Rows label="What was asked" rows={[
              ["Action", request.action],
              ["Record", request.objectId === undefined ? "A new record" : <span className="font-mono text-xs">{request.objectId}</span>],
              ...request.values.map(({ label, value }) => [label, <span className="tabular-nums">{value}</span>] as const),
            ]} />
          </Step>
        )}
        <Step icon={ShieldCheck} label="The check">{check}</Step>
        <Step icon={ShieldX} label="The result" last>
          <Rows label="Who, when and why" rows={[
            ["Who", caller],
            ["When", <time dateTime={answered.toISOString()}>{answered.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })}</time>],
            ["Why", why],
            ["Kept", "Nothing was written. The cell keeps no receipt of a refused call."],
          ]} />
        </Step>
      </ol>
    </div>
  )
}

/**
 * The link a role reads through: the entity whose link to `objectType` declares `role` as its
 * inverse (a document's `lines` are the line records that link to it), as the definitions say.
 */
function inverseOf(artifacts: readonly Artifact[], objectType: string, role: string): { readonly sourceType: string; readonly link: string } | undefined {
  for (const artifact of artifacts) {
    if (artifact.kind !== "entity") continue
    const type = (artifact.payload as { objectType?: { name?: string; links?: readonly { id: string; inverse?: string; target?: string }[] } } | undefined)?.objectType
    const link = type?.links?.find((candidate) => candidate.inverse === role && candidate.target === objectType)
    if (type?.name !== undefined && link !== undefined) return { sourceType: type.name, link: link.id }
  }
  return undefined
}

/** Linked records read for their sources, at most: a refusal's sum rarely spans more. */
const LINKED = 25

/**
 * Where a checked value came from: this request; or the system the record's field provenance names
 * for it, read through a link from each linked record's own provenance (a summed line amount), when
 * this reader may see sources. Otherwise it is the value the cell holds, and says only that.
 */
function CheckedFrom({ path, fromRequest, objectType, id }: { readonly path: string; readonly fromRequest: boolean; readonly objectType: string | undefined; readonly id: string | undefined }) {
  const catalog = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const object = useQuery({ queryKey: ["object", objectType, id], queryFn: () => transport.object(objectType!, id!), enabled: objectType !== undefined && id !== undefined && !fromRequest, retry: false })
  const dot = path.indexOf(".")
  const through = dot < 0 || objectType === undefined ? undefined : inverseOf(catalog.data?.document ?? [], objectType, path.slice(0, dot))
  const linked = through === undefined || id === undefined ? [] : (object.data?.links.document.items ?? [])
    .filter((item) => item.sourceType === through.sourceType && item.link === through.link && item.targetType === objectType && item.target === id)
    .slice(0, LINKED).map((item) => item.source)
  const lines = useQueries({ queries: linked.map((source) => ({ queryKey: ["object", through?.sourceType, source], queryFn: () => transport.object(through!.sourceType, source), retry: false })) })
  if (fromRequest) return <Chip tone="gray" data-checked-from="request" title="The value this request sent">This request</Chip>
  const systems = dot < 0
    ? [object.data?.source?.document.fieldProvenance[path]?.source.system].filter((system) => system !== undefined)
    : lines.every((line) => line.data?.source?.document.fieldProvenance[path.slice(dot + 1)] !== undefined) && lines.length > 0
      ? [...new Set(lines.map((line) => line.data!.source!.document.fieldProvenance[path.slice(dot + 1)]!.source.system))]
      : []
  if (systems.length === 0) return <Chip tone="gray" data-checked-from="record" title="The value the cell holds; your role does not see where it came from, or it was written in Norn">On record</Chip>
  return <>{systems.map((system) => <SourceChip key={system} system={system} title={`${dot < 0 ? "The value the record holds" : `Each of the ${lines.length} linked records`}, read from ${systemName(system)}`} />)}</>
}

/**
 * A governing law refused the change (DIAG-REASON-1). Business view: what happened in plain words,
 * the law's own sentence, the values it checked, and what to do. Technical view adds the code, the
 * subject and the law's pin, and the cell's own message.
 */
export function LawRefusal({ code, reason, explanation, fields, technical, onUse, request, at = new Date() }: { readonly code: string; readonly reason: RefusalReason; readonly explanation: string; readonly fields: Readonly<Record<string, InputField>>; readonly technical: boolean
  /** What was asked; the request step is left out without it. */
  readonly request?: RefusalRequest
  /** When the refusal reached the form. */
  readonly at?: Date
  /** Put a value the record holds into the form's field (the cell checks it again on the next submit). */
  readonly onUse?: (path: string, text: string) => void }) {
  const catalog = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const rationale = rationaleOf(catalog.data?.document ?? [], reason.pin?.artifactId)
  const checked = Object.entries(reason.values)
  // The checked values this form did not set are the ones the record still holds.
  const held = checked.filter(([path]) => !Object.hasOwn(fields, path))
  const sentence = (text: string) => text.charAt(0).toUpperCase() + text.slice(1)
  // Each value by the label the action asks it by, at the field's declared scale. A list is every
  // line the law summed (LAW-LINK-MANY-1): it is shown as their sum, at the scale of the value it is held to.
  const listed = checked.find(([, value]) => isList(value))?.[0]
  const scaleOf = (path: string) => fields[path]?.nornType?.kind === "decimal" ? fields[path]?.nornType?.scale : undefined
  const listScale = checked.map(([path]) => scaleOf(path)).find((scale) => scale !== undefined)
  const label = (path: string) => fields[path]?.title ?? (path === listed ? `sum of ${words(roleOf(path))}` : words(path))
  const shown = (path: string, value: unknown) => isList(value) ? atDeclared(compared(value).text, listScale) : atDeclared(value, scaleOf(path))
  const parts = (value: unknown) => isList(value) ? value.map((part) => atDeclared(part, listScale)).join(" + ") : undefined
  const from = (path: string) => <CheckedFrom path={path} fromRequest={Object.hasOwn(fields, path)} objectType={request?.objectType} id={request?.objectId} />
  return (
    <RefusalSteps variant="disclosed" request={request} at={at} why={rationale ?? explanation} check={
      <div className="grid gap-2.5">
        {rationale !== undefined && <p className="flex items-start gap-2 font-medium text-pretty text-foreground"><span className="min-w-0 flex-1">{rationale}</span><Chip tone="red" icon={ShieldX} data-verdict="fails">Fails</Chip></p>}
        {checked.length === 2 && held.length === 1 ? (() => {
          // Two values the law holds equal: the one the record holds against the one this form set.
          const [[heldPath, heldValue]] = held as [[string, unknown]]
          const [setPath, setValue] = checked.find(([path]) => path !== heldPath)!
          return <>
            <div className="grid grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-center gap-2 tabular-nums" aria-label="The values it checked">
              <div className="rounded-lg bg-muted px-3 py-2" data-checked={heldPath}><div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">{sentence(label(heldPath))}{from(heldPath)}</div><div className="pt-0.5 text-lg font-light tracking-tight">{shown(heldPath, heldValue)}</div>{parts(heldValue) && <div className="text-xs text-muted-foreground" aria-label="The lines it summed">{parts(heldValue)}</div>}</div>
              <span aria-label="does not equal" className="flex size-7 items-center justify-center rounded-full bg-tone-red/12 text-sm text-tone-red">≠</span>
              <div className="rounded-lg bg-muted px-3 py-2" data-checked={setPath}><div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">{sentence(label(setPath))}{from(setPath)}</div><div className="pt-0.5 text-lg font-light tracking-tight text-tone-red">{shown(setPath, setValue)}</div></div>
            </div>
            {onUse && Object.hasOwn(fields, setPath) && <div><Button type="button" size="sm" variant="outline" onClick={() => onUse(setPath, shown(setPath, heldValue))}>Use {shown(setPath, heldValue)}</Button></div>}
          </>
        })() : checked.length > 0 && (
          <div className="flex flex-wrap items-center gap-1.5" aria-label="The values it checked">
            {checked.map(([path, value], index) => <span key={path} className="contents" data-checked={path}>
              {index > 0 && <span className="text-meta text-muted-foreground">{index === checked.length - 1 ? "vs" : "·"}</span>}
              <FactChip label={label(path)} className="font-mono [&>span:first-of-type]:font-sans">{shown(path, value)}</FactChip>
              {from(path)}
            </span>)}
          </div>
        )}
        <p className="text-meta text-muted-foreground">
          {held.length > 0 && held.length < checked.length
            ? held.map(([path, value]) => `${sentence(label(path))} is ${shown(path, value)} on the record`).join("; ") + "; change the values so the rule holds."
            : "Change the values so the rule holds."}
        </p>
        {technical && (
          <span className="grid gap-0.5 border-t border-border/70 pt-2 font-mono text-xs text-muted-foreground">
            <span>{code} · {reason.subject}</span>
            {reason.pin?.artifactId !== undefined && <span>{reason.pin.artifactId}@{reason.pin.version ?? "?"}</span>}
            <span className="whitespace-pre-wrap">{explanation}</span>
          </span>
        )}
      </div>
    } />
  )
}

/**
 * A law refused the change, and the cell disclosed no detail (a hosted cell sends `reason: null`;
 * #1353): say plainly that a law refused it and nothing was written, with the cell's own words. Then,
 * as separate facts, the open law findings on this record that the reader can already read (the same
 * ones its page shows): which law each records and the facts it compares, with the record's values.
 * Nothing here claims which law refused this write, and nothing evaluates a law.
 */
export function RedactedLawRefusal({ objectType, id, code, message, technical, request, at = new Date() }: { readonly objectType: string; readonly id: string | undefined; readonly code: string; readonly message: string; readonly technical: boolean
  /** What was asked; the request step is left out without it. */
  readonly request?: RefusalRequest
  /** When the refusal reached the form. */
  readonly at?: Date }) {
  const { locale } = useLocale()
  const { view } = useView()
  const catalog = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const findings = useQuery({ queryKey: ["findings", objectType, id], queryFn: () => readFindings(objectType, id!), enabled: id !== undefined, retry: false })
  const object = useQuery({ queryKey: ["object", objectType, id], queryFn: () => transport.object(objectType, id!), enabled: id !== undefined, retry: false })
  // A record held in quarantine is not served by the object route; its history (the same read its page shows) still is.
  const history = useQuery({ queryKey: ["history", objectType, id], queryFn: () => readHistory(objectType, id!), enabled: id !== undefined && object.isSuccess && object.data === null, retry: false })
  const concept = catalog.data ? catalogOf(catalog.data.document, locale).byName.get(objectType) : undefined
  const open = findings.data?.document.findings.filter(({ status }) => status === "open") ?? []
  const record = object.data ?? undefined
  const fact = (fieldId: string) => {
    const field = concept?.fields.find(({ id: candidate }) => candidate === fieldId)
    if (field === undefined) return { label: fieldId.replaceAll("_", " "), value: undefined }
    const label = field.label ?? field.id.replaceAll("_", " ")
    if (record) return { label, value: factDisplay(field, record.object.document, record.object.numbers, "", view) }
    // Newest version first; a value the reader may not see stays a dash.
    if (history.data && history.data.document.versions.length > 0) {
      const text = versionValue(history.data, 0, fieldId)
      return { label, value: text === "—" ? undefined : atScale(text, field.decimal?.scale) }
    }
    return { label, value: undefined }
  }
  // A value the finding recorded: a list (every line the law summed) is its sum, at the scale of the fact it is held to.
  const readFact = (path: string, value: unknown, heldTo: string) => {
    const scale = concept?.fields.find(({ id: candidate }) => candidate === heldTo)?.decimal?.scale
    if (isList(value)) {
      const sum = compared(value)
      return { label: `sum of ${roleOf(path).replaceAll("_", " ")}`, value: `${atScale(sum.text, scale)} (${(sum.parts ?? []).map((part) => atScale(part, scale)).join(" + ")})` }
    }
    const field = concept?.fields.find(({ id: candidate }) => candidate === path)
    return { label: field?.label ?? path.replaceAll(".", " ").replaceAll("_", " "), value: atScale(String(value), field?.decimal?.scale ?? scale) }
  }
  return (
    <RefusalSteps variant="redacted" request={request} at={at} why={message} check={
      <div className="grid gap-2.5">
        <p className="text-meta text-muted-foreground">The cell said: “{message}”. It does not say which law, or which values, to your role.</p>
        {open.length > 0 && (
          <div className="grid gap-2" data-open-findings>
            <div className="text-xs font-medium text-muted-foreground">Open findings on this record</div>
            {open.map((finding) => {
              const law = finding.ruleRef.kind === "law" ? concept?.laws.find(({ id: lawId }) => lawId === (finding.ruleRef as { lawId: string }).lawId) : undefined
              // The values the law read, as the finding records them; else the record's stored ones.
              const read = finding.values
              const comparedFacts = law?.compares?.map((path) => read && Object.hasOwn(read, path) ? readFact(path, read[path], law.compares![0]) : fact(path))
              return (
                <div key={finding.id} className="grid gap-1.5 rounded-lg bg-muted px-3 py-2">
                  <p className="text-pretty">{law?.rationale ?? finding.message}</p>
                  {comparedFacts && <div className="flex flex-wrap items-center gap-1.5 tabular-nums">
                    {comparedFacts.map(({ label, value }, index) => <span key={label} className="contents">
                      {index > 0 && <span className="text-meta text-muted-foreground">vs</span>}
                      <FactChip label={label}>{value ?? "—"}</FactChip>
                    </span>)}
                  </div>}
                </div>
              )
            })}
            <p className="text-xs text-muted-foreground">These are on the record as it is stored; correcting them is what the law asks.</p>
          </div>
        )}
        {technical && <span className="border-t border-border/70 pt-2 font-mono text-xs text-muted-foreground">{code}</span>}
      </div>
    } />
  )
}
