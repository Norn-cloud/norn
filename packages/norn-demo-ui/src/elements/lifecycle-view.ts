// `<norn-lifecycle-view>` (UI-LIFECYCLE-2): an object type's lifecycle as the declaration draws
// it, each state with the cell's live count, and the checker's counterexamples stepped through.
//
// Counts are the server's: one live read of the type's facets on the lifecycle property
// (`GET /v1/objects/{type}:facets?properties=<property>`, HOST-READ-7). When `otherRows === 0`,
// a declared state the answer does not list has no rows (0); when `otherRows > 0`, an omitted
// state may belong to those unlisted values and is marked unlisted rather than zero. Past the
// cell's scan bound (`lowerBound: true`), every count reads honestly as a lower bound ("at least
// N"). Rows the reader may not see are said, never counted. The view computes nothing a count
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

/** The cell's facets answer (HOST-READ-7, `object_facets.rs`), as far as the view reads it. */
export interface FacetsAnswer {
  readonly rows: number
  readonly withheldRows: number
  readonly quarantinedRows: number
  readonly lowerBound: boolean
  readonly scanLimit?: number
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
  /** The counterexample's witness steps, present only when attributed to a specific widget page machine. */
  readonly witness?: { readonly steps: readonly { readonly edge: number; readonly moves: readonly WitnessMove[] }[] } | null
  /** True when a widget on this lifecycle produced the finding without a canonical page pointer, so no witness is attached. */
  readonly unattributed?: boolean
}

export type Locale = "en" | "ar"

/** One live answer: what the cell's GET returned, as the subscriber, now. */
export interface LiveAnswer {
  readonly status: number
  readonly body: unknown
  /** Optional server snapshot digest (UI-LIVE-1) used to detect changes even when bodies are omitted. */
  readonly digest?: string
}

export interface LiveSubscriptionRefusal {
  readonly code: string
  readonly message: string
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
  /** Read the type's facets on the lifecycle property once (the cell's GET, as the reader). Omit when no live cell is connected. */
  readonly read?: (signal?: AbortSignal) => Promise<LiveAnswer>
  /** Subscribe to changes of that answer (the live read); each new answer is read again. Returns the unsubscribe. */
  readonly changes?: (
    listener: (answer: LiveAnswer) => void,
    onRefused?: (refusal: LiveSubscriptionRefusal) => void,
  ) => () => void
  /** Findings with a witness (or unattributed findings) for this lifecycle (`widget.lifecycle.*`). */
  readonly findings?: readonly LifecycleFinding[]
  readonly locale?: Locale
  /** A state's or transition's display name; defaults to its id in words, sentence case. */
  readonly label?: (id: string) => string
  /** Technical view: each finding also shows its code and the checker's own words. */
  readonly technical?: boolean
  /** Open one finding's stepper at a step (a deep link, or a design specimen). */
  readonly open?: { readonly finding: number; readonly step: number }
  /** Hide the inner title heading when hosted inside a titled workbench panel. */
  readonly hideTitle?: boolean
}

