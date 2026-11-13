import { useQuery } from "@tanstack/react-query"
import { transport, type Artifact } from "@/lib/transport"
import type { RefusalReason } from "../lib/actions.ts"

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

/**
 * A governing law refused the change (DIAG-REASON-1). Business view: what happened in plain words,
 * the law's own sentence, the values it checked, and what to do. Technical view adds the code, the
 * subject and the law's pin, and the cell's own message.
 */
export function LawRefusal({ code, reason, explanation, fields, technical }: { readonly code: string; readonly reason: RefusalReason; readonly explanation: string; readonly fields: Readonly<Record<string, InputField>>; readonly technical: boolean }) {
  const catalog = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const rationale = rationaleOf(catalog.data?.document ?? [], reason.pin?.artifactId)
  const checked = Object.entries(reason.values)
  // The checked values this form did not set are the ones the record still holds.
  const held = checked.filter(([path]) => !Object.hasOwn(fields, path))
  const sentence = (text: string) => text.charAt(0).toUpperCase() + text.slice(1)
  // Each value by the label the action asks it by, at the field's declared scale.
  const label = (path: string) => fields[path]?.title ?? words(path)
  const shown = (path: string, value: unknown) => atDeclared(value, fields[path]?.nornType?.kind === "decimal" ? fields[path]?.nornType?.scale : undefined)
  return (
    <div role="alert" className="grid gap-1.5 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm">
      <span className="font-medium text-destructive">This change breaks a rule, so nothing was saved</span>
      {rationale !== undefined && <span className="text-foreground">{rationale}</span>}
      {checked.length > 0 && (
        <span className="text-meta text-foreground">
          It checked {checked.map(([path, value], index) => <span key={path}>{index > 0 && (index === checked.length - 1 ? " against " : ", ")}{label(path)} <span className="font-mono tabular-nums">{shown(path, value)}</span></span>)}.
        </span>
      )}
      <span className="text-meta text-foreground">
        {held.length > 0 && held.length < checked.length
          ? held.map(([path, value]) => `${sentence(label(path))} is ${shown(path, value)} on the record`).join("; ") + "; change the values so the rule holds."
          : "Change the values so the rule holds."}
      </span>
      {technical && (
        <span className="grid gap-0.5 font-mono text-xs text-muted-foreground">
          <span>{code} · {reason.subject}</span>
          {reason.pin?.artifactId !== undefined && <span>{reason.pin.artifactId}@{reason.pin.version ?? "?"}</span>}
          <span className="whitespace-pre-wrap">{explanation}</span>
        </span>
      )}
    </div>
  )
}
