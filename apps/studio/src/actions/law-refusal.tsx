import { useQuery } from "@tanstack/react-query"
import { Hand, ShieldCheck, ShieldX, type LucideIcon } from "lucide-react"
import type { ReactNode } from "react"
import { Chip, FactChip } from "@/components/chip"
import { liveAccess } from "@/lib/live"
import { Button } from "@/components/ui/button"
import { transport, type Artifact } from "@/lib/transport"
import type { RefusalReason } from "../../../../packages/norn-sdk/src/index.ts"
import { compared, isList, roleOf } from "../lib/law-values.ts"

/** The law the cell pinned, from the catalog: its own sentence, as declared. */
function rationaleOf(artifacts: readonly Artifact[], artifactId: string | undefined): string | undefined {
  const law = artifacts.find((artifact) => artifact.kind === "law" && artifact.artifactId === artifactId)
  const payload = law?.payload as { rationale?: unknown } | undefined
  return typeof payload?.rationale === "string" ? payload.rationale : undefined
}

/** The law's own name ("An itemized document's total equals the sum of its lines"), as its definition declares it. */
function lawNameOf(artifacts: readonly Artifact[], artifactId: string | undefined): string | undefined {
  const law = artifacts.find((artifact) => artifact.kind === "law" && artifact.artifactId === artifactId)
  return typeof law?.name === "string" && law.name !== "" ? law.name : undefined
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
    <dl aria-label={label} className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1 text-sm">
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
 * disclosed to this reader: the rule and the values it compared, or, without the disclosure grant,
 * that it does not say), and the result with who, when and why. Where each compared value came from
 * is not shown: the refusal does not carry the operands' provenance, and a record read now may not
 * be what the law evaluated.
 * The cell decided all of it; this only lays out its refusal.
 */
function RefusalSteps({ variant, request, check, why, at: answered, decidedAt }: { readonly variant: "disclosed" | "redacted"; readonly request: RefusalRequest | undefined; readonly check: ReactNode; readonly why: string
  /** When the cell's answer reached this page; labelled "Received at" when the refusal carries no `decidedAt`. */
  readonly at: Date
  /** The cell's RFC 3339 UTC timestamp (`decidedAt`), when carried on `norn/refusal@2`. */
  readonly decidedAt?: string }) {
  const caller = useCaller()
  const cellTime = decidedAt !== undefined && !Number.isNaN(Date.parse(decidedAt)) ? new Date(decidedAt) : null
  const shownTime = cellTime ?? answered
  const timeLabel = cellTime !== null ? "Decided at" : "Received at"
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
            [timeLabel, <time dateTime={cellTime !== null ? decidedAt : shownTime.toISOString()} title={cellTime !== null ? decidedAt : shownTime.toISOString()}>{shownTime.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })}</time>],
            ["Why", why],
            ["Kept", "Nothing was written. The cell keeps no receipt of a refused call."],
          ]} />
        </Step>
      </ol>
    </div>
  )
}

/**
 * A governing law refused the change (DIAG-REASON-1). Business view: what happened in plain words,
 * the law's own sentence, the values it checked, and what to do. Technical view adds the code, the
 * subject and the law's pin, and the cell's own message.
 */
