import { ChevronRight, CircleCheck, CircleX, Scale, Sparkles } from "lucide-react"
import type { ReactNode } from "react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { STAGE, type Verdict } from "./verdict.ts"

/**
 * The reason a person reads: the checker's own `message: "…"` texts when it nests its findings
 * (`[Diagnostic { code: …, message: "…", span: … }]`), else its whole message, verbatim either way.
 */
export function reasons(message: string): string[] {
  const nested = [...message.matchAll(/message: "((?:[^"\\]|\\.)*)"/g)].map(([, text = ""]) => text.replaceAll('\\"', '"'))
  return nested.length > 0 ? nested : [message]
}

const where = ({ artifact, pointer }: { readonly artifact: string; readonly pointer: string }) => `${artifact} ${pointer}`

/** The technical facts behind a verdict, collapsed: where the proposal pointed and what the checker wrote. */
function Details({ children }: { readonly children: ReactNode }) {
  return (
    <details className="group/details text-meta text-muted-foreground">
      <summary className="flex w-fit cursor-pointer list-none items-center gap-1 rounded-sm outline-none hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/30 [&::-webkit-details-marker]:hidden">
        <ChevronRight aria-hidden className="size-3.5 transition-transform group-open/details:rotate-90" />Details
      </summary>
      <div className="mt-2 space-y-2 border-s ps-3">{children}</div>
    </details>
  )
}

const Mono = ({ children }: { readonly children: ReactNode }) => <div className="font-mono text-[11px] leading-relaxed break-all whitespace-pre-wrap">{children}</div>

/** What the server's checker said about one proposal; Studio adds no verdict of its own. */
export function VerdictCard({ verdict }: { readonly verdict: Verdict }) {
  if ("error" in verdict) {
    return (
      <div className="enter space-y-1.5 rounded-xl border bg-card p-3.5 text-sm">
        <div className="flex items-center gap-2 font-medium"><CircleX className="size-4 text-destructive" />Not a change the checker can read</div>
        <p className="text-muted-foreground">{verdict.error}</p>
      </div>
    )
  }
  if (verdict.admitted) {
    return (
      <div className="enter space-y-2 rounded-xl border border-success/40 bg-success/5 p-3.5 text-sm">
        <div className="flex items-center gap-2 font-semibold text-success"><CircleCheck className="size-4" />Admitted by the checker</div>
        <p className="text-muted-foreground">Checked against every law, not installed.</p>
        <Details>
          <Mono>{where(verdict.proposal)}</Mono>
          <div>Receipt <span className="font-mono">{verdict.receiptSha256?.slice(0, 12) ?? "none"}</span></div>
        </Details>
      </div>
    )
  }
  return (
    <div className="enter overflow-hidden rounded-xl border border-destructive/40 bg-card text-sm">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 border-b border-destructive/20 bg-destructive/5 px-3.5 py-2">
        <CircleX className="size-4 text-destructive" /><span className="font-semibold">Refused by the checker</span>
        <span className="ms-auto text-meta text-muted-foreground">{STAGE[verdict.stage] ?? verdict.stage}</span>
      </div>
      <div className="divide-y">
        {verdict.refusals.map((refusal, index) => (
          <div key={`${refusal.code}:${index}`} className="space-y-2.5 px-3.5 py-3">
            {refusal.law && (
              <div className="flex items-start gap-2.5">
                <span className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-lg bg-muted"><Scale className="size-4 text-muted-foreground" /></span>
                <div className="min-w-0 space-y-0.5">
                  <div className="text-meta text-muted-foreground">It would break the law</div>
                  <div className="text-[0.9375rem] leading-snug font-semibold">{refusal.law.name ?? refusal.law.artifactId}</div>
                  {refusal.law.rationale && <p className="text-muted-foreground">{refusal.law.rationale}</p>}
                  {refusal.law.expression && <code className="inline-block pt-0.5 font-mono text-meta text-muted-foreground">{refusal.law.expression}</code>}
                </div>
              </div>
            )}
            <div className="space-y-1">{reasons(refusal.message).map((reason) => <p key={reason}>{reason}</p>)}</div>
            <Details>
              <div className="flex flex-wrap items-center gap-2"><Badge variant="outline" className="font-mono">{refusal.code}</Badge></div>
              <Mono>{where(verdict.proposal)}</Mono>
              {reasons(refusal.message)[0] !== refusal.message && <Mono>{refusal.message}</Mono>}
            </Details>
          </div>
        ))}
      </div>
    </div>
  )
}

/** After a refusal: stop here, or ask the agent for a change the checker would admit. */
export function DecisionCard({ onStop, onAlternative, disabled }: { readonly onStop: () => void; readonly onAlternative: () => void; readonly disabled: boolean }) {
  return (
    <div className="enter flex flex-wrap items-center gap-2 rounded-xl border bg-card p-2 ps-3.5 text-sm">
      <span className="me-auto text-muted-foreground">What next?</span>
      <Button size="sm" variant="ghost" onClick={onStop} disabled={disabled}>Stop</Button>
      <Button size="sm" onClick={onAlternative} disabled={disabled}><Sparkles />Try an alternative</Button>
    </div>
  )
}
