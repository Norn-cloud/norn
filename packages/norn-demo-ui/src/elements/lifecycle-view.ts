// `<norn-lifecycle-view>` (UI-LIFECYCLE-2): an object type's lifecycle as the declaration draws
// it, each state with the cell's live count, and the checker's counterexamples stepped through.
//
// Counts are the server's: one live read of the type's facets on the lifecycle property
// (`GET /v1/objects/{type}:facets?properties=<property>`, HOST-READ-7). A declared state the
// answer does not list has no rows (0); a count past the cell's scan bound reads as a lower
// bound; rows the reader may not see are said, never counted. The view computes nothing a count
// could be built from. A counterexample is a checker finding's `witness`: its steps name which
// machine moved and by which transition, so the view highlights each lifecycle transition the
// run takes and the state it lands in. Nothing is read from a finding's message.

import { createDriver, type Driver } from "../machine/driver.ts"
import type { Machine } from "../machine/machine.ts"
import { classifyResponse } from "../machine/outcome.ts"
import { adoptLifecycleStyles } from "./lifecycle.css.ts"

export interface LifecycleTransitionDoc {
  readonly id: string
  readonly from: readonly string[]
  readonly to: string
}

/** The effective lifecycle, as the catalog declares it. */
export interface LifecycleDoc {
  readonly property: string
  readonly states: readonly string[]
  readonly initial: readonly string[]
  readonly terminal: readonly string[]
  readonly transitions: readonly LifecycleTransitionDoc[]
  readonly endings?: readonly { readonly state: string; readonly outcome: string }[]
}

/** The cell's facets answer (HOST-READ-7), as far as the view reads it. */
export interface FacetsAnswer {
  readonly rows: number
  readonly withheldRows: number
  readonly quarantinedRows: number
  readonly approximate: boolean
  readonly facets: readonly {
    readonly property: string
    readonly values: readonly { readonly value: unknown; readonly count: number }[]
    readonly otherRows: number
    readonly nulls: number
  }[]
}

export interface WitnessMove {
  readonly side: "ui" | "backend"
  readonly label: string
}

/** A checker finding with its run (`norn-checker/diagnostics@1`, `witness`). */
export interface LifecycleFinding {
  readonly code: string
  readonly message: string
  /** The page state the run ends in, from the widget's own machine (the host knows it, not the witness). */
  readonly pageState?: string
  readonly witness: { readonly steps: readonly { readonly edge: number; readonly moves: readonly WitnessMove[] }[] }
}

export type Locale = "en" | "ar"

/** One live answer: what the cell's GET returned, as the subscriber, now. */
export interface LiveAnswer {
  readonly status: number
  readonly body: unknown
}

/** The view's admitted widget document (`lifecycle-view.widget.json`): its load states as a machine. */
export interface LifecycleViewDocument {
  readonly payload: { readonly ux: { readonly pages: readonly { readonly machine: Machine }[] } }
}

export interface LifecycleViewOptions {
  /** The admitted document whose machine decides the view's load states (UI-MACHINE-1). */
  readonly document: LifecycleViewDocument
  readonly lifecycle: LifecycleDoc
  /** The object type's title, as the host names it. */
  readonly title: string
  /** Read the type's facets on the lifecycle property once (the cell's GET, as the reader). */
  readonly read: () => Promise<LiveAnswer>
  /** Subscribe to changes of that answer (the live read); each new answer is read again. Returns the unsubscribe. */
  readonly changes: (listener: (answer: LiveAnswer) => void) => () => void
  /** Findings with a witness for this lifecycle (`widget.lifecycle.*`). */
  readonly findings?: readonly LifecycleFinding[]
  readonly locale?: Locale
  /** A state's or transition's display name; defaults to its id in words, sentence case. */
  readonly label?: (id: string) => string
  /** Technical view: each finding also shows its code and the checker's own words. */
  readonly technical?: boolean
  /** Open one finding's stepper at a step (a deep link, or a design specimen). */
  readonly open?: { readonly finding: number; readonly step: number }
}