const WORDS = {
  en: {
    rows: (n: string, count: number, lowerBound: boolean) => lowerBound ? `at least ${n} records` : `${n} ${count === 1 ? "record" : "records"}`,
    start: "start", end: "ending", rests: "can rest here", none: "—",
    pending: "Counting…", refused: "You can't see these records' counts.", unavailable: "The cell did not answer.",
    noCell: "No live cell is connected; showing the declared lifecycle.",
    liveOff: "Live updates are off; counts show the last read.",
    refresh: "Refresh",
    unlisted: "Not listed",
    unlistedHint: "Omitted by the cell's value limit; may have records in unlisted values",
    withheld: (n: string) => `${n} withheld from you`, quarantined: (n: string) => `${n} held in quarantine`, unset: (n: string) => `${n} without a state`,
    other: (n: string, hasUnlisted: boolean) => hasUnlisted ? `${n} in unlisted values (omitted states are not counted as 0)` : `${n} in values outside this lifecycle`,
    scanBound: (limit: string) => `Scan limit reached (${limit} rows); counts are lower bounds`,
    atLeast: (n: string) => `at least ${n}`,
    lowerBoundHint: "Lower bound; the cell's scan limit was reached",
    findings: "Counterexamples the checker found", step: (i: string, n: string) => `Step ${i} of ${n}`, prev: "Previous step", next: "Next step",
    page: "page", object: "object", opened: "Where the page opens",
    unattributed: "Unattributed finding (missing or invalid page pointer; no witness attached)",
    stuck: (page: string, type: string, state: string) => `The page can show “${page}” while the ${type} is still “${state}”, and nothing can finish it.`,
    unknown: (page: string, type: string, state: string) => `The page can show “${page}” while the ${type} is “${state}”; whether it can finish depends on the rules.`,
    unexpected: (page: string, type: string, state: string) => `The page shows “${page}” while the ${type} is “${state}”, which it does not expect.`,
    notServed: "This cell does not serve lifecycle counts yet.", failed: "The cell did not answer.", retry: "Try again", again: "Asking again shortly.",
  },
  ar: {
    rows: (n: string, _count: number, lowerBound: boolean) => lowerBound ? `${n} سجل على الأقل` : `${n} سجل`,
    start: "البداية", end: "نهاية", rests: "يمكن أن يستقر هنا", none: "—",
    pending: "جارٍ العد…", refused: "لا يمكنك رؤية أعداد هذه السجلات.", unavailable: "لم تُجب الخلية.",
    noCell: "لا توجد خلية متصلة؛ تُعرض دورة الحياة المُعلنة.",
    liveOff: "التحديثات المباشرة متوقفة؛ الأعداد تعكس آخر قراءة.",
    refresh: "تحديث",
    unlisted: "غير مدرجة",
    unlistedHint: "حُذفت بسبب حد قيم الخلية؛ قد توجد سجلات في القيم غير المدرجة",
    withheld: (n: string) => `${n} محجوبة عنك`, quarantined: (n: string) => `${n} في الحجر`, unset: (n: string) => `${n} بلا حالة`,
    other: (n: string, hasUnlisted: boolean) => hasUnlisted ? `${n} في قيم غير مدرجة (الحالات غير المدرجة لا تُعدّ صفراً)` : `${n} في قيم خارج دورة الحياة هذه`,
    scanBound: (limit: string) => `تم بلوغ حد الفحص (${limit} سجل)؛ الأعداد حدود دنيا`,
    atLeast: (n: string) => `${n} على الأقل`,
    lowerBoundHint: "حد أدنى؛ تم بلوغ حد الفحص في الخلية",
    findings: "أمثلة مضادة وجدها المدقق", step: (i: string, n: string) => `الخطوة ${i} من ${n}`, prev: "الخطوة السابقة", next: "الخطوة التالية",
    page: "الصفحة", object: "الكائن", opened: "حيث تُفتح الصفحة",
    unattributed: "ملاحظة غير منسوبة (مؤشر الصفحة مفقود أو غير صالح؛ لم يُرفق شاهد)",
    stuck: (page: string, type: string, state: string) => `قد تعرض الصفحة «${page}» بينما ${type} ما زال «${state}»، ولا شيء يستطيع إنهاءه.`,
    unknown: (page: string, type: string, state: string) => `قد تعرض الصفحة «${page}» بينما ${type} «${state}»؛ وإمكان إنهائه يتوقف على القواعد.`,
    unexpected: (page: string, type: string, state: string) => `تعرض الصفحة «${page}» بينما ${type} «${state}»، وهذا ما لا تتوقعه.`,
    notServed: "هذه الخلية لا تقدّم أعداد دورة الحياة بعد.", failed: "لم تُجب الخلية.", retry: "حاول مرة أخرى", again: "سنسأل مرة أخرى بعد قليل.",
  },
} as const

/** Display role of a declared state, shared between the type-wide graph and record stepper. */
export function lifecycleStateRole(lifecycle: Pick<LifecycleDoc, "initial" | "terminal" | "endings">, state: string, locale: Locale = "en"): string {
  const w = WORDS[locale]
  const ending = lifecycle.endings?.find((candidate) => candidate.state === state)?.outcome
  return [
    lifecycle.initial.includes(state) ? w.start : "",
    lifecycle.terminal.includes(state) ? w.end : ending === "resting" ? w.rests : "",
  ].filter(Boolean).join(" · ")
}

/** Whether `value` has the facets document's shape (`object_facets.rs`). */
export function isFacetsAnswer(value: unknown): value is FacetsAnswer {
  const body = value as Partial<FacetsAnswer> | null | undefined
  return typeof body?.rows === "number"
    && typeof body.lowerBound === "boolean"
    && Array.isArray(body.facets)
    && body.facets.every((facet) =>
      typeof facet?.property === "string"
      && Array.isArray(facet.values)
      && facet.values.every((entry: unknown) =>
        typeof entry === "object" && entry !== null
        && "value" in entry && (typeof entry.value === "string" || typeof entry.value === "boolean")
        && "count" in entry && typeof entry.count === "number" && Number.isFinite(entry.count))
      && typeof facet.otherRows === "number"
      && typeof facet.nulls === "number")
}

