import type { Schemas } from "../../norn-sdk/src/index.ts"
import { exactSource, member } from "./exact.ts"
import { escape, renderShell, type ShellSection } from "./shell.ts"

type Question = Schemas["NornIrQuestion"]
type Answer = Schemas["NornQueryResult"]
type Historical = Schemas["NornHistoricalQuestionResult"]
type Row = Schemas["NornQueryRow"]

/** A server document and the exact JSON text of the same read. */
export interface ExactRead<T> {
  readonly document: T
  readonly source: string
}

/** One typed question with its current answer and, optionally, the answer at a knowledge cutoff. */
export interface QuestionCard {
  readonly question: ExactRead<Question>
  readonly answer: ExactRead<Answer>
  readonly asOf?: ExactRead<Historical>
}

const code = (value: string): string => `<code>${escape(value)}</code>`
const instant = (ms: number): string => new Date(ms).toISOString()

/** Number texts of one read, addressed by pointer. */
type Texts = ReadonlyMap<string, string>

const FRESHNESS: Record<Answer["freshness"], { readonly label: string; readonly meaning: string }> = {
  exact: { label: "Exact", meaning: "Complete coverage with zero heartbeat lag." },
  fresh: { label: "Fresh", meaning: "Complete coverage within the admitted heartbeat lag." },
  stale: { label: "Stale", meaning: "Known coverage at an aged frontier." },
  incomplete: { label: "Incomplete", meaning: "Missing coverage or explicit exclusions." },
}

const OPERATOR: Record<string, string> = { eq: "=", ne: "≠", lt: "<", le: "≤", gt: ">", ge: "≥" }

const path = ({ links, property }: { readonly links?: readonly string[]; readonly property: string }): string =>
  [...(links ?? []), property].join(".")

function literal(value: unknown, exact: string | undefined): string {
  if (exact !== undefined) return `<span class="exact">${escape(exact)}</span>`
  return code(typeof value === "string" ? value : JSON.stringify(value))
}

function decimal(texts: Texts, pointer: string): string {
  const text = texts.get(pointer)
  return text === undefined ? '<span class="absent">unavailable</span>' : `<span class="exact">${escape(text)}</span>`
}

function freshnessPolicy(policy: Question["freshness"]): string {
  switch (policy.kind) {
    case "live": return `live, at most ${escape(String(policy.maxAgeMs))} ms old`
    case "delayed": return `delayed, at most ${escape(String(policy.maxAgeMs))} ms old`
    case "as_of": return `as of ${code(policy.at)}`
  }
}

function definition({ document: question, source }: ExactRead<Question>): string {
  const texts = exactSource(question, source)
  const metrics = question.aggregate.map(({ metric }) => code(metric)).join(", ")
  const filters = (question.filter ?? []).map((filter, index) =>
    `${code(path(filter.path))} ${escape(OPERATOR[filter.operator] ?? filter.operator)} ${literal(filter.value, texts.get(`/filter/${index}/value`))}`)
  const groups = (question.groupBy ?? []).map((dimension) =>
    dimension.grain ? `${escape(dimension.grain)} of ${code(path(dimension.path))}` : code(path(dimension.path)))
  return `<dl>
  <dt>Measures</dt><dd>${metrics} over ${code(question.objectSet)}</dd>
  ${filters.length ? `<dt>Where</dt><dd>${filters.join(" and ")}</dd>` : ""}
  ${groups.length ? `<dt>By</dt><dd>${groups.join(", ")}</dd>` : ""}
  <dt>Window</dt><dd>${code(question.window.start)} to ${code(question.window.end)} <span class="sub">(end excluded)</span></dd>
  <dt>Freshness</dt><dd>${freshnessPolicy(question.freshness)}</dd>
</dl>`
}

function bucketLabel(row: Row, texts: Texts, pointer: string): string {
  if (row.bucket === null) return "All"
  return literal(row.bucket, texts.get(`${pointer}/bucket`))
}