const WORDS = {
  en: {
    rows: (n: string) => `${n} records`, start: "start", end: "ending", rests: "can rest here", none: "—",
    pending: "Counting…", refused: "You can't see these records' counts.", unavailable: "The cell did not answer.",
    withheld: (n: number) => `${n} withheld from you`, quarantined: (n: number) => `${n} held in quarantine`, unset: (n: number) => `${n} without a state`,
    other: (n: number) => `${n} in states this view does not list`, atLeast: (n: string) => `≥ ${n}`,
    findings: "Counterexamples the checker found", step: (i: string, n: string) => `Step ${i} of ${n}`, prev: "Previous step", next: "Next step",
    page: "page", object: "object", opened: "Where the page opens",
    stuck: (page: string, type: string, state: string) => `The page can show “${page}” while the ${type} is still “${state}”, and nothing can finish it.`,
    unknown: (page: string, type: string, state: string) => `The page can show “${page}” while the ${type} is “${state}”; whether it can finish depends on the rules.`,
    unexpected: (page: string, type: string, state: string) => `The page shows “${page}” while the ${type} is “${state}”, which it does not expect.`,
    notServed: "This cell does not serve lifecycle counts yet.", failed: "The cell did not answer.", retry: "Try again", again: "Asking again shortly.",
  },
  ar: {
    rows: (n: string) => `${n} سجل`, start: "البداية", end: "نهاية", rests: "يمكن أن يستقر هنا", none: "—",
    pending: "جارٍ العد…", refused: "لا يمكنك رؤية أعداد هذه السجلات.", unavailable: "لم تُجب الخلية.",
    withheld: (n: number) => `${n} محجوبة عنك`, quarantined: (n: number) => `${n} في الحجر`, unset: (n: number) => `${n} بلا حالة`,
    other: (n: number) => `${n} في حالات لا يعرضها هذا العرض`, atLeast: (n: string) => `${n} على الأقل`,
    findings: "أمثلة مضادة وجدها المدقق", step: (i: string, n: string) => `الخطوة ${i} من ${n}`, prev: "الخطوة السابقة", next: "الخطوة التالية",
    page: "الصفحة", object: "الكائن", opened: "حيث تُفتح الصفحة",
    stuck: (page: string, type: string, state: string) => `قد تعرض الصفحة «${page}» بينما ${type} ما زال «${state}»، ولا شيء يستطيع إنهاءه.`,
    unknown: (page: string, type: string, state: string) => `قد تعرض الصفحة «${page}» بينما ${type} «${state}»؛ وإمكان إنهائه يتوقف على القواعد.`,
    unexpected: (page: string, type: string, state: string) => `تعرض الصفحة «${page}» بينما ${type} «${state}»، وهذا ما لا تتوقعه.`,
    notServed: "هذه الخلية لا تقدّم أعداد دورة الحياة بعد.", failed: "لم تُجب الخلية.", retry: "حاول مرة أخرى", again: "سنسأل مرة أخرى بعد قليل.",
  },
} as const

/** Columns by shortest distance from an initial state; states no run reaches go last. */
export function layout(lifecycle: LifecycleDoc): string[][] {
  const depth = new Map<string, number>()
  const queue = lifecycle.initial.filter((state) => lifecycle.states.includes(state))
  for (const state of queue) depth.set(state, 0)
  for (let index = 0; index < queue.length; index += 1) {
    const state = queue[index]!
    for (const transition of lifecycle.transitions) {
      if (!transition.from.includes(state) || depth.has(transition.to) || !lifecycle.states.includes(transition.to)) continue
      depth.set(transition.to, depth.get(state)! + 1)
      queue.push(transition.to)
    }
  }
  const last = Math.max(-1, ...depth.values()) + 1
  const columns: string[][] = []
  for (const state of lifecycle.states) {
    const column = depth.get(state) ?? last
    ;(columns[column] ??= []).push(state)
  }
  return columns.filter((column) => column !== undefined)
}

/** A card's edges, in the board's own coordinates. */
export interface Box {
  readonly left: number
  readonly top: number
  readonly right: number
  readonly bottom: number
}

export type Point = readonly [number, number]

/** An arrow as data: straight to `to`, or curved by one (quadratic) or two (cubic) control points. */
export interface ArrowRoute {
  readonly start: Point
  readonly segments: readonly { readonly to: Point; readonly via?: readonly Point[] }[]
  readonly label: Point
}

