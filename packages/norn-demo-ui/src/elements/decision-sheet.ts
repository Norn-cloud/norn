// `<norn-decision-sheet>`: one decision, rendered from the admitted page document
// (`decision-sheet.widget.json`) and driven only by its declared machine (UI-MACHINE-1).
//
// The element is framework-free. It draws UI SDK parts (`ui-radio-group`, `ui-textarea`) whose
// behaviour the host registers by importing `@ui-sdk/web/<part>`; the element never fetches by
// itself, never decides permission, and offers a control only when the machine's current state
// takes one of its events. What it shows about the decision and its outcome is the server's own
// document (the question, options, receipt, resolution, refusal), as sent.

import { enabled, eventsOf } from "../../../norn-react/src/machine/controls.ts"
import { type Driver, type Transport, createDriver } from "../../../norn-react/src/machine/driver.ts"
import type { Evaluate, Fields, Machine, Msg, Snapshot } from "../../../norn-react/src/machine/machine.ts"
import { adoptSheetStyles } from "./sheet.css.ts"

export type Locale = "en" | "ar"
type Text = Readonly<Record<string, string>>

/** The slice of the decisions contract (`norn.decision@1`) the sheet shows. */
export interface SheetDecision {
  readonly id: string
  readonly question: string
  readonly risk: "low" | "medium" | "high"
  /** The run waiting on it; for an approval of a direct action call, the parked call's id with no state. */
  readonly run: { readonly id: string; readonly state?: string | null }
  readonly flow: { readonly title: string | null; readonly ruleId: string }
  readonly step: { readonly id: string; readonly label: string | null }
  readonly options: readonly { readonly id: string; readonly label: string; readonly scope: string; readonly effect: { readonly action: string } | { readonly binding: string } }[]
  readonly agentPick: string | null
  readonly expiresAtMs: number
  /** For the approval of a parked action call, the call; absent for a flow's ask. */
  readonly call?: unknown
}

/**
 * Whether `decision` approves a direct action call: a parked call no run waits on (the cell names
 * the call's own id there, with no state). Approving it runs nothing and there is no run to open:
 * its requester runs it.
 */
export const directCall = (decision: Pick<SheetDecision, "run" | "call">): boolean =>
  decision.call !== undefined && decision.call !== null && (decision.run.state ?? null) === null

interface WidgetNodeDoc {
  readonly nodeId: string
  readonly component: { readonly kind: string; readonly label?: Text; readonly section?: { readonly label: Text } }
  readonly children: readonly WidgetNodeDoc[]
}

/** The admitted widget artifact this element renders: one page with a machine. */
export interface SheetDocument {
  readonly payload: { readonly ux: { readonly pages: readonly { readonly root: WidgetNodeDoc; readonly machine: Machine }[] } }
}

export interface SheetLawInfo {
  readonly name?: string
  readonly rationale?: string
}

export interface SheetOptions {
  readonly document: SheetDocument
  readonly decision: SheetDecision
  /**
   * The ZEN engine the checker type-checked the machine's guards and assignments with. A promise
   * is fine: the sheet paints its first state at once and takes input once the engine is here.
   */
  readonly evaluate: Evaluate | Promise<Evaluate>
  readonly transport: Transport
  readonly locale?: Locale
  /** The cell's clock now (for the countdown); the host keeps it current with `setNow`. */
  readonly now?: number
  /** Show technical identifiers (refusal code, subject, law pin) on refusal panels. */
  readonly technical?: boolean
  /** The verified caller, as the cell named them (`GET /v1/access`); defaults to "You". */
  readonly caller?: string
  /** Optional catalog metadata for disclosed law pins (never read on redacted refusals). */
  readonly laws?: Readonly<Record<string, SheetLawInfo>>
  /** The machine reached a terminal state. */
  readonly onSettled?: (state: string) => void
  /** The person is done with the sheet (Back to inbox, or the machine closed). */
  readonly onClose?: () => void
  /** Follow an in-app link (the run). */
  readonly navigate?: (path: string) => void
  /**
   * PROVISIONAL CONTRACT (#1208, PR #1631 S1/S2): whether the current scoped cell advertises
   * `learning.Episode` so a server-supplied `episodeId` on `:decide` may link to `/episodes/$id`.
   * Defaults to false (fail-closed behind the S1 capability gate).
   */
  readonly canOpenEpisode?: () => boolean
  /** Every step, for telemetry and tests. */
  readonly onStep?: (snapshot: Snapshot, msg: Msg | null) => void
}