function frontier(answer: Answer): string {
  const rows = Object.entries(answer.sourceCursors).flatMap(([source, partitions]) =>
    Object.entries(partitions).map(([partition, cursor]) => {
      const watermark = answer.watermarks[source]?.[partition]
      const heartbeat = watermark?.heartbeatMs == null ? '<span class="absent">unavailable</span>' : code(instant(watermark.heartbeatMs))
      const coverage = watermark ? (watermark.complete ? "complete" : "incomplete") : '<span class="absent">no watermark</span>'
      return `<tr><td>${code(source)}</td><td>${code(partition)}</td><td>${code(cursor)}</td><td>${heartbeat}</td><td>${coverage}</td></tr>`
    }))
  return rows.length
    ? `<div class="scroll"><table><thead><tr><th>Source</th><th>Partition</th><th>Cursor</th><th>Heartbeat</th><th>Coverage</th></tr></thead><tbody>${rows.join("")}</tbody></table></div>`
    : '<p class="absent">This answer depends on no external source.</p>'
}

function quarantine(answer: Answer): string {
  const excluded = Object.entries(answer.exclusions)
  if (!excluded.length) return ""
  const items = excluded.map(([identity, pin]) => `<li>${code(identity)} <span class="sub">finding ${code(pin)}</span></li>`).join("")
  return `<section class="callout" aria-label="Quarantined inputs">
  <h4>${excluded.length} quarantined ${excluded.length === 1 ? "input" : "inputs"} excluded</h4>
  <p>The server left these out of every bucket, so the answer is incomplete until they are resolved.</p>
  <ul class="plain">${items}</ul>
</section>`
}

function answerTable(answer: Answer, texts: Texts, prefix: string, anchor: string, dimension: string): string {
  const keys = Object.keys(answer.rows)
  if (!keys.length) return '<p class="empty">No object in the authorized population matched this question.</p>'
  const ratio = keys.some((key) => (answer.rows[key]?.totals.length ?? 0) > 1)
  const head = `<tr><th>${escape(dimension)}</th><th class="num">Value</th>${ratio ? '<th class="num">Numerator</th><th class="num">Denominator</th>' : '<th class="num">Sum</th>'}<th class="num">Inputs</th></tr>`
  const body = keys.map((key, index) => {
    const row = answer.rows[key] as Row
    const pointer = `${prefix}/rows/${member(key)}`
    const totals = row.totals.map((_, total) => `<td class="num">${decimal(texts, `${pointer}/totals/${total}`)}</td>`).join("")
    const inputs = Object.keys(row.members).length
    return `<tr><td>${bucketLabel(row, texts, pointer)}</td><td class="num value"><a href="#${anchor}-${index}" aria-label="Where this number comes from">${decimal(texts, `${pointer}/value`)}</a></td>${totals}<td class="num">${inputs}</td></tr>`
  }).join("")
  return `<div class="scroll"><table class="answer"><thead>${head}</thead><tbody>${body}</tbody></table></div>`
}

function drillThrough(answer: Answer, texts: Texts, anchor: string): string {
  const buckets = Object.entries(answer.rows).map(([key, row], index) => {
    const pointer = `/rows/${member(key)}`
    const members = Object.entries(row.members).map(([identity, contribution]) => {
      const amounts = contribution.amounts.map((amount, position) => `<td class="num">${amount === null
        ? '<span class="absent">excluded by a filter</span>'
        : decimal(texts, `${pointer}/members/${member(identity)}/amounts/${position}`)}</td>`).join("")
      return `<tr><td>${code(identity)}</td><td class="num">${escape(String(contribution.version))}</td>${amounts}</tr>`
    }).join("")
    const operands = row.totals.length > 1 ? '<th class="num">Numerator</th><th class="num">Denominator</th>' : '<th class="num">Amount</th>'
    return `<section class="drill" id="${anchor}-${index}" aria-label="Inputs of one bucket"><h5>${bucketLabel(row, texts, pointer)} <span class="sub">value ${decimal(texts, `${pointer}/value`)} from ${Object.keys(row.members).length} object versions</span></h5>
  <div class="scroll"><table><thead><tr><th>Object</th><th class="num">Version</th>${operands}</tr></thead><tbody>${members}</tbody></table></div>
</section>`
  }).join("")
  return `<h4>Where each number comes from</h4>
<p class="sub">Select a value to see the object versions it is the exact sum of. Lineage ${code(answer.lineageRef)}.</p>
${buckets}
<p class="absent">Source rows and documents behind each object version are not exposed by the server.</p>`
}

