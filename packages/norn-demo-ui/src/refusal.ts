import type { Schemas } from "../../norn-sdk/src/index.ts"
import { escape, renderShell } from "./shell.ts"

type Refusal = Schemas["Refusal"]
type Pin = Schemas["NornIrDataQualityRuleRef"]

/** A `norn/refusal@2` response and the exact JSON text the server returned. */
export interface RefusalResponse {
  readonly refusal: Refusal
  readonly source: string
}

const code = (value: string): string => `<code>${escape(value)}</code>`

const space = (source: string, at: number): number => {
  while (at < source.length && " \t\n\r".includes(source[at] ?? "")) at += 1
  return at
}

const malformed = (): never => { throw new Error("Refusal source is not the JSON of one response") }

/** End offset of the JSON value starting at `at`. */
function valueEnd(source: string, at: number): number {
  const first = source[at]
  if (first === '"') {
    let index = at + 1
    while (source[index] !== '"') {
      if (index >= source.length) malformed()
      index += source[index] === "\\" ? 2 : 1
    }
    return index + 1
  }
  if (first === "{" || first === "[") {
    let depth = 0
    let index = at
    do {
      if (index >= source.length) malformed()
      const character = source[index]
      if (character === '"') { index = valueEnd(source, index); continue }
      if (character === "{" || character === "[") depth += 1
      if (character === "}" || character === "]") depth -= 1
      index += 1
    } while (depth > 0)
    return index
  }
  let index = at
  while (index < source.length && !",}] \t\n\r".includes(source[index] ?? "")) index += 1
  return index === at ? malformed() : index
}

/** Members of the object starting at `at`, as decoded keys and exact value text. */
function members(source: string, at: number): Map<string, { readonly start: number; readonly text: string }> | undefined {
  if (source[at] !== "{") return undefined
  const result = new Map<string, { readonly start: number; readonly text: string }>()
  let index = space(source, at + 1)
  while (source[index] !== "}") {
    if (source[index] !== '"') malformed()
    const keyEnd = valueEnd(source, index)
    const key = JSON.parse(source.slice(index, keyEnd)) as string
    const colon = space(source, keyEnd)
    if (source[colon] !== ":") malformed()
    const start = space(source, colon + 1)
    const end = valueEnd(source, start)
    result.set(key, { start, text: source.slice(start, end) })
    index = space(source, end)
    if (source[index] === ",") index = space(source, index + 1)
    else if (source[index] !== "}") malformed()
  }
  return result
}

/**
 * Exact operand text from the response bytes: decimals keep their scale
 * (`100.000` stays `100.000`), which a parsed JavaScript number cannot.
 */
function operandText(source: string): ReadonlyMap<string, string> {
  const reason = members(source, space(source, 0))?.get("reason")
  const values = reason && members(source, reason.start)?.get("values")
  const operands = values && members(source, values.start)
  if (!operands) throw new Error("Refusal source does not carry the reason's values")
  return new Map([...operands].map(([key, { text }]) => [key, text]))
}

/** Structural JSON equality, independent of member order. */
function same(left: unknown, right: unknown): boolean {
  if (left === right) return true
  if (typeof left !== "object" || typeof right !== "object" || left === null || right === null) return false
  if (Array.isArray(left) !== Array.isArray(right)) return false
  const leftKeys = Object.keys(left)
  const rightKeys = Object.keys(right)
  return leftKeys.length === rightKeys.length && leftKeys.every((key) =>
    Object.hasOwn(right, key) && same((left as Record<string, unknown>)[key], (right as Record<string, unknown>)[key]))
}

function operand(text: string): string {
  if (text.startsWith('"')) return code(JSON.parse(text) as string)
  return `<code class="exact">${escape(text)}</code>`
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
  // The whole source (no trailing text) must be this refusal, field for field.
  if (!same(JSON.parse(source), refusal)) throw new Error("Refusal source and parsed refusal differ")
  const reason = refusal.reason
  const disclosed = reason
    ? (() => {
      const exact = operandText(source)
      const rows = [...exact].map(([name, text]) => `<tr><td>${code(name)}</td><td>${operand(text)}</td></tr>`).join("")
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