/** Columns by shortest distance from an initial state; peers keep declared order; states no run reaches go last. */
export function layout(lifecycle: Pick<LifecycleDoc, "states" | "initial" | "transitions">): string[][] {
  const states = [...new Set(lifecycle.states)]
  const known = new Set(states)
  const depth = new Map<string, number>()
  const queue = [...new Set(lifecycle.initial.filter((state) => known.has(state)))]
  for (const state of queue) depth.set(state, 0)
  for (let index = 0; index < queue.length; index += 1) {
    const state = queue[index]!
    for (const transition of lifecycle.transitions) {
      if (!transition.from.includes(state) || depth.has(transition.to) || !known.has(transition.to)) continue
      depth.set(transition.to, depth.get(state)! + 1)
      queue.push(transition.to)
    }
  }
  const last = Math.max(-1, ...depth.values()) + 1
  const columns: string[][] = []
  for (const state of states) {
    const column = depth.get(state) ?? last
    ;(columns[column] ??= []).push(state)
  }
  return columns.filter((column) => column !== undefined)
}

/** Canonical flat state order shared by the type-wide graph and the record-level stepper. */
export function orderLifecycleStates(lifecycle: Pick<LifecycleDoc, "states" | "initial" | "transitions">): string[] {
  return layout(lifecycle).flat()
}

/** A card's edges, in the stage's own coordinates. */
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

export type StateCount =
  | { readonly kind: "count"; readonly count: number; readonly lowerBound: boolean }
  | { readonly kind: "unlisted" }
  | { readonly kind: "pending" }

export interface Counts {
  readonly states: ReadonlyMap<string, StateCount>
  readonly notes: readonly string[]
  readonly refused: string | null
}

/**
 * The per-state counts of one live answer: the server's numbers, nothing derived except that an
 * omitted declared state has 0 rows ONLY when `facet.otherRows === 0`. When `facet.otherRows > 0`,
 * values were truncated by the cell's facet limit and an omitted declared state is marked `unlisted`.
 */
export function countsFrom(lifecycle: LifecycleDoc, answer: LiveAnswer | null, locale: Locale = "en"): Counts {
  const w = WORDS[locale]
  const number = new Intl.NumberFormat(locale === "ar" ? "ar-EG" : "en")
  const pending = new Map(lifecycle.states.map((state) => [state, { kind: "pending" } as StateCount]))
  if (answer === null) return { states: pending, notes: [], refused: null }
  if (answer.status === 401 || answer.status === 403 || answer.status === 404) return { states: pending, notes: [], refused: w.refused }
  if (answer.status >= 400) {
    const message = typeof (answer.body as { message?: unknown } | undefined)?.message === "string"
      ? (answer.body as { message: string }).message
      : w.unavailable
    return { states: pending, notes: [], refused: message }
  }
  if (!isFacetsAnswer(answer.body)) return { states: pending, notes: [], refused: w.unavailable }
  const body = answer.body
  const facet = body.facets.find((candidate) => candidate.property === lifecycle.property)
  if (!facet) return { states: pending, notes: [], refused: w.unavailable }
  const lowerBound = body.lowerBound === true
  const hasOther = facet.otherRows > 0
  const states = new Map<string, StateCount>()
  let anyUnlisted = false
  for (const state of lifecycle.states) {
    const found = facet.values.find((entry) => entry.value === state)
    if (found !== undefined) {
      states.set(state, { kind: "count", count: found.count, lowerBound })
    } else if (hasOther) {
      anyUnlisted = true
      states.set(state, { kind: "unlisted" })
    } else {
      states.set(state, { kind: "count", count: 0, lowerBound })
    }
  }
  const fmt = (value: number) => lowerBound ? w.atLeast(number.format(value)) : number.format(value)
  const notes = [
    lowerBound && typeof body.scanLimit === "number" && body.scanLimit > 0 ? w.scanBound(number.format(body.scanLimit)) : "",
    (body.withheldRows ?? 0) > 0 ? w.withheld(fmt(body.withheldRows)) : "",
    (body.quarantinedRows ?? 0) > 0 ? w.quarantined(fmt(body.quarantinedRows)) : "",
    facet.nulls > 0 ? w.unset(fmt(facet.nulls)) : "",
    facet.otherRows > 0 ? w.other(fmt(facet.otherRows), anyUnlisted) : "",
  ].filter(Boolean)
  return { states, notes, refused: null }
}

/** After `taken` steps of a witness: the lifecycle transitions taken so far and the state the object is in. */
export function stepState(lifecycle: LifecycleDoc, finding: LifecycleFinding, taken: number): { readonly transitions: ReadonlySet<string>; readonly state: string | null } {
  const transitions = new Set<string>()
  let state: string | null = null
  if (!finding.witness || !Array.isArray(finding.witness.steps)) return { transitions, state }
  for (const step of finding.witness.steps.slice(0, taken)) {
    for (const move of step.moves) {
      if (move.side !== "backend") continue
      transitions.add(move.label)
      state = lifecycle.transitions.find((transition) => transition.id === move.label)?.to ?? state
    }
  }
  return { transitions, state }
}