function restatement(answer: Answer, texts: Texts, asOf: ExactRead<Historical>): string {
  const past = exactSource(asOf.document, asOf.source)
  const then = asOf.document.result
  const keys = [...new Set([...Object.keys(then.rows), ...Object.keys(answer.rows)])]
  const changed = keys.flatMap((key) => {
    const pointer = `/rows/${member(key)}`
    const before = past.get(`/result${pointer}/value`)
    const after = texts.get(`${pointer}/value`)
    if (before === after) return []
    const row = (answer.rows[key] ?? then.rows[key]) as Row
    const label = bucketLabel(row, answer.rows[key] ? texts : past, answer.rows[key] ? pointer : `/result${pointer}`)
    return [`<tr><td>${label}</td><td class="num">${decimal(past, `/result${pointer}/value`)}</td><td class="num">${decimal(texts, `${pointer}/value`)}</td></tr>`]
  })
  const unchanged = keys.length - changed.length
  return `<h4>Restatement</h4>
<p class="sub">Known as of commit ${escape(String(then.knownAsOf))} at ${code(instant(asOf.document.knowledgeTimeMs))}, compared with commit ${escape(String(answer.knownAsOf))} now. ${unchanged} ${unchanged === 1 ? "bucket is" : "buckets are"} unchanged.</p>
${changed.length
    ? `<div class="scroll"><table><thead><tr><th>Bucket</th><th class="num">Then</th><th class="num">Now</th></tr></thead><tbody>${changed.join("")}</tbody></table></div>`
    : '<p class="absent">No bucket changed between these knowledge times.</p>'}`
}

function card({ question, answer, asOf }: QuestionCard, index: number): string {
  const result = answer.document
  const texts = exactSource(result, answer.source)
  const anchor = `question-${index}`
  const freshness = FRESHNESS[result.freshness]
  const dimension = question.document.groupBy?.[0]
  const dimensionLabel = dimension ? `${dimension.grain ? `${dimension.grain} of ` : ""}${path(dimension.path)}` : "Bucket"
  return `<article class="card" id="${anchor}" aria-labelledby="${anchor}-name">
  <header><h3 id="${anchor}-name">${escape(question.document.name)}</h3><span class="chip freshness ${escape(result.freshness)}" title="${escape(freshness.meaning)}">${escape(freshness.label)}</span></header>
  ${definition(question)}
  ${quarantine(result)}
  <ul class="facts" aria-label="Read">
    <li><span>Known as of</span><b>commit ${escape(String(result.knownAsOf))}</b></li>
    <li><span>Effective until</span><b>${escape(result.effectiveAsOf)}</b></li>
    <li><span>Observed</span><b>${escape(instant(result.nowMs))}</b></li>
    <li><span>Engine</span><b>${escape(result.engine)}</b></li>
  </ul>
  <p class="sub">${escape(freshness.meaning)}</p>
  ${answerTable(result, texts, "", anchor, dimensionLabel)}
  ${asOf ? restatement(result, texts, asOf) : ""}
  ${drillThrough(result, texts, anchor)}
  <h4>Source frontier</h4>
  ${frontier(result)}
  <details><summary>Pins</summary><dl>
    <dt>Query</dt><dd>${code(result.queryPin)}</dd>
    <dt>Definition</dt><dd>${code(result.definitionPin)}</dd>
    <dt>Mapping</dt><dd>${code(result.mappingPin)}</dd>
    <dt>Population</dt><dd>${code(result.populationPin)} <span class="sub">authorization epoch ${escape(String(result.authorizationEpoch))}</span></dd>
    <dt>Ledger head</dt><dd>${code(result.revision.hash)} <span class="sub">seq ${escape(String(result.revision.seq))}</span></dd>
    <dt>World</dt><dd>${code(result.world)} <span class="sub">scope ${code(result.scope)}</span></dd>
  </dl></details>
</article>`
}

/**
 * The data app's question board: each typed question beside the server's
 * exact answer, its freshness and frontier, its quarantined inputs and the
 * object versions behind every number. Nothing is recomputed here.
 */
export function renderQuestionBoard({ questions, title = "Questions" }: {
  readonly questions: readonly QuestionCard[]
  readonly title?: string
}): string {
  const sections: ShellSection[] = questions.map(({ question }, index) => ({ id: `question-${index}`, label: question.document.name }))
  return renderShell({
    title,
    eyebrow: "Data app",
    intro: "Typed questions answered exactly by the server. Every number opens to the object versions it was computed from.",
    observation: "Observed from the server's answers. Each card states the frontier and freshness of its own read.",
    sections,
    body: questions.length ? questions.map(card).join("") : '<p class="empty">This board has no questions.</p>',
  })
}