const WORDS = {
  en: {
    "risk.low": "Low risk", "risk.medium": "Medium risk", "risk.high": "High risk", suggested: "Suggested", suggestedTitle: "The agent's recommendation; you decide.",
    starts: "Starts an action", records: "Records the choice", optional: "(optional)", notePlaceholder: "Why, for whoever reads the run later",
    choose: "Choose an option", decide: "Decide", decideAs: "Decide: {0}", confirmAs: "Yes, {0}", sending: "Sending…",
    "confirm.high": "This is a high-risk decision.", "confirm.action": "This starts an action.",
    "confirm.body": "You're choosing \"{0}\" ({1}). The run goes on with it as soon as the cell records it.",
    "confirm.bodyNoScope": "You're choosing \"{0}\". The run goes on with it as soon as the cell records it.",
    "confirm.call": "You're choosing \"{0}\" ({1}). The cell records it; an approved call runs when its requester runs it.",
    "confirm.callNoScope": "You're choosing \"{0}\". The cell records it; an approved call runs when its requester runs it.",
    failed: "The cell did not answer; nothing was decided.", openRun: "Open run", seeRun: "See the run go on", seeEpisode: "See episode", back: "Back to inbox",
    decidedAs: "Decided: {0} · {1}", byYou: "by you", byOther: "by {0}", replayed: " (already recorded; the cell returned the first receipt)",
    details: "Details", copy: "Copy", receipt: "Receipt", commit: "Commit", recorded: "Recorded", decision: "Decision",
    resolvedDecided: "Decided by {0} at {1}", resolvedChoice: ": {0}", anotherDecider: "another decider", resolvedBody: "Someone else got there first. Nothing you chose was sent.",
    withdrawn: "Withdrawn at {0}", withdrawnBody: "The run no longer needs this decision.", expiredAt: "Expired at {0}", expiredBody: "The deadline passed before anyone decided.",
    expired: "Expired", gone: "No longer here", awaiting: "Waiting for approval", awaitingBody: "Your choice waits for an approval before it takes effect.",
    leaveBody: "Nothing has been sent. Your choice and note will be cleared; the decision stays in your inbox.", left: "{0} left", lapsed: "Expired",
    "call.committed": "The approved call committed.", "call.refused": "The approved call was refused ({0}); this approval never runs it again.",
    "call.not_executed": "Approved; waiting for the requester to run it.",
    refusedBadge: "Refused", refusedDisclosedHead: "This choice breaks a rule, so nothing was decided.", refusedNotEvaluatedHead: "This choice could not be checked against a rule, so nothing was decided.", refusedRedactedHead: "Refused by a rule. Nothing was decided.",
    stepRequest: "The request", stepCheck: "The check", stepResult: "The result",
    reqDecision: "Decision", reqChoice: "Choice", reqNote: "Note",
    resWho: "Who", resDecidedAt: "Decided at", resReceivedAt: "Received at", resWhy: "Why", resKept: "Kept",
    you: "You", fails: "Fails", notEvaluated: "Not evaluated", refusedRedactedCheck: "It does not say which rule, or which values, to your role.",
    refusedRedactedWhy: "A rule this choice would break. Your role is not told which.",
    refusedKept: "Nothing was decided. The cell keeps no receipt of a refused call.",
    checkedValues: "The values it checked",
  },
  ar: {
    "risk.low": "مخاطرة منخفضة", "risk.medium": "مخاطرة متوسطة", "risk.high": "مخاطرة عالية", suggested: "مقترح", suggestedTitle: "توصية الوكيل؛ والقرار لك.",
    starts: "يبدأ إجراءً", records: "يسجّل الاختيار", optional: "(اختياري)", notePlaceholder: "السبب، لمن يقرأ التشغيل لاحقًا",
    choose: "اختر خيارًا", decide: "قرّر", decideAs: "قرّر: {0}", confirmAs: "نعم، {0}", sending: "جارٍ الإرسال…",
    "confirm.high": "هذا قرار عالي المخاطرة.", "confirm.action": "هذا يبدأ إجراءً.",
    "confirm.body": "تختار «{0}» ({1}). يتابع التشغيل به فور تسجيل الخلية له.",
    "confirm.bodyNoScope": "تختار «{0}». يتابع التشغيل به فور تسجيل الخلية له.",
    "confirm.call": "تختار «{0}» ({1}). تسجّله الخلية؛ ويُنفّذ الإجراء الموافق عليه عندما ينفّذه مقدّم الطلب.",
    "confirm.callNoScope": "تختار «{0}». تسجّله الخلية؛ ويُنفّذ الإجراء الموافق عليه عندما ينفّذه مقدّم الطلب.",
    failed: "لم تُجب الخلية؛ لم يُتخذ أي قرار.", openRun: "فتح التشغيل", seeRun: "تابع التشغيل", seeEpisode: "عرض الحلقة", back: "العودة إلى الصندوق",
    decidedAs: "تقرّر: {0} · {1}", byYou: "بواسطتك", byOther: "بواسطة {0}", replayed: " (مسجّل من قبل؛ أعادت الخلية الإيصال الأول)",
    details: "التفاصيل", copy: "نسخ", receipt: "الإيصال", commit: "الالتزام", recorded: "سُجّل", decision: "القرار",
    resolvedDecided: "قرّر {0} عند {1}", resolvedChoice: ": {0}", anotherDecider: "صاحب قرار آخر", resolvedBody: "سبقك شخص آخر. لم يُرسل شيء مما اخترته.",
    withdrawn: "سُحب عند {0}", withdrawnBody: "لم يعد التشغيل بحاجة إلى هذا القرار.", expiredAt: "انتهت المهلة عند {0}", expiredBody: "انقضت المهلة قبل أن يقرّر أحد.",
    expired: "انتهت المهلة", gone: "لم يعد هنا", awaiting: "بانتظار الموافقة", awaitingBody: "اختيارك بانتظار موافقة قبل أن يسري.",
    leaveBody: "لم يُرسل شيء. سيُمسح اختيارك وملاحظتك؛ ويبقى القرار في صندوقك.", left: "متبقٍ {0}", lapsed: "انتهت المهلة",
    "call.committed": "نُفّذ الإجراء الموافق عليه.", "call.refused": "رُفض الإجراء الموافق عليه ({0})؛ ولن تنفّذه هذه الموافقة مرة أخرى.",
    "call.not_executed": "تمت الموافقة ولم يُنفّذ بعد: يُنفّذ عندما يعيد مقدّم الطلب المحاولة.",
    refusedBadge: "مرفوض", refusedDisclosedHead: "هذا الاختيار يخالف قاعدة، لذلك لم يُتخذ قرار.", refusedNotEvaluatedHead: "تعذّر التحقق من هذا الاختيار وفق القاعدة، لذلك لم يُتخذ قرار.", refusedRedactedHead: "مرفوض بموجب قاعدة. لم يُتخذ قرار.",
    stepRequest: "الطلب", stepCheck: "الفحص", stepResult: "النتيجة",
    reqDecision: "القرار", reqChoice: "الاختيار", reqNote: "الملاحظة",
    resWho: "من", resDecidedAt: "وقت القرار", resReceivedAt: "وقت الاستلام", resWhy: "السبب", resKept: "المحفوظ",
    you: "أنت", fails: "فشل", notEvaluated: "لم يُقيَّم", refusedRedactedCheck: "لا تذكر الخلية القاعدة أو القيم لدورك.",
    refusedRedactedWhy: "قاعدة يخالفها هذا الاختيار. لا يُكشف لدورك أي قاعدة.",
    refusedKept: "لم يُتخذ أي قرار. لا تحتفظ الخلية بإيصال لطلب مرفوض.",
    checkedValues: "القيم التي فُحصت",
  },
} as const satisfies Record<Locale, Record<string, string>>

type WordKey = keyof (typeof WORDS)["en"]