const SVG_NS = "http://www.w3.org/2000/svg"

const isRouteNotServed = (answer: LiveAnswer | null): boolean => {
  if (answer === null) return false
  const code = (answer.body as { code?: unknown } | undefined)?.code
  return (answer.status === 404 || answer.status === 503) && (typeof code !== "string" || code === "route.not_found")
}

function sameJson(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true
  if (typeof a !== typeof b || a === null || b === null || typeof a !== "object") return false
  if (Array.isArray(a) !== Array.isArray(b)) return false
  if (Array.isArray(a)) {
    const other = b as readonly unknown[]
    return a.length === other.length && a.every((item, index) => sameJson(item, other[index]))
  }
  const left = a as Record<string, unknown>
  const right = b as Record<string, unknown>
  const keys = Object.keys(left)
  return keys.length === Object.keys(right).length && keys.every((key) => Object.hasOwn(right, key) && sameJson(left[key], right[key]))
}

function answersMatch(a: LiveAnswer, b: LiveAnswer): boolean {
  if (a.status !== b.status) return false
  if (a.body !== undefined && b.body !== undefined) return sameJson(a.body, b.body)
  if (a.digest !== undefined && b.digest !== undefined) return a.digest === b.digest
  return a.body === b.body
}

export interface LifecycleReadControllerOptions {
  readonly machine: Machine
  readonly read: (signal?: AbortSignal) => Promise<LiveAnswer>
  readonly changes?: LifecycleViewOptions["changes"]
  readonly onState: (state: string, last: LiveAnswer | null) => void
  readonly onLiveRefusal?: (refusal: LiveSubscriptionRefusal | null) => void
}

export interface LifecycleReadController {
  readonly refresh: () => void
  readonly retry: () => void
  readonly state: () => string
  readonly lastAnswer: () => LiveAnswer | null
  readonly liveRefusal: () => LiveSubscriptionRefusal | null
  readonly dispose: () => void
}

/**
 * Drives the lifecycle view's admitted machine over `read` (GET) and `changes` (live socket).
 * Reconciles the first socket snapshot, including after a refusal, with the latest GET. If the
 * two answers differ, one fresh GET runs after both observations so the displayed counts converge
 * to the cell's newest snapshot. Also surfaces live subscription
 * refusals (e.g. `live.not_subscribable` past the 512-byte path limit) so the view can offer manual
 * refresh instead of silently going stale.
 */
