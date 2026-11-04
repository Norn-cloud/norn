import { CircleCheck, CircleX, Scale } from "lucide-react"
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

/** What the server's checker said about one proposal; Studio adds no verdict of its own. */
export function VerdictCard({ verdict }: { readonly verdict: Verdict }) {
  if ("error" in verdict) {
    return (
      <div className="rounded-xl border bg-card p-3 text-sm">
        <div className="flex items-center gap-2 font-medium"><CircleX className="size-4 text-destructive" />Not a change the checker can read</div>
        <p className="mt-1 text-muted-foreground">{verdict.error}</p>
      </div>
    )
  }
  if (verdict.admitted) {
    return (
      <div className="rounded-xl border bg-card p-3 text-sm">
        <div className="flex items-center gap-2 font-medium"><CircleCheck className="size-4 text-success" />Admitted by the checker</div>
        <p className="mt-1 font-mono text-meta break-all text-muted-foreground">{where(verdict.proposal)}</p>
        <p className="mt-1 text-muted-foreground">Checked, not installed. Receipt {verdict.receiptSha256?.slice(0, 12) ?? "none"}.</p>
      </div>
    )
  }
  return (
    <div className="space-y-2 rounded-xl border border-destructive/40 bg-card p-3 text-sm">
      <div className="flex flex-wrap items-center gap-2 font-medium">
        <CircleX className="size-4 text-destructive" />Refused by the checker
        <Badge variant="outline">{STAGE[verdict.stage] ?? verdict.stage}</Badge>
      </div>
      <p className="font-mono text-meta break-all text-muted-foreground">{where(verdict.proposal)}</p>
      {verdict.refusals.map((refusal, index) => (
        <div key={`${refusal.code}:${index}`} className="space-y-1 rounded-lg bg-muted px-3 py-2">
          {refusal.law && (
            <div className="flex items-start gap-2">
              <Scale className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
              <div>
                <div className="font-medium">{refusal.law.name ?? refusal.law.artifactId}</div>
                {refusal.law.rationale && <div className="text-muted-foreground">{refusal.law.rationale}</div>}
                {refusal.law.expression && <code className="text-meta">{refusal.law.expression}</code>}
              </div>
            </div>
          )}
          {reasons(refusal.message).map((reason) => <div key={reason}>{reason}</div>)}
          <div className="flex flex-wrap items-center gap-2">
            <Badge variant="outline" className="font-mono">{refusal.code}</Badge>
            {reasons(refusal.message)[0] !== refusal.message && (
              <details className="text-meta text-muted-foreground">
                <summary className="cursor-pointer">Details</summary>
                <pre className="mt-1 whitespace-pre-wrap break-all font-mono">{refusal.message}</pre>
              </details>
            )}
          </div>
        </div>
      ))}
    </div>
  )
}

/** After a refusal: stop here, or ask the agent for a change the checker would admit. */
export function DecisionCard({ onStop, onAlternative, disabled }: { readonly onStop: () => void; readonly onAlternative: () => void; readonly disabled: boolean }) {
  return (
    <div className="flex flex-wrap items-center gap-2 rounded-xl border bg-card p-3 text-sm">
      <span className="me-auto text-muted-foreground">What next?</span>
      <Button size="sm" variant="outline" onClick={onStop} disabled={disabled}>Stop</Button>
      <Button size="sm" onClick={onAlternative} disabled={disabled}>Try an alternative</Button>
    </div>
  )
}