/** A route's SVG path data. */
export function pathData(route: ArrowRoute): string {
  const at = (point: Point) => `${point[0]} ${point[1]}`
  return `M${at(route.start)}` + route.segments.map((segment) =>
    !segment.via ? ` L${at(segment.to)}` : ` ${segment.via.length === 1 ? "Q" : "C"}${segment.via.map(at).join(" ")} ${at(segment.to)}`).join("")
}

/** Straight runs through `points`, each corner rounded. */
function rounded(points: readonly Point[], radius: number): ArrowRoute["segments"] {
  const segments: { to: Point; via?: Point[] }[] = []
  for (let index = 1; index < points.length; index += 1) {
    const [x, y] = points[index]!
    const next = points[index + 1]
    if (!next) {
      segments.push({ to: [x, y] })
      continue
    }
    const [px, py] = points[index - 1]!
    const r = Math.min(radius, Math.hypot(x - px, y - py) / 2, Math.hypot(next[0] - x, next[1] - y) / 2)
    segments.push({ to: [x - Math.sign(x - px) * r, y - Math.sign(y - py) * r] })
    segments.push({ to: [x + Math.sign(next[0] - x) * r, y + Math.sign(next[1] - y) * r], via: [[x, y]] })
  }
  return segments
}

/**
 * One arrow from card `a` to card `b`, `hop` columns ahead, drawn only where no card is: in the
 * gaps between columns and below the lowest card (`floor`). `layout` puts a transition's target at
 * most one column ahead, so a forward arrow crosses one gap. Within a column it arcs through the
 * gap on the column's far side; one column back it returns through the gap, lower on the cards
 * than the forward arrows; further back it leaves by the gap, runs under every card in lane
 * `lane`, and comes up the gap beside its target.
 */
export function routeArrow(a: Box, b: Box, hop: number, rtl: boolean, floor: number, lane: number): ArrowRoute {
  const forward = rtl ? -1 : 1
  const lead = (box: Box) => (rtl ? box.left : box.right)
  const trail = (box: Box) => (rtl ? box.right : box.left)
  const down = (box: Box, share: number) => box.top + (box.bottom - box.top) * share
  if (hop > 0) {
    const [x1, y1, x2, y2] = [lead(a), down(a, 0.5), trail(b), down(b, 0.5)]
    const bend = (x2 - x1) / 2
    return { start: [x1, y1], segments: [{ to: [x2, y2], via: [[x1 + bend, y1], [x2 - bend, y2]] }], label: [(x1 + x2) / 2, (y1 + y2) / 2 - 6] }
  }
  if (hop === 0) {
    // A transition back to its own state loops on the one card.
    const [y1, y2] = a === b ? [down(a, 0.3), down(a, 0.7)] : [down(a, 0.5), down(b, 0.5)]
    const out = (rtl ? Math.min(lead(a), lead(b)) : Math.max(lead(a), lead(b))) + forward * 28
    return { start: [lead(a), y1], segments: [{ to: [lead(b), y2], via: [[out, y1], [out, y2]] }], label: [out - forward * 4, (y1 + y2) / 2] }
  }
  const [x1, y1, x2, y2] = [trail(a), down(a, 0.72), lead(b), down(b, 0.72)]
  if (hop === -1) {
    const bend = (x2 - x1) / 2
    return { start: [x1, y1], segments: [{ to: [x2, y2], via: [[x1 + bend, y1], [x2 - bend, y2]] }], label: [(x1 + x2) / 2, (y1 + y2) / 2 + 14] }
  }
  const shift = (lane % 3) * 6
  const [leave, arrive, under] = [x1 - forward * (16 + shift), x2 + forward * (40 + shift), floor + 24 + lane * 16]
  return { start: [x1, y1], segments: rounded([[x1, y1], [leave, y1], [leave, under], [arrive, under], [arrive, y2], [x2, y2]], 8), label: [(leave + arrive) / 2, under - 5] }
}

export type StateCount = { readonly kind: "count"; readonly count: number; readonly lowerBound: boolean } | { readonly kind: "pending" }

