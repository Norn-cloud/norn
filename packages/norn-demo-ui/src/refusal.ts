import type { Schemas } from "../../norn-sdk/src/index.ts"
import { exactSource, member } from "./exact.ts"
import { escape, renderShell } from "./shell.ts"

type Refusal = Schemas["Refusal"]
type Pin = Schemas["NornIrDataQualityRuleRef"]

/** A `norn/refusal@2` response and the exact JSON text the server returned. */
export interface RefusalResponse {
  readonly refusal: Refusal
  readonly source: string
}

const code = (value: string): string => `<code>${escape(value)}</code>`

function operand(value: unknown, exact: string | undefined): string {
  if (exact !== undefined) return `<code class="exact">${escape(exact)}</code>`
  return code(typeof value === "string" ? value : JSON.stringify(value))
}

function pin(value: Pin | null | undefined): string {
  if (!value) return '<p class="absent">The server did not pin a declaration for this reason.</p>'
  switch (value.kind) {
    case "law": return `<dl><dt>Law</dt><dd>${code(value.lawId)}</dd><dt>Declared in</dt><dd>${code(value.artifactId)}</dd><dt>Declaration digest</dt><dd>${code(value.version)}</dd></dl>`
    case "invariant": return `<dl><dt>Invariant</dt><dd>${code(value.invariantId)}</dd><dt>Owner</dt><dd>${escape(value.owner.kind)} ${code(value.owner.name)}</dd><dt>Definition digest</dt><dd>${code(value.version)}</dd></dl>`
    case "builtin": return `<dl><dt>Built-in rule</dt><dd>${code(value.id)}</dd><dt>Semantics version</dt><dd>${code(value.version)}</dd></dl>`
  }
}

/** The refused state of a submitted action, with exactly what the server disclosed. */
function refusedPanel({ refusal, source }: RefusalResponse): string {
  const exact = exactSource(refusal, source)
  const reason = refusal.reason
  const disclosed = reason
    ? (() => {
      const rows = Object.entries(reason.values).map(([name, value]) =>
        `<tr><td>${code(name)}</td><td>${operand(value, exact.get(`/reason/values/${member(name)}`))}</td></tr>`).join("")
      return `<h4>Governing declaration</h4>${pin(reason.pin)}
<dl><dt>Reason</dt><dd>${code(reason.law)}</dd><dt>Subject</dt><dd>${code(reason.subject)}</dd></dl>
${rows ? `<h4>Values at refusal</h4><div class="scroll"><table><thead><tr><th>Operand</th><th>Exact value</th></tr></thead><tbody>${rows}</tbody></table></div>` : ""}`
    })()
    : '<h4>Governing declaration</h4><p class="absent">The server did not disclose the governing law or its values for this refusal.</p>'
  return `<ol class="states" aria-label="Action status">
  <li class="done">Submitted</li><li class="current refused" aria-current="step">Refused</li>
</ol>
<section class="card assurance" aria-labelledby="effects"><h3 id="effects">Nothing changed</h3>
  <p class="desc">A refused action commits no effects: its writes, outbox entries and timers either all commit or none do. Only the refusal is recorded.</p>
  <p class="sub">Guarantee <code>TXN-ATOMIC-1</code></p>
</section>
<section class="card" aria-labelledby="why"><h3 id="why">Why it was refused</h3>
  <dl>
    <dt>Code</dt><dd>${code(refusal.code)}</dd>
    <dt>Category</dt><dd><span class="chip">${escape(refusal.category)}</span></dd>
    <dt>Retry</dt><dd>${refusal.retryable ? "The server marks this refusal retryable." : "Not marked retryable by the server."}</dd>
  </dl>
  ${disclosed}
  <h4>How to fix it</h4><p class="absent">A repair hint is not exposed by the server.</p>
</section>`
}

/**
 * "Why was this refused?" for one action response. The page renders only the
 * server's projection: undisclosed pins and operands stay absent, and no
 * category is reinterpreted on the client.
 */
export function renderRefusalDocument(response: RefusalResponse): string {
  return renderShell({
    title: response.refusal.message,
    eyebrow: "Why was this refused?",
    intro: response.refusal.explanation,
    observation: "Observed from the server's response to this action.",
    sections: [],
    body: refusedPanel(response),
  })
}