export function createLifecycleReadController(options: LifecycleReadControllerOptions): LifecycleReadController {
  let last: LiveAnswer | null = null
  let pendingRefresh = false
  let pendingFirstSnapshot: LiveAnswer | null = null
  let liveRefusal: LiveSubscriptionRefusal | null = null
  let disposed = false
  let seen: string | null = null

  const driver: Driver = createDriver({
    machine: options.machine,
    // The view's machine has no guards or assignments, so no expression engine is needed.
    evaluate: () => {
      throw new Error("the lifecycle view's machine evaluates no expressions")
    },
    transport: {
      async call(_call, _input, callOptions) {
        const readOnce = async () => {
          last = null
          last = await options.read(callOptions.signal)
          if (isRouteNotServed(last)) return { outcome: "network_failure" as const, payload: {} }
          const classified = classifyResponse(last.status, last.body)
          if (classified.outcome === "accepted" && !isFacetsAnswer(last.body)) {
            return { outcome: "network_failure" as const, payload: {} }
          }
          return classified
        }
        const readWithRetry = async () => {
          let classified = await readOnce()
          // A concurrent commit (409 object.snapshot_moved) is retryable once before falling back to unavailable.
          if (classified.outcome === "answer_moved" && !callOptions.signal.aborted) {
            classified = await readOnce()
          }
          if (classified.outcome === "answer_moved") return { outcome: "network_failure" as const, payload: {} }
          return classified
        }
        let classified = await readWithRetry()
        // If the first socket snapshot arrived while this GET was in flight and disagrees with the GET,
        // re-read once now (linearized after both observations) so older GET data never wins over a newer commit.
        if (pendingFirstSnapshot !== null) {
          const first = pendingFirstSnapshot
          pendingFirstSnapshot = null
          if (!callOptions.signal.aborted && (last === null || !answersMatch(last, first))) {
            classified = await readWithRetry()
          }
        }
        return classified
      },
    },
    onStep: (step) => {
      if (disposed) return
      const state = step.snapshot.stateId
      if (pendingFirstSnapshot !== null && state !== "reading" && state !== "opening") {
        const first = pendingFirstSnapshot
        pendingFirstSnapshot = null
        if (last === null || !answersMatch(last, first)) pendingRefresh = true
      }
      options.onState(state, last)
      if (pendingRefresh && state !== "reading" && state !== "opening") {
        pendingRefresh = false
        queueMicrotask(() => {
          if (disposed) return
          const current = driver.snapshot().stateId
          if (current === "unavailable") driver.dispatch({ kind: "user", eventId: "retry" })
          else if (current !== "reading" && current !== "opening") driver.dispatch({ kind: "subscription", bindingId: "subject" })
        })
      }
    },
  })

  const triggerRefresh = () => {
    if (disposed) return
    const state = driver.snapshot().stateId
    if (state === "reading" || state === "opening") {
      pendingRefresh = true
    } else if (state === "unavailable") {
      driver.dispatch({ kind: "user", eventId: "retry" })
    } else {
      driver.dispatch({ kind: "subscription", bindingId: "subject" })
    }
  }

  const setLiveRefusal = (refusal: LiveSubscriptionRefusal | null) => {
    if (liveRefusal?.code === refusal?.code && liveRefusal?.message === refusal?.message) return
    liveRefusal = refusal
    options.onLiveRefusal?.(liveRefusal)
  }

  options.onState(driver.snapshot().stateId, last)
  driver.dispatch({ kind: "page_opened" })

  const stop = options.changes?.(
    (answer) => {
      if (disposed) return
      setLiveRefusal(null)
      const digest = answer.digest ?? JSON.stringify({ status: answer.status, body: answer.body })
      if (seen === null) {
        seen = digest
        const state = driver.snapshot().stateId
        if (state === "reading" || state === "opening") {
          pendingFirstSnapshot = answer
        } else if (last === null || !answersMatch(last, answer)) {
          triggerRefresh()
        }
        return
      }
      if (digest !== seen) {
        seen = digest
        pendingFirstSnapshot = null
        triggerRefresh()
      }
    },
    (refusal) => {
      if (disposed) return
      // A manual refresh during a refusal can move the displayed counts beyond the last digest.
      seen = null
      setLiveRefusal(refusal)
    },
  )

  return {
    refresh: triggerRefresh,
    retry: () => {
      if (!disposed) driver.dispatch({ kind: "user", eventId: "retry" })
    },
    state: () => driver.snapshot().stateId,
    lastAnswer: () => last,
    liveRefusal: () => liveRefusal,
    dispose: () => {
      disposed = true
      stop?.()
      driver.dispose()
    },
  }
}

const hasWitness = (finding: LifecycleFinding): finding is LifecycleFinding & { readonly witness: { readonly steps: readonly { readonly edge: number; readonly moves: readonly WitnessMove[] }[] } } =>
  finding.witness !== undefined && finding.witness !== null && Array.isArray(finding.witness.steps)