export interface Counts {
  readonly states: ReadonlyMap<string, StateCount>
  readonly notes: readonly string[]
  readonly refused: string | null
}

/** The per-state counts of one live answer: the server's numbers, nothing derived but "absent = 0". */
export function countsFrom(lifecycle: LifecycleDoc, answer: LiveAnswer | null, locale: Locale = "en"): Counts {
  const w = WORDS[locale]
  const pending = new Map(lifecycle.states.map((state) => [state, { kind: "pending" } as StateCount]))
  if (answer === null) return { states: pending, notes: [], refused: null }
  if (answer.status === 401 || answer.status === 403 || answer.status === 404) return { states: pending, notes: [], refused: w.refused }
  if (answer.status >= 400) return { states: pending, notes: [], refused: w.unavailable }
  const body = answer.body as FacetsAnswer
  const facet = body.facets?.find((candidate) => candidate.property === lifecycle.property)
  if (!facet) return { states: pending, notes: [], refused: w.unavailable }
  const lowerBound = body.approximate === true
  const states = new Map<string, StateCount>()
  for (const state of lifecycle.states) {
    const found = facet.values.find((entry) => entry.value === state)
    states.set(state, { kind: "count", count: found?.count ?? 0, lowerBound })
  }
  const notes = [
    body.withheldRows > 0 ? w.withheld(body.withheldRows) : "",
    body.quarantinedRows > 0 ? w.quarantined(body.quarantinedRows) : "",
    facet.nulls > 0 ? w.unset(facet.nulls) : "",
    facet.otherRows > 0 ? w.other(facet.otherRows) : "",
  ].filter(Boolean)
  return { states, notes, refused: null }
}

/** After `taken` steps of a witness: the lifecycle transitions taken so far and the state the object is in. */
export function stepState(lifecycle: LifecycleDoc, finding: LifecycleFinding, taken: number): { readonly transitions: ReadonlySet<string>; readonly state: string | null } {
  const transitions = new Set<string>()
  let state: string | null = null
  for (const step of finding.witness.steps.slice(0, taken)) {
    for (const move of step.moves) {
      if (move.side !== "backend") continue
      transitions.add(move.label)
      state = lifecycle.transitions.find((transition) => transition.id === move.label)?.to ?? state
    }
  }
  return { transitions, state }
}

