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
  readonly witness: { readonly steps: readonly { readonly edge: number; readonly moves: readonly WitnessMove[] }[] }
}

export type Locale = "en" | "ar"

/** One live answer: what the cell's GET returned, as the subscriber, now. */
export interface LiveAnswer {
  readonly status: number
  readonly body: unknown
}

export interface LifecycleViewOptions {
  readonly lifecycle: LifecycleDoc
  /** The object type's title, as the host names it. */
  readonly title: string
  /** Subscribe to the type's facets on the lifecycle property; returns the unsubscribe. */
  readonly counts: (listener: (answer: LiveAnswer) => void) => () => void
  /** Findings with a witness for this lifecycle (`widget.lifecycle.*`). */
  readonly findings?: readonly LifecycleFinding[]
  readonly locale?: Locale
  /** A state's or transition's display name; defaults to its id with spaces. */
  readonly label?: (id: string) => string
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
  },
  ar: {
    rows: (n: string) => `${n} سجل`, start: "البداية", end: "نهاية", rests: "يمكن أن يستقر هنا", none: "—",
    pending: "جارٍ العد…", refused: "لا يمكنك رؤية أعداد هذه السجلات.", unavailable: "لم تُجب الخلية.",
    withheld: (n: number) => `${n} محجوبة عنك`, quarantined: (n: number) => `${n} في الحجر`, unset: (n: number) => `${n} بلا حالة`,
    other: (n: number) => `${n} في حالات لا يعرضها هذا العرض`, atLeast: (n: string) => `${n} على الأقل`,
    findings: "أمثلة مضادة وجدها المدقق", step: (i: string, n: string) => `الخطوة ${i} من ${n}`, prev: "الخطوة السابقة", next: "الخطوة التالية",
    page: "الصفحة", object: "الكائن", opened: "حيث تُفتح الصفحة",
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
  const label = options.label ?? ((id: string) => id.replaceAll("_", " "))
  const number = new Intl.NumberFormat(locale === "ar" ? "ar-EG" : "en")
  const findings = (options.findings ?? []).filter((finding) => finding.witness.steps.length > 0)
  adoptLifecycleStyles(root.ownerDocument)
  const endings = new Map((lifecycle.endings ?? []).map((ending) => [ending.state, ending.outcome]))
  const role = (state: string) =>
    [lifecycle.initial.includes(state) ? w.start : "", lifecycle.terminal.includes(state) ? w.end : endings.get(state) === "resting" ? w.rests : ""].filter(Boolean).join(" · ")
  const columns = layout(lifecycle)
  root.innerHTML = `
    <section class="norn-life" dir="${locale === "ar" ? "rtl" : "ltr"}" lang="${locale}">
      <header class="norn-life-head">
        <h2 class="norn-life-title">${escape(options.title)} · ${escape(label(lifecycle.property))}</h2>
        <p class="norn-life-meta" data-region="total" aria-live="polite"></p>
      </header>
      <p class="norn-life-refused" data-region="refused" hidden></p>
      <div class="norn-life-board" data-region="board">
        <svg class="norn-life-arrows" aria-hidden="true"><defs><marker id="norn-life-head" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0 0 8 4 0 8z"/></marker></defs></svg>
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
            <button type="button" data-pick="${index}">${escape(finding.message)}</button>
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
    const rtl = locale === "ar"
    const parts: string[] = []
    for (const transition of lifecycle.transitions) {
      const to = box(transition.to)
      if (!to) continue
      for (const from of transition.from) {
        const a = box(from)
        if (!a) continue
        const forward = rtl ? to.right <= a.left : to.left >= a.right
        const x1 = (forward ? (rtl ? a.left : a.right) : a.left + a.width / 2) - origin.left + board.scrollLeft
        const y1 = (forward ? a.top + a.height / 2 : a.bottom) - origin.top
        const x2 = (forward ? (rtl ? to.right : to.left) : to.left + to.width / 2) - origin.left + board.scrollLeft
        const y2 = (forward ? to.top + to.height / 2 : to.bottom) - origin.top
        const bend = forward ? (x2 - x1) / 2 : 36
        const d = forward ? `M${x1} ${y1} C${x1 + bend} ${y1} ${x2 - bend} ${y2} ${x2} ${y2}` : `M${x1} ${y1} C${x1} ${y1 + bend} ${x2} ${y2 + bend} ${x2} ${y2}`
        const on = active.has(transition.id) ? " data-active" : ""
        parts.push(`<path d="${d}" marker-end="url(#norn-life-head)"${on}/><text x="${(x1 + x2) / 2}" y="${(y1 + y2) / 2 - 4}" text-anchor="middle"${on}>${escape(label(transition.id))}</text>`)
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

  const paintCounts = (counts: Counts, rows: { total: number; lowerBound: boolean } | null) => {
    for (const card of root.querySelectorAll<HTMLElement>("[data-state]")) {
      const cell = card.querySelector<HTMLElement>("[data-count]")
      const count = counts.states.get(card.dataset.state ?? "")
      if (!cell || !count) continue
      cell.toggleAttribute("data-pending", count.kind === "pending")
      cell.textContent = count.kind === "pending" ? (counts.refused ? w.none : w.pending) : count.lowerBound ? w.atLeast(number.format(count.count)) : number.format(count.count)
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
  const stop = options.counts((answer) => {
    const counts = countsFrom(lifecycle, answer, locale)
    const body = answer.status < 400 ? (answer.body as Partial<FacetsAnswer>) : null
    paintCounts(counts, body && typeof body.rows === "number" ? { total: body.rows, lowerBound: body.approximate === true } : null)
    drawArrows()
  })
  return () => {
    stop()
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