export function LawRefusal({ code, reason, explanation, fields, technical, onUse, request, at = new Date(), decidedAt }: { readonly code: string; readonly reason: RefusalReason; readonly explanation: string; readonly fields: Readonly<Record<string, InputField>>; readonly technical: boolean
  /** What was asked; the request step is left out without it. */
  readonly request?: RefusalRequest
  /** When the refusal reached the form (fallback when `decidedAt` is absent). */
  readonly at?: Date
  /** When the cell decided the refusal (`norn/refusal@2`'s `decidedAt`). */
  readonly decidedAt?: string
  /** Put a value the record holds into the form's field (the cell checks it again on the next submit). */
  readonly onUse?: (path: string, text: string) => void }) {
  const catalog = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  // Only a law's pin names an artifact; an invariant or builtin pin names none.
  const pin = reason.pin?.kind === "law" ? reason.pin : undefined
  const rationale = rationaleOf(catalog.data?.document ?? [], pin?.artifactId)
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
  return (
    <RefusalSteps variant="disclosed" request={request} at={at} decidedAt={decidedAt} why={lawNameOf(catalog.data?.document ?? [], pin?.artifactId) ?? rationale ?? explanation} check={
      <div className="grid gap-2.5">
        {rationale !== undefined && <p className="flex items-start gap-2 font-medium text-pretty text-foreground"><span className="min-w-0 flex-1">{rationale}</span><Chip tone="red" icon={ShieldX} data-verdict="fails">Fails</Chip></p>}
        {checked.length === 2 && held.length === 1 ? (() => {
          // Two values the law holds equal: the one the record holds against the one this form set.
          const [[heldPath, heldValue]] = held as [[string, unknown]]
          const [setPath, setValue] = checked.find(([path]) => path !== heldPath)!
          return <>
            <div className="grid grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-center gap-2 tabular-nums" aria-label="The values it checked">
              <div className="rounded-lg bg-muted px-3 py-2" data-checked={heldPath}><div className="text-xs text-muted-foreground">{sentence(label(heldPath))}</div><div className="pt-0.5 text-lg font-light tracking-tight">{shown(heldPath, heldValue)}</div>{parts(heldValue) && <div className="text-xs text-muted-foreground" aria-label="The lines it summed">{parts(heldValue)}</div>}</div>
              <span aria-label="does not equal" className="flex size-7 items-center justify-center rounded-full bg-tone-red/12 text-sm text-tone-red">≠</span>
              <div className="rounded-lg bg-muted px-3 py-2" data-checked={setPath}><div className="text-xs text-muted-foreground">{sentence(label(setPath))}</div><div className="pt-0.5 text-lg font-light tracking-tight text-tone-red">{shown(setPath, setValue)}</div></div>
            </div>
            {onUse && Object.hasOwn(fields, setPath) && <div><Button type="button" size="sm" variant="outline" onClick={() => onUse(setPath, shown(setPath, heldValue))}>Use {shown(setPath, heldValue)}</Button></div>}
          </>
        })() : checked.length > 0 && (
          <div className="flex flex-wrap items-center gap-1.5" aria-label="The values it checked">
            {checked.map(([path, value], index) => <span key={path} className="contents" data-checked={path}>
              {index > 0 && <span className="text-meta text-muted-foreground">{index === checked.length - 1 ? "vs" : "·"}</span>}
              <FactChip label={label(path)} className="font-mono [&>span:first-of-type]:font-sans">{shown(path, value)}</FactChip>
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
            {pin !== undefined && <span>{pin.artifactId}@{pin.version}</span>}
            <span className="whitespace-pre-wrap">{explanation}</span>
          </span>
        )}
      </div>
    } />
  )
}

/**
 * A law refused the change, and the cell disclosed no detail (a hosted cell sends `reason: null`;
 * #1353): say plainly that a law refused it and nothing was written, with the cell's own words. It
 * reads nothing but the refusal itself: no finding, record or history this page may hold, which
 * could name a rule or a value the cell did not disclose for this call (a grant revoked since).
 */
export function RedactedLawRefusal({ code, message, technical, request, at = new Date(), decidedAt }: { readonly code: string; readonly message: string; readonly technical: boolean
  /** What was asked; the request step is left out without it. */
  readonly request?: RefusalRequest
  /** When the refusal reached the form (fallback when `decidedAt` is absent). */
  readonly at?: Date
  /** When the cell decided the refusal (`norn/refusal@2`'s `decidedAt`). */
  readonly decidedAt?: string }) {
  return (
    <RefusalSteps variant="redacted" request={request} at={at} decidedAt={decidedAt} why="A law this change would break. Your role is not told which." check={
      <div className="grid gap-2.5">
        <p className="text-meta text-muted-foreground">The cell said: “{message}”. It does not say which law, or which values, to your role.</p>
        {technical && <span className="border-t border-border/70 pt-2 font-mono text-xs text-muted-foreground">{code}</span>}
      </div>
    } />
  )
}