const escape = (text: string) => text.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`)

/** Mount the view into `root`. Returns the disposer (it ends the live read). */
export function mountLifecycleView(root: HTMLElement, options: LifecycleViewOptions): () => void {
  const { lifecycle } = options
  const locale = options.locale ?? "en"
  const w = WORDS[locale]
  // The definition's ids in words, sentence case ("on_accepted" reads "On accepted").
  const label = options.label ?? ((id: string) => { const words = id.replaceAll(/[_-]/g, " "); return words.charAt(0).toUpperCase() + words.slice(1) })
  const number = new Intl.NumberFormat(locale === "ar" ? "ar-EG" : "en")
  const findings = (options.findings ?? []).filter((finding) => finding.witness.steps.length > 0)
  adoptLifecycleStyles(root.ownerDocument)
  const endings = new Map((lifecycle.endings ?? []).map((ending) => [ending.state, ending.outcome]))
  const role = (state: string) =>
    [lifecycle.initial.includes(state) ? w.start : "", lifecycle.terminal.includes(state) ? w.end : endings.get(state) === "resting" ? w.rests : ""].filter(Boolean).join(" · ")
  /** A finding in the reader's words: the page state and the object's state at the end of its run. */
  const headline = (finding: LifecycleFinding) => {
    const object = stepState(lifecycle, finding, finding.witness.steps.length).state
    if (finding.pageState === undefined || object === null) return finding.message
    const type = locale === "en" ? options.title.toLowerCase() : options.title
    const words = finding.code.endsWith(".stuck") ? w.stuck : finding.code.endsWith(".completion_unknown") ? w.unknown : finding.code.endsWith(".assumption_violated") ? w.unexpected : null
    return words === null ? finding.message : words(label(finding.pageState), type, label(object))
  }
  const columns = layout(lifecycle)
  const column = new Map(columns.flatMap((states, index) => states.map((state) => [state, index] as const)))
  // An arrow more than one column back runs under the cards; the board keeps room for each.
  const lanes = lifecycle.transitions.reduce((count, transition) =>
    count + transition.from.filter((from) => (column.get(transition.to) ?? 0) - (column.get(from) ?? 0) < -1).length, 0)
  root.innerHTML = `
    <section class="norn-life" dir="${locale === "ar" ? "rtl" : "ltr"}" lang="${locale}">
      <header class="norn-life-head">
        <h2 class="norn-life-title">${escape(options.title)} · ${escape(label(lifecycle.property))}</h2>
        <p class="norn-life-meta" data-region="total" aria-live="polite"></p>
      </header>
      <p class="norn-life-refused" data-region="refused" role="status" hidden></p>
      <p class="norn-life-refused" data-region="unavailable" role="status" hidden><span data-region="unavailable-text"></span> <button type="button" class="norn-life-retry" data-node="retry-button">${escape(w.retry)}</button></p>
      <div class="norn-life-board" data-region="board" style="--norn-life-lanes:${lanes}">
        <svg class="norn-life-arrows" aria-hidden="true"><defs>${["", "-on"].map((on) =>
          `<marker id="norn-life-head${on}" viewBox="0 0 8 8" refX="7" refY="4" markerUnits="userSpaceOnUse" markerWidth="8" markerHeight="8" orient="auto"><path d="M0 0 8 4 0 8z"/></marker>`).join("")}</defs></svg>
        ${columns.map((column) => `<ol class="norn-life-col">${column.map((state) => `
          <li class="norn-life-state" data-state="${escape(state)}"${lifecycle.terminal.includes(state) ? " data-ending" : ""}>
            <span class="norn-life-name">${escape(label(state))}</span>
            <span class="norn-life-count" data-count aria-live="polite"></span>
            <span class="norn-life-role">${escape(role(state))}</span>
          </li>`).join("")}</ol>`).join("")}
      </div>
      <p class="norn-life-foot" data-region="notes" hidden></p>
      ${findings.length === 0 ? "" : `
      <section class="norn-life-findings" aria-label="${escape(w.findings)}">
        <h3>${escape(w.findings)}</h3>
        ${findings.map((finding, index) => `
          <article class="norn-life-finding" data-finding="${index}">
            <button type="button" data-pick="${index}">${escape(headline(finding))}</button>
            ${options.technical ? `<p class="norn-life-technical"><code>${escape(finding.code)}</code> ${escape(finding.message)}</p>` : ""}
            <div data-region="stepper" hidden>
              <ol class="norn-life-steps">
                <li data-step="0">${escape(w.opened)}</li>
                ${finding.witness.steps.map((step, at) => `<li data-step="${at + 1}">${step.moves.map((move) => `<span class="norn-life-side">${escape(move.side === "ui" ? w.page : w.object)}</span> ${escape(label(move.label))}`).join(" · ")}</li>`).join("")}
              </ol>
              <div class="norn-life-step-controls">
                <button type="button" data-move="-1" aria-label="${escape(w.prev)}">‹</button>
                <span data-region="position" aria-live="polite"></span>
                <button type="button" data-move="1" aria-label="${escape(w.next)}">›</button>
              </div>
            </div>
          </article>`).join("")}
      </section>`}
    </section>`
  const q = <T extends Element = HTMLElement>(selector: string) => root.querySelector<T>(selector)
  let picked: number | null = options.open && findings[options.open.finding] ? options.open.finding : null
  let taken = picked === null ? 0 : Math.max(0, Math.min(findings[picked]!.witness.steps.length, options.open?.step ?? 0))

  const drawArrows = () => {
    const board = q('[data-region="board"]')
    const svg = q<SVGSVGElement>(".norn-life-arrows")
    if (!board || !svg) return
    const origin = board.getBoundingClientRect()
    const box = (state: string) => q(`[data-state="${CSS.escape(state)}"]`)?.getBoundingClientRect()
    const active = picked === null ? new Set<string>() : stepState(lifecycle, findings[picked]!, taken).transitions
    // Cards in the board's own coordinates; the routes are `routeArrow`'s.
    const local = (rect: DOMRect): Box => ({ left: rect.left - origin.left + board.scrollLeft, right: rect.right - origin.left + board.scrollLeft, top: rect.top - origin.top, bottom: rect.bottom - origin.top })
    const cards = new Map(lifecycle.states.flatMap((state) => { const rect = box(state); return rect ? [[state, local(rect)] as const] : [] }))
    const floor = Math.max(0, ...[...cards.values()].map((card) => card.bottom))
    const parts: string[] = []
    let lane = 0
    for (const transition of lifecycle.transitions) {
      const to = cards.get(transition.to)
      const toColumn = column.get(transition.to)
      if (!to || toColumn === undefined) continue
      for (const from of transition.from) {
        const a = cards.get(from)
        const fromColumn = column.get(from)
        if (!a || fromColumn === undefined) continue
        const hop = toColumn - fromColumn
        const route = routeArrow(a, to, hop, locale === "ar", floor, hop < -1 ? lane++ : 0)
        const on = active.has(transition.id)
        parts.push(`<path d="${pathData(route)}" marker-end="url(#norn-life-head${on ? "-on" : ""})"${on ? " data-active" : ""}/><text x="${route.label[0]}" y="${route.label[1]}" text-anchor="middle"${on ? " data-active" : ""}>${escape(label(transition.id))}</text>`)
      }
    }
    svg.setAttribute("width", String(board.scrollWidth))
    svg.setAttribute("height", String(board.scrollHeight))
    svg.querySelectorAll(":scope > path, :scope > text").forEach((node) => node.remove())
    svg.insertAdjacentHTML("beforeend", parts.join(""))
  }

  const paintSteps = () => {
    const current = picked === null ? null : stepState(lifecycle, findings[picked]!, taken).state
    for (const card of root.querySelectorAll<HTMLElement>("[data-state]")) card.toggleAttribute("data-current", card.dataset.state === current)
    for (const article of root.querySelectorAll<HTMLElement>("[data-finding]")) {
      const index = Number(article.dataset.finding)
      const open = index === picked
      article.toggleAttribute("aria-current", open)
      const stepper = article.querySelector<HTMLElement>('[data-region="stepper"]')
      if (stepper) stepper.hidden = !open
      if (!open) continue
      const total = findings[index]!.witness.steps.length
      for (const item of article.querySelectorAll<HTMLElement>("[data-step]")) item.toggleAttribute("aria-current", Number(item.dataset.step) === taken)
      const position = article.querySelector('[data-region="position"]')
      if (position) position.textContent = w.step(number.format(taken), number.format(total))
      article.querySelector<HTMLButtonElement>('[data-move="-1"]')!.disabled = taken === 0
      article.querySelector<HTMLButtonElement>('[data-move="1"]')!.disabled = taken === total
    }
    drawArrows()
  }

  const paintCounts = (counts: Counts, rows: { total: number; lowerBound: boolean } | null, dash = false) => {
    for (const card of root.querySelectorAll<HTMLElement>("[data-state]")) {
      const cell = card.querySelector<HTMLElement>("[data-count]")
      const count = counts.states.get(card.dataset.state ?? "")
      if (!cell || !count) continue
      cell.toggleAttribute("data-pending", count.kind === "pending")
      cell.textContent = count.kind === "pending" ? (counts.refused || dash ? w.none : w.pending) : count.lowerBound ? w.atLeast(number.format(count.count)) : number.format(count.count)
    }
    const total = q('[data-region="total"]')
    if (total) total.textContent = rows === null ? "" : w.rows(rows.lowerBound ? w.atLeast(number.format(rows.total)) : number.format(rows.total))
    const refused = q('[data-region="refused"]')
    if (refused) {
      refused.textContent = counts.refused ?? ""
      refused.hidden = counts.refused === null
    }
    const notes = q('[data-region="notes"]')
    if (notes) {
      notes.textContent = counts.notes.join(" · ")
      notes.hidden = counts.notes.length === 0
    }
  }

  root.addEventListener("click", (event) => {
    const target = event.target as Element | null
    const pick = target?.closest<HTMLElement>("[data-pick]")
    if (pick) {
      const index = Number(pick.dataset.pick)
      picked = picked === index ? null : index
      taken = 0
      paintSteps()
      return
    }
    const move = target?.closest<HTMLButtonElement>("[data-move]")
    if (move && picked !== null && !move.disabled) {
      taken = Math.max(0, Math.min(findings[picked]!.witness.steps.length, taken + Number(move.dataset.move)))
      paintSteps()
    }
  })

  paintCounts(countsFrom(lifecycle, null, locale), null)
  paintSteps()
  const resize = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => drawArrows())
  const board = q('[data-region="board"]')
  if (resize && board) resize.observe(board)

  // The machine decides every load state; the view only draws the one it is in.
  const page = options.document.payload.ux.pages[0]
  if (!page) throw new Error("the lifecycle view's document has no page")
  let last: LiveAnswer | null = null
  const paintState = (state: string) => {
    q(".norn-life")?.setAttribute("data-machine-state", state)
    const unavailable = q('[data-region="unavailable"]')
    if (unavailable) unavailable.hidden = state !== "unavailable"
    const text = q('[data-region="unavailable-text"]')
    // A bare 404 or 503 (no coded refusal) is a route this cell does not serve, as everywhere in Studio.
    const coded = typeof (last?.body as { code?: unknown } | undefined)?.code === "string"
    if (text) text.textContent = state !== "unavailable" ? "" : last !== null && (last.status === 404 || last.status === 503) && !coded ? `${w.notServed} ${w.again}` : `${w.failed} ${w.again}`
    if (state === "showing" || state === "refused") {
      const counts = countsFrom(lifecycle, last, locale)
      const body = last && last.status < 400 ? (last.body as Partial<FacetsAnswer>) : null
      paintCounts(counts, body && typeof body.rows === "number" ? { total: body.rows, lowerBound: body.approximate === true } : null)
    } else if (state === "unavailable") paintCounts(countsFrom(lifecycle, null, locale), null, true)
    drawArrows()
  }
  const driver: Driver = createDriver({
    machine: page.machine,
    // The view's machine has no guards or assignments, so no expression engine is needed.
    evaluate: () => {
      throw new Error("the lifecycle view's machine evaluates no expressions")
    },
    transport: {
      async call() {
        last = await options.read()
        return classifyResponse(last.status, last.body)
      },
    },
    onStep: (step) => paintState(step.snapshot.stateId),
  })
  paintState(driver.snapshot().stateId)
  driver.dispatch({ kind: "page_opened" })
  root.addEventListener("click", (event) => {
    if ((event.target as Element | null)?.closest('[data-node="retry-button"]')) driver.dispatch({ kind: "user", eventId: "retry" })
  })
  let seen: string | null = null
  const stop = options.changes((answer) => {
    // The first answer marks where the view already is; a later one is a commit that moved it.
    const digest = JSON.stringify(answer)
    if (seen !== null && digest !== seen) driver.dispatch({ kind: "subscription", bindingId: "subject" })
    seen = digest
  })
  return () => {
    stop()
    driver.dispose()
    resize?.disconnect()
  }
}

/** `<norn-lifecycle-view>`: set `options` to mount; removing the element ends its live read. */
export interface NornLifecycleView extends HTMLElement {
  options: LifecycleViewOptions
}

/** Register `<norn-lifecycle-view>` once (made here, not at import, so non-DOM imports load). */
export function defineLifecycleView(name = "norn-lifecycle-view"): void {
  if (customElements.get(name)) return
  class Element extends HTMLElement implements NornLifecycleView {
    #options: LifecycleViewOptions | null = null
    #dispose: (() => void) | null = null
    set options(options: LifecycleViewOptions) {
      this.#options = options
      if (this.isConnected) this.#mount()
    }
    get options(): LifecycleViewOptions {
      if (this.#options === null) throw new Error("the lifecycle view has no options yet")
      return this.#options
    }
    connectedCallback() {
      if (this.#options && !this.#dispose) this.#mount()
    }
    disconnectedCallback() {
      this.#dispose?.()
      this.#dispose = null
    }
    #mount() {
      this.#dispose?.()
      this.#dispose = this.#options ? mountLifecycleView(this, this.#options) : null
    }
  }
  customElements.define(name, Element)
}