const escape = (text: string) => text.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`)
const fill = (template: string, ...values: readonly string[]) => values.reduce((text, value, index) => text.replace(`{${index}}`, value), template)

function nodes(root: WidgetNodeDoc): Map<string, WidgetNodeDoc> {
  const out = new Map<string, WidgetNodeDoc>()
  const walk = (node: WidgetNodeDoc) => {
    out.set(node.nodeId, node)
    node.children.forEach(walk)
  }
  walk(root)
  return out
}

function label(node: WidgetNodeDoc | undefined, locale: Locale): string {
  const text = node?.component.label ?? node?.component.section?.label
  return text?.[locale] ?? text?.en ?? ""
}

/** Deciding this option asks for confirmation: a high-risk decision, or an option that starts an action. */
export const needsConfirm = (decision: Pick<SheetDecision, "risk" | "options">, optionId: string) =>
  decision.risk === "high" || decision.options.some((option) => option.id === optionId && "action" in option.effect)

/** The request a `decideDecision` operation call sends (POST /v1/decisions/{id}:decide). */
export function decideRequest(decisionId: string, input: Fields, idempotencyKey: string): { readonly url: string; readonly init: RequestInit } {
  const note = typeof input.note === "string" && input.note.trim() !== "" ? { note: input.note } : {}
  return {
    url: `/v1/decisions/${encodeURIComponent(decisionId)}:decide`,
    init: { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ choice: input.choice, ...note, idempotencyKey }) },
  }
}

const UNITS: Readonly<Record<Locale, { readonly day: (n: number) => string; readonly h: string; readonly min: string; readonly s: string }>> = {
  en: { day: (n) => (n === 1 ? "day" : "days"), h: "h", min: "min", s: "s" },
  ar: { day: (n) => (n === 1 ? "يوم" : n === 2 ? "يومان" : n <= 10 ? "أيام" : "يومًا"), h: "س", min: "د", s: "ث" },
}

/** What is left before the deadline, in the two largest units; null once it has passed. */
export function remaining(expiresAtMs: number, now: number, locale: Locale = "en"): string | null {
  const ms = expiresAtMs - now
  if (ms <= 0) return null
  const u = UNITS[locale]
  const s = Math.floor(ms / 1000)
  const d = Math.floor(s / 86_400)
  const h = Math.floor((s % 86_400) / 3600)
  const m = Math.floor((s % 3600) / 60)
  if (d > 0) return [`${d} ${u.day(d)}`, h > 0 ? `${h} ${u.h}` : ""].filter(Boolean).join(" ")
  if (h > 0) return [`${h} ${u.h}`, m > 0 ? `${m} ${u.min}` : ""].filter(Boolean).join(" ")
  return m > 0 ? `${m} ${u.min}` : `${s} ${u.s}`
}

/** A flow's name in words when the server sends no title: `canon.refund_review.flow.rule` → "Refund review". */
export function flowName(flow: SheetDecision["flow"]): string {
  if (flow.title) return flow.title
  const id = flow.ruleId.replace(/\.flow\.rule$/, "")
  const words = id.slice(id.lastIndexOf(".") + 1).replace(/([a-z])([A-Z])/g, "$1 $2").replaceAll(/[_-]+/g, " ").trim()
  return words === "" ? flow.ruleId : words.charAt(0).toUpperCase() + words.slice(1).toLowerCase()
}

/** The sheet's markup for one decision. Structure only; `paint` applies the machine's state. */
export function sheetMarkup(document: SheetDocument, decision: SheetDecision, locale: Locale = "en"): string {
  const page = document.payload.ux.pages[0]
  if (!page) throw new Error("the sheet document has no page")
  const byId = nodes(page.root)
  const w = WORDS[locale]
  const id = escape(decision.id)
  const button = (node: string, variant: string, text = label(byId.get(node), locale)) =>
    `<button type="button" data-node="${node}" data-variant="${variant}" class="norn-button">${escape(text)}</button>`
  // The scope line says something only when options differ in it; one shared scope is the sheet's own subject.
  const scopes = new Set(decision.options.map((option) => option.scope))
  const options = decision.options
    .map(
      (option) => `
      <div class="norn-option" data-option="${escape(option.id)}">
        <label class="norn-option-row" data-slot="label">
          <ui-radio value="${escape(option.id)}" aria-label="${escape(option.label)}"><ui-radio-indicator></ui-radio-indicator></ui-radio>
          <span class="norn-option-text">
            <span class="norn-option-label">${escape(option.label)}${option.id === decision.agentPick ? ` <span class="norn-chip" data-tone="agent" title="${escape(w.suggestedTitle)}">${escape(w.suggested)}</span>` : ""}</span>
            ${scopes.size > 1 ? `<span class="norn-meta">${escape(option.scope)}</span>` : ""}
            <span class="norn-meta">${escape("action" in option.effect ? w.starts : w.records)}</span>
          </span>
        </label>
        <div class="norn-refusal-slot" role="status" data-refusal hidden></div>
      </div>`,
    )
    .join("")
  return `
    <section class="norn-sheet" dir="${locale === "ar" ? "rtl" : "ltr"}" lang="${escape(locale)}" aria-label="${escape(label(byId.get("decision-sheet"), locale))}">
      <header class="norn-sheet-header">
        <h2 class="norn-title" data-region="question">${escape(decision.question)}</h2>
        <p class="norn-meta">${escape(flowName(decision.flow))} · ${escape(decision.step.label ?? decision.step.id)}</p>
        <div class="norn-actions">
          <span class="norn-chip" data-tone="risk-${escape(decision.risk)}">${escape(w[`risk.${decision.risk}`])}</span>
          <span class="norn-meta" data-region="countdown"></span>
          <span class="norn-spacer"></span>
          ${directCall(decision) ? "" : `<a class="norn-link" href="/runs/${escape(encodeURIComponent(decision.run.id))}" data-link>${escape(w.openRun)}</a>`}
        </div>
      </header>
      <div class="norn-status" role="alert" data-region="status" hidden></div>
      <form class="norn-form" data-region="form" novalidate aria-label="${escape(label(byId.get("decision-actions"), locale))}">
        <fieldset class="norn-fieldset" data-region="decision-choices">
          <legend class="norn-legend">${escape(label(byId.get("decision-choices"), locale))}</legend>
          <ui-radio-group data-node="option-choice" aria-label="${escape(label(byId.get("option-choice"), locale))}">${options}</ui-radio-group>
          <p class="norn-problem" role="alert" data-problem="/choice" id="norn-choice-${id}" hidden></p>
        </fieldset>
        <div class="norn-field" data-region="decision-note">
          <label class="norn-legend" for="norn-note-${id}">${escape(label(byId.get("note-input"), locale))} <span class="norn-meta">${escape(w.optional)}</span></label>
          <ui-textarea data-node="note-input"><textarea id="norn-note-${id}" rows="3" placeholder="${escape(w.notePlaceholder)}" aria-describedby="norn-note-${id}-problem"></textarea></ui-textarea>
          <p class="norn-problem" role="alert" data-problem="/note" id="norn-note-${id}-problem" hidden></p>
        </div>
        <div class="norn-confirm" role="alertdialog" aria-labelledby="norn-confirm-${id}" data-region="confirm" data-tone="${decision.risk === "high" ? "danger" : "neutral"}" hidden>
          <p class="norn-confirm-title" id="norn-confirm-${id}">${escape(decision.risk === "high" ? w["confirm.high"] : w["confirm.action"])}</p>
          <p class="norn-meta" data-region="confirm-body"></p>
        </div>
        <div class="norn-confirm" role="alertdialog" aria-labelledby="norn-leave-${id}" data-region="decision-leave" data-tone="neutral" hidden>
          <p class="norn-confirm-title" id="norn-leave-${id}">${escape(label(byId.get("decision-leave"), locale))}</p>
          <p class="norn-meta">${escape(w.leaveBody)}</p>
          <div class="norn-actions"><span class="norn-spacer"></span>${button("stay-button", "ghost")}${button("leave-button", "outline")}</div>
        </div>
        <footer class="norn-actions norn-footer" data-region="decision-actions">
          <span class="norn-meta" data-region="hint"></span>
          ${button("close-button", "ghost")}<span class="norn-spacer"></span>${button("back-button", "ghost")}${button("retry-button", "outline")}${button("confirm-button", decision.risk === "high" ? "danger" : "solid")}${button("decide-button", "solid")}
        </footer>
      </form>
      <section class="norn-outcome-card" data-region="decision-outcome" hidden>
        <p class="norn-outcome" role="status" aria-live="polite" data-region="outcome-title"></p>
        <p class="norn-meta" data-region="outcome-body"></p>
        <p class="norn-meta" data-region="outcome-call" hidden></p>
        <a class="norn-link" data-region="outcome-run" data-link hidden>${escape(w.seeRun)}</a>
        <a class="norn-link" data-region="outcome-episode" data-link hidden>${escape(w.seeEpisode)}</a>
        <details class="norn-details" data-region="outcome-details" hidden>
          <summary class="norn-meta">${escape(w.details)}</summary>
          <dl class="norn-dl"></dl>
        </details>
        <div class="norn-actions"><span class="norn-spacer"></span><button type="button" class="norn-button" data-variant="solid" data-host="back">${escape(w.back)}</button></div>
      </section>
    </section>`
}

const record = (value: unknown): Readonly<Record<string, unknown>> => (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : {})
const text = (value: unknown): string | null => (typeof value === "string" ? value : null)
const time = (ms: number, locale: Locale) => new Date(ms).toLocaleTimeString(locale, { hour: "2-digit", minute: "2-digit" })

interface Problem {
  readonly code: string
  readonly message: string
  readonly pointer: string | null
}

/** A refusal's diagnostics with their pointers (`span.pointer`, or a flat `pointer`). */
function problems(body: unknown): readonly Problem[] {
  const list = record(body).diagnostics
  if (!Array.isArray(list)) return []
  return list.map((item) => {
    const d = record(item)
    const pointer = text(record(d.span).pointer) ?? text(d.pointer)
    return { code: text(d.code) ?? "", message: text(d.message) ?? "", pointer }
  })
}

const UNPROVEN_DETAIL = "diagnostic detail is unavailable under the caller's disclosure authority"
const GENERIC_LAW_MESSAGE = new Set(["a declared precondition does not hold", "the decider law is not satisfied"])

export function refusalPanelMarkup(
  body: unknown,
  context: {
    readonly decision: SheetDecision
    readonly choiceLabel: string
    readonly note: string
    readonly locale: Locale
    readonly technical: boolean
    readonly caller: string | undefined
    readonly laws: Readonly<Record<string, SheetLawInfo>> | undefined
    readonly receivedAt: Date
  },
): { readonly html: string; readonly variant: "" | "disclosed" | "redacted" } {
  const w = WORDS[context.locale]
  const top = record(body)
  const code = text(top.code) ?? ""
  const message = text(top.message) ?? ""
  const explanation = text(top.explanation) ?? ""
  const decidedAt = text(top.decidedAt)
  const rawReason = record(top.reason)
  const law = text(rawReason.law)
  const disclosed = law !== null && law !== ""
  const isLaw = disclosed || code === "core.laws.violated" || code === "decision.law_not_satisfied"
  if (!isLaw) {
    const plain = [message, explanation].filter(Boolean).join(" ")
    return { html: plain === "" ? "" : `<span class="norn-note">${escape(plain)}</span>`, variant: "" }
  }
  const variant: "disclosed" | "redacted" = disclosed ? "disclosed" : "redacted"
  const pin = record(rawReason.pin)
  const pinArtifact = text(pin.artifactId)
  const pinVersion = text(pin.version)
  const subject = text(rawReason.subject) ?? ""
  const values = record(rawReason.values)
  const valueEntries = Object.entries(values)
  // On `decision.law_not_satisfied`, the chosen option belongs in "The request", not as an evaluated law operand.
  const checkedEntries = code === "decision.law_not_satisfied"
    ? valueEntries.filter(([k]) => k !== "choice")
    : valueEntries
  // A decider law the cell could not evaluate carries no evaluated operands (or states it could not evaluate).
  const unevaluated = disclosed && code === "decision.law_not_satisfied"
    && (checkedEntries.length === 0 || /cannot evaluate/i.test(explanation))
  // Catalog metadata is read ONLY when the server disclosed the law reason; never on a redacted refusal.
  const lawInfo = disclosed ? (context.laws?.[pinArtifact ?? ""] ?? context.laws?.[law]) : undefined

  const cellTime = decidedAt !== null && !Number.isNaN(Date.parse(decidedAt)) ? new Date(decidedAt) : null
  const shownTime = cellTime ?? context.receivedAt
  const timeIso = cellTime !== null ? decidedAt! : shownTime.toISOString()
  const timeLabel = cellTime !== null ? w.resDecidedAt : w.resReceivedAt
  const formattedTime = shownTime.toLocaleString(context.locale, { dateStyle: "medium", timeStyle: "short" })

  const reqRows: [string, string][] = [
    [w.reqDecision, context.decision.question],
    [w.reqChoice, context.choiceLabel],
    ...(context.note.trim() !== "" ? [[w.reqNote, context.note] as [string, string]] : []),
  ]
  const checkBody = disclosed
    ? (() => {
        const lead = lawInfo?.rationale ?? (message || law)
        const chips = checkedEntries.length > 0
          ? `<div class="norn-checked-list" aria-label="${escape(w.checkedValues)}">${checkedEntries.map(([k, v]) =>
            `<span class="norn-checked-chip" data-checked="${escape(k)}"><span class="norn-checked-key">${escape(k.replaceAll(".", " ").replaceAll("_", " "))}</span><span class="norn-checked-val">${escape(typeof v === "string" ? v : JSON.stringify(v))}</span></span>`).join("")}</div>`
          : ""
        const extraNote = explanation !== "" && explanation !== lead && explanation !== message && explanation !== UNPROVEN_DETAIL
          ? `<p class="norn-meta">${escape(explanation)}</p>` : ""
        const tech = context.technical
          ? `<div class="norn-refusal-tech"><span>${escape([code, subject].filter(Boolean).join(" · "))}</span>${pinArtifact ? `<span>${escape(`${pinArtifact}@${pinVersion ?? "?"}`)}</span>` : ""}${explanation ? `<span>${escape(explanation)}</span>` : ""}</div>`
          : ""
        const verdictTone = unevaluated ? "risk-medium" : "risk-high"
        const verdictAttr = unevaluated ? "not-evaluated" : "fails"
        const verdictLabel = unevaluated ? w.notEvaluated : w.fails
        return `<p class="norn-refusal-check-lead"><span>${escape(lead)}</span><span class="norn-chip" data-tone="${verdictTone}" data-verdict="${verdictAttr}">${escape(verdictLabel)}</span></p>${chips}${extraNote}${tech}`
      })()
    : (() => {
        const parts: string[] = []
        if (message !== "") parts.push(`<p class="norn-refusal-check-msg">${escape(message)}</p>`)
        if (explanation !== "" && explanation !== UNPROVEN_DETAIL && explanation !== message) {
          parts.push(`<p class="norn-meta">${escape(explanation)}</p>`)
        }
        if (explanation === "" || explanation === UNPROVEN_DETAIL) {
          parts.push(`<p class="norn-meta">${escape(w.refusedRedactedCheck)}</p>`)
        }
        const tech = context.technical && code !== "" ? `<div class="norn-refusal-tech"><span>${escape(code)}</span></div>` : ""
        return `${parts.join("")}${tech}`
      })()

  const whyText = disclosed
    ? (lawInfo?.name ?? lawInfo?.rationale ?? (message || explanation || law))
    : (message !== "" && !GENERIC_LAW_MESSAGE.has(message) ? message : w.refusedRedactedWhy)

  const headerText = disclosed
    ? (unevaluated ? w.refusedNotEvaluatedHead : w.refusedDisclosedHead)
    : w.refusedRedactedHead

  const html = `
    <div class="norn-refusal-card">
      <div class="norn-refusal-header">
        <span class="norn-chip" data-tone="risk-high">${escape(w.refusedBadge)}</span>
        <span class="norn-refusal-head">${escape(headerText)}</span>
      </div>
      <ol class="norn-refusal-steps">
        <li class="norn-refusal-step">
          <section aria-label="${escape(w.stepRequest)}">
            <h4>${escape(w.stepRequest)}</h4>
            <dl class="norn-refusal-dl">${reqRows.map(([k, v]) => `<dt>${escape(k)}</dt><dd>${escape(v)}</dd>`).join("")}</dl>
          </section>
        </li>
        <li class="norn-refusal-step">
          <section aria-label="${escape(w.stepCheck)}">
            <h4>${escape(w.stepCheck)}</h4>
            ${checkBody}
          </section>
        </li>
        <li class="norn-refusal-step">
          <section aria-label="${escape(w.stepResult)}">
            <h4>${escape(w.stepResult)}</h4>
            <dl class="norn-refusal-dl">
              <dt>${escape(w.resWho)}</dt><dd>${escape(context.caller ?? w.you)}</dd>
              <dt>${escape(timeLabel)}</dt><dd><time datetime="${escape(timeIso)}" title="${escape(timeIso)}">${escape(formattedTime)}</time></dd>
              <dt>${escape(w.resWhy)}</dt><dd>${escape(whyText)}</dd>
              <dt>${escape(w.resKept)}</dt><dd>${escape(w.refusedKept)}</dd>
            </dl>
          </section>
        </li>
      </ol>
    </div>`
  return { html, variant }
}

export interface MountedDecisionSheet {
  readonly driver: Driver
  readonly raise: (nodeId: string, extra?: Record<string, unknown>) => boolean
  readonly choose: (optionId: string) => boolean
  readonly setNow: (ms: number) => void
  readonly setContext: (next: { readonly technical?: boolean; readonly caller?: string; readonly laws?: Readonly<Record<string, SheetLawInfo>> }) => void
}

/**
 * Mount one decision into `root` and run its machine. Returns the handle so a host can drive or dispose it.
 * Every control's presence and enabled state comes from the machine via `enabled`.
 */
export function mountDecisionSheet(root: HTMLElement, options: SheetOptions): MountedDecisionSheet {
  const { document, decision } = options
  // The engine evaluates guards and assignments only; until it arrives the sheet shows its first
  // state read-only and raises nothing, so no event is ever decided without it.
  let engine: Evaluate | null = typeof options.evaluate === "function" ? options.evaluate : null
  const evaluate: Evaluate = (expression, input) => {
    if (engine === null) throw new Error("the expression engine is not loaded yet")
    return engine(expression, input)
  }
  const locale = options.locale ?? "en"
  const w = WORDS[locale]
  const page = document.payload.ux.pages[0]
  if (!page) throw new Error("the sheet document has no page")
  const machine = page.machine
  adoptSheetStyles(root.ownerDocument)
  root.innerHTML = sheetMarkup(document, decision, locale)
  let lastBody: unknown
  let lastRefusedAt: Date = new Date()
  let lastSentNote = ""
  // The server's refusal panel for the refused choice, kept from the refusal that refused it.
  let refusedPanel: { readonly html: string; readonly variant: "" | "disclosed" | "redacted" } = { html: "", variant: "" }
  let now = options.now
  let technical = options.technical === true
  let caller = options.caller
  let laws = options.laws
  const transport: Transport = {
    async call(call, input, callOptions) {
      lastSentNote = typeof input.note === "string" ? input.note : ""
      const classified = await options.transport.call(call, input, callOptions)
      lastBody = classified.body
      if (classified.outcome === "refused") lastRefusedAt = new Date()
      return classified
    },
  }
  const q = <T extends Element = HTMLElement>(selector: string) => root.querySelector<T>(selector)
  const say = (key: WordKey, ...values: string[]) => fill(w[key], ...values)
  const optionLabel = (choice: unknown) => decision.options.find((option) => option.id === choice)?.label ?? String(choice ?? "")

  const paintCountdown = (settled: boolean) => {
    const countdown = q('[data-region="countdown"]')
    if (!countdown) return
    const left = now === undefined || settled ? undefined : remaining(decision.expiresAtMs, now, locale)
    countdown.textContent = left === undefined ? "" : left === null ? w.lapsed : say("left", left)
    countdown.toggleAttribute("data-urgent", now !== undefined && decision.expiresAtMs - now < 3_600_000)
  }

  const paintOutcome = (state: string) => {
    const card = q('[data-region="decision-outcome"]')
    const title = q('[data-region="outcome-title"]')
    const body = q('[data-region="outcome-body"]')
    const run = q<HTMLAnchorElement>('[data-region="outcome-run"]')
    const episode = q<HTMLAnchorElement>('[data-region="outcome-episode"]')
    const details = q('[data-region="outcome-details"]')
    const callLine = q('[data-region="outcome-call"]')
    if (!card || !title || !body || !run || !details || !callLine) return
    const doc = record(lastBody)
    // APPROVAL-RECOVERY-1: where an approved call stands, exactly as the cell answered it.
    const call = record(state === "resolved" ? record(doc.resolution).call : state === "decided" ? doc.call : undefined)
    const callState = text(call.state)
    callLine.textContent = callState === "committed" ? w["call.committed"]
      : callState === "refused" ? say("call.refused", text(call.code) ?? "")
      : callState === "not_executed" ? w["call.not_executed"] : ""
    callLine.hidden = callLine.textContent === ""
    if (callState === null) callLine.removeAttribute("data-call-state")
    else callLine.setAttribute("data-call-state", callState)
    run.hidden = true
    if (episode) episode.hidden = true
    details.hidden = true
    let heading = ""
    let words = ""
    if (state === "decided") {
      const decided = record(doc.decision)
      const by = text(decided.decidedBy)
      heading = say("decidedAs", optionLabel(decided.choice), by === null || by === "you" ? w.byYou : say("byOther", by)) + (doc.outcome === "replayed" ? w.replayed : "")
      words = text(decided.note) ? `“${text(decided.note)}”` : ""
      const runId = text(record(doc.run).id)
      // A direct call's approval has no run behind it: the call line says it waits for its requester.
      if (runId && !directCall(decision)) {
        run.href = `/runs/${encodeURIComponent(runId)}`
        run.hidden = false
      }
      // PROVISIONAL CONTRACT (#1208, PR #1631 S1/S2): only link when the cell advertises learning.Episode and supplies episodeId.
      const episodeId = text(doc.episodeId)
      if (episode && episodeId && options.canOpenEpisode?.() === true) {
        episode.href = `/episodes/${encodeURIComponent(episodeId)}`
        episode.hidden = false
      }
      const receipt = record(doc.receipt)
      const rows: [string, string][] = [
        [w.receipt, text(receipt.receiptHash) ?? ""],
        [w.commit, String(receipt.commitSeq ?? "")],
        [w.recorded, typeof receipt.recordedAtMs === "number" ? new Date(receipt.recordedAtMs).toLocaleString(locale, { dateStyle: "medium", timeStyle: "medium" }) : ""],
        [w.decision, decision.id],
      ]
      const dl = details.querySelector("dl")
      // Dates, numbers and ids read whole; only the receipt hash shortens, with a copy button for all of it.
      const cell = (term: string, value: string) =>
        term === w.receipt && value.length > 20
          ? `<dd class="norn-hash"><code title="${escape(value)}">${escape(value.slice(0, 16))}…</code><button type="button" class="norn-button" data-variant="ghost" data-copy="${escape(value)}">${escape(w.copy)}</button></dd>`
          : `<dd><span class="norn-value">${escape(value)}</span></dd>`
      if (dl) dl.innerHTML = rows.map(([term, value]) => `<dt>${escape(term)}</dt>${cell(term, value)}`).join("")
      details.hidden = false
    } else if (state === "resolved") {
      const resolution = record(doc.resolution)
      const at = typeof resolution.atMs === "number" ? time(resolution.atMs, locale) : ""
      if (resolution.resolution === "withdrawn") [heading, words] = [say("withdrawn", at), w.withdrawnBody]
      else if (resolution.resolution === "expired") [heading, words] = [say("expiredAt", at), w.expiredBody]
      else {
        const choice = text(resolution.choice)
        heading = say("resolvedDecided", text(resolution.by) ?? w.anotherDecider, at) + (choice === null ? "" : say("resolvedChoice", optionLabel(choice)))
        words = w.resolvedBody
      }
    } else if (state === "expired") [heading, words] = [w.expired, text(doc.message) ?? w.expiredBody]
    else if (state === "gone") [heading, words] = [w.gone, text(doc.message) ?? ""]
    else if (state === "awaiting-approval") [heading, words] = [w.awaiting, w.awaitingBody]
    title.textContent = heading
    body.textContent = words
    body.hidden = words === ""
    card.hidden = heading === ""
  }

  const paint = (snapshot: Snapshot) => {
    const state = snapshot.stateId
    const role = machine.states.find((candidate) => candidate.stateId === state)?.role
    const settled = role === "terminal"
    const ready = engine !== null
    q(".norn-sheet")?.setAttribute("data-state", state)
    q(".norn-sheet")?.setAttribute("data-engine", ready ? "ready" : "loading")
    // A terminal machine shows only its outcome; otherwise the form stays, with what it offers now.
    q('[data-region="form"]')?.toggleAttribute("hidden", settled)
    q('[data-host="back"]')?.toggleAttribute("hidden", !settled)
    paintCountdown(settled)
    paintOutcome(state)
    const choice = snapshot.fields.choice
    for (const element of root.querySelectorAll<HTMLButtonElement>("button[data-node]")) {
      const node = element.dataset.node ?? ""
      const on = ready && enabled(machine, evaluate, snapshot, node)
      // Decide stays in view while a choice is still to be made, so the form says what it waits for.
      element.hidden = !on && !(node === "decide-button" && (state === "choosing" || state === "sending" || state === "refused"))
      element.disabled = !on
    }
    const decideButton = q<HTMLButtonElement>('[data-node="decide-button"]')
    if (decideButton) decideButton.textContent = state === "sending" ? w.sending : choice ? say("decideAs", optionLabel(choice)) : w.decide
    const confirmButton = q<HTMLButtonElement>('[data-node="confirm-button"]')
    if (confirmButton) confirmButton.textContent = say("confirmAs", optionLabel(choice).toLowerCase())
    const hint = q('[data-region="hint"]')
    if (hint) hint.textContent = choice || !ready || !enabled(machine, evaluate, snapshot, "option-choice") ? "" : w.choose
    // The footer shows only while it offers something.
    const footer = q('[data-region="decision-actions"]')
    if (footer) footer.hidden = ![...footer.querySelectorAll<HTMLButtonElement>("button[data-node]")].some((button) => !button.hidden)
    const confirmBody = q('[data-region="confirm-body"]')
    if (confirmBody) {
      // The scope is named only when the options differ in it.
      const scope = new Set(decision.options.map((o) => o.scope)).size > 1 ? decision.options.find((o) => o.id === choice)?.scope ?? "" : ""
      const direct = directCall(decision)
      confirmBody.textContent = say(scope ? (direct ? "confirm.call" : "confirm.body") : direct ? "confirm.callNoScope" : "confirm.bodyNoScope", optionLabel(choice), scope)
    }
    q('[data-region="confirm"]')?.toggleAttribute("hidden", state !== "confirming")
    q('[data-region="decision-leave"]')?.toggleAttribute("hidden", state !== "leaving")
    const editable = ready && enabled(machine, evaluate, snapshot, "option-choice")
    // Choices and the note show while they can change, while a call is in flight, and while the
    // engine loads (read-only, so the sheet does not jump when it arrives).
    for (const region of ["decision-choices", "decision-note"]) q(`[data-region="${region}"]`)?.toggleAttribute("hidden", !editable && state !== "sending" && ready)
    q("textarea")?.toggleAttribute("disabled", !editable)
    q('[data-node="option-choice"]')?.toggleAttribute("aria-disabled", !editable)
    q('[data-node="option-choice"]')?.toggleAttribute("disabled", !ready)
    // The server's words for a refusal: the top-level message and unpointed diagnostics at the top,
    // pointed diagnostics at their field.
    const found = state === "refused" ? problems(lastBody) : []
    const top = record(lastBody)
    const status = q('[data-region="status"]')
    if (status) {
      // A refusal of one option reads under that option, not here.
      const onOption = state === "refused" && snapshot.fields.refusedChoice != null
      const message = onOption ? "" : state === "refused" ? text(top.message) ?? String(snapshot.fields.lastCode ?? "") : state === "failed" ? w.failed : ""
      const extra = onOption ? [] : found.filter((p) => p.pointer !== "/choice" && p.pointer !== "/note" && p.message !== message).map((p) => p.message)
      status.textContent = [message, ...extra].filter(Boolean).join(" ")
      status.hidden = status.textContent === ""
      status.dataset.tone = "danger"
    }
    for (const problem of root.querySelectorAll<HTMLElement>("[data-problem]")) {
      const words = found.filter((p) => p.pointer === problem.dataset.problem).map((p) => p.message).join(" ")
      problem.textContent = words
      problem.hidden = words === ""
    }
    // A choice the cell refused by law or permission stays refused: its radio is disabled and the
    // server's structured request/check/result refusal panel reads under it, while other options stay open.
    const refusedChoice = typeof snapshot.fields.refusedChoice === "string" ? snapshot.fields.refusedChoice : null
    if (state === "refused" && refusedChoice !== null) {
      refusedPanel = refusalPanelMarkup(lastBody, {
        decision,
        choiceLabel: optionLabel(refusedChoice),
        note: lastSentNote,
        locale,
        technical,
        caller,
        laws,
        receivedAt: lastRefusedAt,
      })
    }
    for (const option of root.querySelectorAll<HTMLElement>("[data-option]")) {
      const refused = option.dataset.option === refusedChoice
      option.toggleAttribute("data-refused", refused)
      option.querySelector("ui-radio")?.toggleAttribute("disabled", refused)
      const why = option.querySelector<HTMLElement>("[data-refusal]")
      if (why) {
        if (refused) {
          why.setAttribute("data-refusal", refusedPanel.variant)
          why.innerHTML = refusedPanel.html
          why.hidden = refusedPanel.html === ""
        } else {
          why.setAttribute("data-refusal", "")
          why.innerHTML = ""
          why.hidden = true
        }
      }
    }
    // The radio group shows the machine's choice: a refusal that clears it unchecks the option.
    const radios = q<HTMLElement & { value?: unknown }>('[data-node="option-choice"]')
    if (radios && (radios.value ?? "") !== (snapshot.fields.choice ?? "")) radios.value = snapshot.fields.choice ?? ""
    const textarea = q<HTMLTextAreaElement>("textarea")
    if (textarea) {
      if (found.some((p) => p.pointer === "/note")) textarea.setAttribute("aria-invalid", "true")
      else textarea.removeAttribute("aria-invalid")
      if (textarea.value !== String(snapshot.fields.note ?? "")) textarea.value = String(snapshot.fields.note ?? "")
    }
  }

  const driver = createDriver({
    machine,
    evaluate,
    transport,
    onStep: (step, msg) => {
      paint(step.snapshot)
      options.onStep?.(step.snapshot, msg)
      const role = machine.states.find((s) => s.stateId === step.snapshot.stateId)?.role
      if (step.fired && role === "terminal") {
        options.onSettled?.(step.snapshot.stateId)
        if (step.snapshot.stateId === "closed") options.onClose?.()
      }
    },
  })

  /** Raise the first of the node's events the current state takes; nothing before the engine. */
  const raise = (nodeId: string, payload?: Fields) => {
    if (engine === null) return false
    for (const event of eventsOf(machine, nodeId)) {
      if (driver.dispatch({ kind: "user", eventId: event.eventId, ...(payload ? { payload } : {}) }).fired) return true
    }
    return false
  }
  root.addEventListener("click", (event) => {
    const target = event.target as Element | null
    const link = target?.closest<HTMLAnchorElement>("a[data-link]")
    if (link && options.navigate) {
      event.preventDefault()
      options.navigate(link.getAttribute("href") ?? "")
      return
    }
    const copy = target?.closest<HTMLElement>("[data-copy]")
    if (copy) {
      void navigator.clipboard?.writeText(copy.dataset.copy ?? "")
      return
    }
    if (target?.closest("[data-host=back]")) {
      options.onClose?.()
      return
    }
    const button = target?.closest<HTMLButtonElement>("button[data-node]")
    if (button && !button.disabled) raise(button.dataset.node ?? "")
  })
  root.addEventListener("submit", (event) => {
    event.preventDefault()
    if (!raise("confirm-button")) raise("decide-button")
  })
  // UI SDK reports a change on the group itself (`valuechange`, not bubbling); its `value` is the checked radio.
  const group = q<HTMLElement & { value?: unknown }>('[data-node="option-choice"]')
  group?.addEventListener("valuechange", (event) => {
    const detail = (event as CustomEvent<{ value?: unknown }>).detail
    const value = typeof detail?.value === "string" ? detail.value : typeof group.value === "string" ? group.value : ""
    if (value !== "") raise("option-choice", { value, confirm: needsConfirm(decision, value) })
  })
  // The native textarea reports every edit, upgraded or not.
  root.addEventListener("input", (event) => {
    const target = event.target as HTMLTextAreaElement | null
    if (target?.tagName === "TEXTAREA" && String(driver.snapshot().fields.note ?? "") !== target.value) raise("note-input", { value: target.value })
  })
  /** Pick an option exactly as a click on its radio does: a disabled or unknown option is not picked. */
  const choose = (optionId: string) => {
    const radio = [...root.querySelectorAll<HTMLElement>("ui-radio")].find((candidate) => candidate.getAttribute("value") === optionId)
    if (!radio || radio.hasAttribute("disabled") || radio.closest("[hidden]")) return false
    return raise("option-choice", { value: optionId, confirm: needsConfirm(decision, optionId) })
  }
  const setNow = (ms: number) => {
    now = ms
    paint(driver.snapshot())
  }
  const setContext = (next: { readonly technical?: boolean; readonly caller?: string; readonly laws?: Readonly<Record<string, SheetLawInfo>> }) => {
    technical = next.technical === true
    caller = next.caller
    laws = next.laws
    paint(driver.snapshot())
  }
  paint(driver.snapshot())
  if (engine === null) {
    void Promise.resolve(options.evaluate).then(
      (loaded) => {
        engine = loaded
        paint(driver.snapshot())
      },
      () => q(".norn-sheet")?.setAttribute("data-engine", "unavailable"),
    )
  }
  return { driver, raise, choose, setNow, setContext }
}

/** `<norn-decision-sheet>`: set `options` to mount; removing the element disposes the machine. */
export interface NornDecisionSheet extends HTMLElement {
  options: SheetOptions
  /** The machine's current snapshot, for hosts and tests. */
  readonly snapshot: Snapshot | null
  /** The expression engine has arrived, so the sheet takes input. */
  readonly ready: boolean
  /** Raise the first event of `nodeId` the current state takes (a host's Escape raises `close-button`). */
  request(nodeId: string): boolean
  /**
   * Pick `optionId` through the machine, as a click on its radio would (a host's number keys). False,
   * with nothing changed, when the option is unknown, disabled, or the current state takes no choice.
   */
  choose(optionId: string): boolean
  /** Repaint the countdown at the cell's clock `ms`; there once the sheet has mounted. */
  readonly setNow?: (ms: number) => void
  /** Update view/caller/law display context without resetting the sheet's machine. */
  readonly setContext?: (next: { readonly technical?: boolean; readonly caller?: string; readonly laws?: Readonly<Record<string, SheetLawInfo>> }) => void
}

/**
 * Register `<norn-decision-sheet>` once. The class is made here, not at import, so the module loads
 * where there is no DOM (tests, server rendering).
 */
export function defineDecisionSheet(name = "norn-decision-sheet"): void {
  if (customElements.get(name)) return
  class Element extends HTMLElement implements NornDecisionSheet {
    #mounted: MountedDecisionSheet | null = null
    #options: SheetOptions | null = null
    set options(options: SheetOptions) {
      this.#options = options
      if (this.isConnected) this.#mount()
    }
    get options(): SheetOptions {
      if (this.#options === null) throw new Error("the sheet has no options yet")
      return this.#options
    }
    connectedCallback() {
      if (this.#options && !this.#mounted) this.#mount()
    }
    disconnectedCallback() {
      this.#mounted?.driver.dispose()
      this.#mounted = null
    }
    get snapshot(): Snapshot | null {
      return this.#mounted?.driver.snapshot() ?? null
    }
    get ready(): boolean {
      return this.querySelector(".norn-sheet")?.getAttribute("data-engine") === "ready"
    }
    request(nodeId: string): boolean {
      return this.#mounted?.raise(nodeId) ?? false
    }
    choose(optionId: string): boolean {
      return this.#mounted?.choose(optionId) ?? false
    }
    setNow(ms: number): void {
      this.#mounted?.setNow(ms)
    }
    setContext(next: { readonly technical?: boolean; readonly caller?: string; readonly laws?: Readonly<Record<string, SheetLawInfo>> }): void {
      this.#mounted?.setContext(next)
    }
    #mount() {
      this.#mounted?.driver.dispose()
      this.#mounted = this.#options ? mountDecisionSheet(this, this.#options) : null
    }
  }
  customElements.define(name, Element)
}