/** Mount the view into `root`. Returns the disposer (it ends the live read). */
export function mountLifecycleView(root: HTMLElement, options: LifecycleViewOptions): () => void {
  const { lifecycle } = options
  const locale = options.locale ?? "en"
  const rtl = locale === "ar"
  const w = WORDS[locale]
  // The definition's ids in words, sentence case ("on_accepted" reads "On accepted").
  const label = options.label ?? ((id: string) => { const words = id.replaceAll(/[_-]/g, " "); return words.charAt(0).toUpperCase() + words.slice(1) })
  const number = new Intl.NumberFormat(rtl ? "ar-EG" : "en")
  const findings = (options.findings ?? []).filter((finding) => hasWitness(finding) || finding.unattributed === true)
  const doc = root.ownerDocument
  adoptLifecycleStyles(doc)
  const role = (state: string) => lifecycleStateRole(lifecycle, state, locale)
  /** A finding in the reader's words: the page state and the object's state at the end of its run. */
  const headline = (finding: LifecycleFinding) => {
    if (!hasWitness(finding)) return finding.message
    const object = stepState(lifecycle, finding, finding.witness.steps.length).state
    if (finding.pageState === undefined || object === null) return finding.message
    const type = locale === "en" ? options.title.toLowerCase() : options.title
    const words = finding.code.endsWith(".stuck") ? w.stuck : finding.code.endsWith(".completion_unknown") ? w.unknown : finding.code.endsWith(".assumption_violated") ? w.unexpected : null
    return words === null ? finding.message : words(label(finding.pageState), type, label(object))
  }
  const columns = layout(lifecycle)
  const column = new Map(columns.flatMap((states, index) => states.map((state) => [state, index] as const)))
  // An arrow more than one column back runs under the cards; the stage keeps room for each.
  const lanes = lifecycle.transitions.reduce((count, transition) =>
    count + transition.from.filter((from) => (column.get(transition.to) ?? 0) - (column.get(from) ?? 0) < -1).length, 0)
  const prevGlyph = rtl ? "›" : "‹"
  const nextGlyph = rtl ? "‹" : "›"

  const el = <K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Record<string, string> = {}, text?: string) => {
    const node = doc.createElement(tag)
    for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, value)
    if (text !== undefined) node.textContent = text
    return node
  }
  const svgEl = <K extends keyof SVGElementTagNameMap>(tag: K, attrs: Record<string, string> = {}) => {
    const node = doc.createElementNS(SVG_NS, tag)
    for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, value)
    return node
  }

  const section = el("section", { class: "norn-life", dir: rtl ? "rtl" : "ltr", lang: locale })
  const header = el("header", { class: "norn-life-head" })
  if (!options.hideTitle) header.append(el("h2", { class: "norn-life-title" }, `${options.title} · ${label(lifecycle.property)}`))
  header.append(el("p", { class: "norn-life-meta", "data-region": "total", "aria-live": "polite" }))

  const refusedBanner = el("p", { class: "norn-life-refused", "data-region": "refused", role: "status" })
  refusedBanner.hidden = true

  const unavailableBanner = el("p", { class: "norn-life-refused", "data-region": "unavailable", role: "status" })
  unavailableBanner.hidden = true
  unavailableBanner.append(
    el("span", { "data-region": "unavailable-text" }),
    doc.createTextNode(" "),
    el("button", { type: "button", class: "norn-life-retry", "data-node": "retry-button" }, w.retry),
  )

  const liveOffBanner = el("p", { class: "norn-life-refused", "data-region": "live-off", role: "status" })
  liveOffBanner.hidden = true
  liveOffBanner.append(
    el("span", { "data-region": "live-off-text" }),
    doc.createTextNode(" "),
    el("button", { type: "button", class: "norn-life-retry", "data-node": "refresh-button" }, w.refresh),
  )

  const boardEl = el("div", { class: "norn-life-board", "data-region": "board" })
  boardEl.style.setProperty("--norn-life-lanes", String(lanes))
  const stageEl = el("div", { class: "norn-life-stage", "data-region": "stage" })
  const svg = svgEl("svg", { class: "norn-life-arrows", dir: rtl ? "rtl" : "ltr", "aria-hidden": "true" })
  const defs = svgEl("defs")
  for (const suffix of ["", "-on"]) {
    const marker = svgEl("marker", {
      id: `norn-life-head${suffix}`,
      viewBox: "0 0 8 8",
      refX: "7",
      refY: "4",
      markerUnits: "userSpaceOnUse",
      markerWidth: "8",
      markerHeight: "8",
      orient: "auto",
    })
    marker.append(svgEl("path", { d: "M0 0 8 4 0 8z" }))
    defs.append(marker)
  }
  svg.append(defs)
  stageEl.append(svg)

  for (const columnStates of columns) {
    const col = el("ol", { class: "norn-life-col" })
    for (const state of columnStates) {
      const card = el("li", { class: "norn-life-state", "data-state": state })
      if (lifecycle.terminal.includes(state)) card.setAttribute("data-ending", "")
      card.append(
        el("span", { class: "norn-life-name" }, label(state)),
        el("span", { class: "norn-life-count", "data-count": "", "aria-live": "polite" }),
        el("span", { class: "norn-life-role" }, role(state)),
      )
      col.append(card)
    }
    stageEl.append(col)
  }
  boardEl.append(stageEl)

  const notesEl = el("p", { class: "norn-life-foot", "data-region": "notes" })
  notesEl.hidden = true

  section.append(header, refusedBanner, unavailableBanner, liveOffBanner, boardEl, notesEl)

  if (findings.length > 0) {
    const findingsSection = el("section", { class: "norn-life-findings", "aria-label": w.findings })
    findingsSection.append(el("h3", {}, w.findings))
    findings.forEach((finding, index) => {
      if (!hasWitness(finding)) {
        const article = el("article", { class: "norn-life-finding", "data-finding-unattributed": "" })
        article.append(
          el("span", { class: "norn-life-name" }, w.unattributed),
          el("p", { class: "norn-life-meta" }, finding.message),
        )
        if (options.technical) {
          const technical = el("p", { class: "norn-life-technical" })
          technical.append(el("code", {}, finding.code))
          article.append(technical)
        }
        findingsSection.append(article)
        return
      }
      const article = el("article", { class: "norn-life-finding", "data-finding": String(index) })
      article.append(el("button", { type: "button", "data-pick": String(index) }, headline(finding)))
      if (options.technical) {
        const technical = el("p", { class: "norn-life-technical" })
        technical.append(el("code", {}, finding.code), doc.createTextNode(` ${finding.message}`))
        article.append(technical)
      }
      const stepper = el("div", { "data-region": "stepper" })
      stepper.hidden = true
      const stepsList = el("ol", { class: "norn-life-steps" })
      stepsList.append(el("li", { "data-step": "0" }, w.opened))
      finding.witness.steps.forEach((step, at) => {
        const item = el("li", { "data-step": String(at + 1) })
        step.moves.forEach((move, moveIndex) => {
          if (moveIndex > 0) item.append(doc.createTextNode(" · "))
          item.append(
            el("span", { class: "norn-life-side" }, move.side === "ui" ? w.page : w.object),
            doc.createTextNode(` ${label(move.label)}`),
          )
        })
        stepsList.append(item)
      })
      const controls = el("div", { class: "norn-life-step-controls" })
      controls.append(
        el("button", { type: "button", "data-move": "-1", "aria-label": w.prev }, prevGlyph),
        el("span", { "data-region": "position", "aria-live": "polite" }),
        el("button", { type: "button", "data-move": "1", "aria-label": w.next }, nextGlyph),
      )
      stepper.append(stepsList, controls)
      article.append(stepper)
      findingsSection.append(article)
    })
    section.append(findingsSection)
  }

  root.replaceChildren(section)
  const q = <T extends Element = HTMLElement>(selector: string) => root.querySelector<T>(selector)
  const openedFinding = options.open ? findings[options.open.finding] : undefined
  let picked: number | null = options.open && openedFinding && hasWitness(openedFinding) ? options.open.finding : null
  let taken = picked === null ? 0 : Math.max(0, Math.min(findings[picked]!.witness!.steps.length, options.open?.step ?? 0))

  const drawArrows = () => {
    const stage = q('[data-region="stage"]')
    const arrows = q<SVGSVGElement>(".norn-life-arrows")
    if (!stage || !arrows) return
    const origin = stage.getBoundingClientRect()
    const box = (state: string) => q(`[data-state="${CSS.escape(state)}"]`)?.getBoundingClientRect()
    const active = picked === null ? new Set<string>() : stepState(lifecycle, findings[picked]!, taken).transitions
    // Cards in the non-scrolling stage's own coordinates: identical in LTR and RTL at any scroll offset.
    const local = (rect: DOMRect): Box => ({ left: rect.left - origin.left, right: rect.right - origin.left, top: rect.top - origin.top, bottom: rect.bottom - origin.top })
    const cards = new Map(lifecycle.states.flatMap((state) => { const rect = box(state); return rect ? [[state, local(rect)] as const] : [] }))
    const floor = Math.max(0, ...[...cards.values()].map((card) => card.bottom))
    const fragment = doc.createDocumentFragment()
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
        const route = routeArrow(a, to, hop, rtl, floor, hop < -1 ? lane++ : 0)
        const on = active.has(transition.id)
        const path = svgEl("path", { d: pathData(route), "marker-end": `url(#norn-life-head${on ? "-on" : ""})` })
        if (on) path.setAttribute("data-active", "")
        const text = svgEl("text", { x: String(route.label[0]), y: String(route.label[1]), "text-anchor": "middle" })
        if (on) text.setAttribute("data-active", "")
        text.textContent = label(transition.id)
        fragment.append(path, text)
      }
    }
    const width = Math.ceil(origin.width || stage.scrollWidth)
    const height = Math.ceil(origin.height || stage.scrollHeight)
    arrows.setAttribute("width", String(width))
    arrows.setAttribute("height", String(height))
    arrows.setAttribute("viewBox", `0 0 ${width} ${height}`)
    arrows.querySelectorAll(":scope > path, :scope > text").forEach((node) => node.remove())
    arrows.append(fragment)
  }

  const paintSteps = () => {
    const current = picked === null ? null : stepState(lifecycle, findings[picked]!, taken).state
    for (const card of root.querySelectorAll<HTMLElement>("[data-state]")) card.toggleAttribute("data-current", card.dataset.state === current)
    for (const article of root.querySelectorAll<HTMLElement>("[data-finding]")) {
      const index = Number(article.dataset.finding)
      const finding = findings[index]
      if (!finding || !hasWitness(finding)) continue
      const open = index === picked
      article.toggleAttribute("aria-current", open)
      const stepper = article.querySelector<HTMLElement>('[data-region="stepper"]')
      if (stepper) stepper.hidden = !open
      if (!open) continue
      const total = finding.witness.steps.length
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
      if (count.kind === "unlisted") {
        cell.removeAttribute("data-pending")
        cell.removeAttribute("data-lower-bound")
        cell.setAttribute("data-unlisted", "")
        cell.title = w.unlistedHint
        cell.textContent = w.unlisted
      } else if (count.kind === "count") {
        cell.removeAttribute("data-pending")
        cell.removeAttribute("data-unlisted")
        cell.toggleAttribute("data-lower-bound", count.lowerBound)
        cell.title = count.lowerBound ? w.lowerBoundHint : ""
        cell.textContent = count.lowerBound ? w.atLeast(number.format(count.count)) : number.format(count.count)
      } else {
        cell.setAttribute("data-pending", "")
        cell.removeAttribute("data-lower-bound")
        cell.removeAttribute("data-unlisted")
        cell.title = ""
        cell.textContent = counts.refused || dash ? w.none : w.pending
      }
    }
    const total = q('[data-region="total"]')
    if (total) total.textContent = rows === null ? "" : w.rows(number.format(rows.total), rows.total, rows.lowerBound)
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

  const listeners = new AbortController()
  let onRetry: (() => void) | null = null
  let onRefresh: (() => void) | null = null
  root.addEventListener("click", (event) => {
    const target = event.target as Element | null
    if (onRetry && target?.closest('[data-node="retry-button"]')) {
      onRetry()
      return
    }
    if (onRefresh && target?.closest('[data-node="refresh-button"]')) {
      onRefresh()
      return
    }
    const pick = target?.closest<HTMLElement>("[data-pick]")
    if (pick) {
      const index = Number(pick.dataset.pick)
      const chosen = findings[index]
      if (!chosen || !hasWitness(chosen)) return
      picked = picked === index ? null : index
      taken = 0
      paintSteps()
      return
    }
    const move = target?.closest<HTMLButtonElement>("[data-move]")
    if (move && picked !== null && !move.disabled) {
      const chosen = findings[picked]
      if (!chosen || !hasWitness(chosen)) return
      taken = Math.max(0, Math.min(chosen.witness.steps.length, taken + Number(move.dataset.move)))
      paintSteps()
    }
  }, { signal: listeners.signal })

  paintCounts(countsFrom(lifecycle, null, locale), null, options.read === undefined)
  paintSteps()
  const raf = typeof requestAnimationFrame === "undefined" ? null : requestAnimationFrame(() => drawArrows())
  const resize = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => drawArrows())
  if (resize) {
    resize.observe(boardEl)
    resize.observe(stageEl)
  }

  // Declared-only mode (no live cell connected): show the graph and witnesses without network calls.
  if (options.read === undefined) {
    section.setAttribute("data-machine-state", "declared")
    notesEl.textContent = w.noCell
    notesEl.hidden = false
    return () => {
      listeners.abort()
      if (raf !== null && typeof cancelAnimationFrame !== "undefined") cancelAnimationFrame(raf)
      resize?.disconnect()
    }
  }

  const read = options.read
  // The machine decides every load state; the view only draws the one it is in.
  const page = options.document.payload.ux.pages[0]
  if (!page) throw new Error("the lifecycle view's document has no page")

  const paintState = (state: string, last: LiveAnswer | null) => {
    section.setAttribute("data-machine-state", state)
    unavailableBanner.hidden = state !== "unavailable"
    const text = q('[data-region="unavailable-text"]')
    if (text) text.textContent = state !== "unavailable" ? "" : isRouteNotServed(last) ? `${w.notServed} ${w.again}` : `${w.failed} ${w.again}`
    const refreshButton = q<HTMLButtonElement>('[data-node="refresh-button"]')
    if (refreshButton) refreshButton.disabled = state === "reading"
    if (state === "showing" || state === "refused") {
      const counts = countsFrom(lifecycle, last, locale)
      const body = last && last.status < 400 && isFacetsAnswer(last.body) ? last.body : null
      paintCounts(counts, body ? { total: body.rows, lowerBound: body.lowerBound === true } : null)
    } else if (state === "unavailable") paintCounts(countsFrom(lifecycle, null, locale), null, true)
    drawArrows()
  }

  const paintLiveRefusal = (refusal: LiveSubscriptionRefusal | null) => {
    if (refusal === null) {
      section.removeAttribute("data-live-updates")
      section.removeAttribute("data-live-refusal-code")
    } else {
      section.setAttribute("data-live-updates", "off")
      section.setAttribute("data-live-refusal-code", refusal.code)
    }
    liveOffBanner.hidden = refusal === null
    const text = q('[data-region="live-off-text"]')
    if (text) text.textContent = refusal === null ? "" : options.technical ? `${w.liveOff} (${refusal.code}: ${refusal.message})` : w.liveOff
  }

  const controller = createLifecycleReadController({
    machine: page.machine,
    read,
    ...(options.changes ? { changes: options.changes } : {}),
    onState: paintState,
    onLiveRefusal: paintLiveRefusal,
  })
  onRetry = controller.retry
  onRefresh = controller.refresh

  return () => {
    listeners.abort()
    controller.dispose()
    if (raf !== null && typeof cancelAnimationFrame !== "undefined") cancelAnimationFrame(raf)
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
