// `<norn-decision-sheet>`: one decision, rendered from the admitted page document
// (`decision-sheet.widget.json`) and driven only by its declared machine (UI-MACHINE-1).
//
// The element is framework-free. It draws UI SDK parts (`ui-radio-group`, `ui-textarea`) whose
// behaviour the host registers by importing `@ui-sdk/web/<part>`; the element never fetches by
// itself, never decides permission, and offers a control only when the machine's current state
// takes one of its events. What it shows about the decision and its outcome is the server's own
// document (the question, options, receipt, resolution, refusal), as sent.

import { enabled, eventsOf } from "../machine/controls.ts"
import { type Driver, type Transport, createDriver } from "../machine/driver.ts"
import type { Evaluate, Fields, Machine, Msg, Snapshot } from "../machine/machine.ts"
import { adoptSheetStyles } from "./sheet.css.ts"

export type Locale = "en" | "ar"
type Text = Readonly<Record<string, string>>

/** The slice of the decisions contract (`norn.decision@1`) the sheet shows. */
export interface SheetDecision {
  readonly id: string
  readonly question: string
  readonly risk: "low" | "medium" | "high"
  readonly run: { readonly id: string }
  readonly flow: { readonly title: string | null; readonly ruleId: string }
  readonly step: { readonly id: string; readonly label: string | null }
  readonly options: readonly { readonly id: string; readonly label: string; readonly scope: string; readonly effect: { readonly action: string } | { readonly binding: string } }[]
  readonly agentPick: string | null
  readonly expiresAtMs: number
}

interface WidgetNodeDoc {
  readonly nodeId: string
  readonly component: { readonly kind: string; readonly label?: Text; readonly section?: { readonly label: Text } }
  readonly children: readonly WidgetNodeDoc[]
}

/** The admitted widget artifact this element renders: one page with a machine. */
export interface SheetDocument {
  readonly payload: { readonly ux: { readonly pages: readonly { readonly root: WidgetNodeDoc; readonly machine: Machine }[] } }
}

export interface SheetOptions {
  readonly document: SheetDocument
  readonly decision: SheetDecision
  readonly evaluate: Evaluate
  readonly transport: Transport
  readonly locale?: Locale
  /** The cell's clock now (for the countdown); the host keeps it current with `setNow`. */
  readonly now?: number
  /** The machine reached a terminal state. */
  readonly onSettled?: (state: string) => void
  /** The person is done with the sheet (Back to inbox, or the machine closed). */
  readonly onClose?: () => void
  /** Follow an in-app link (the run). */
  readonly navigate?: (path: string) => void
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
    failed: "The cell did not answer; nothing was decided.", openRun: "Open run", seeRun: "See the run go on", back: "Back to inbox",
    decidedAs: "Decided: {0} · {1}", byYou: "by you", byOther: "by {0}", replayed: " (already recorded; the cell returned the first receipt)",
    details: "Details", receipt: "Receipt", commit: "Commit", recorded: "Recorded", decision: "Decision",
    resolvedDecided: "Decided by {0} at {1}", resolvedChoice: ": {0}", anotherDecider: "another decider", resolvedBody: "Someone else got there first. Nothing you chose was sent.",
    withdrawn: "Withdrawn at {0}", withdrawnBody: "The run no longer needs this decision.", expiredAt: "Expired at {0}", expiredBody: "The deadline passed before anyone decided.",
    expired: "Expired", gone: "No longer here", awaiting: "Waiting for approval", awaitingBody: "Your choice waits for an approval before it takes effect.",
    leaveBody: "Nothing has been sent. Your choice and note will be cleared; the decision stays in your inbox.", left: "{0} left", lapsed: "Expired",
  },
  ar: {
    "risk.low": "مخاطرة منخفضة", "risk.medium": "مخاطرة متوسطة", "risk.high": "مخاطرة عالية", suggested: "مقترح", suggestedTitle: "توصية الوكيل؛ والقرار لك.",
    starts: "يبدأ إجراءً", records: "يسجّل الاختيار", optional: "(اختياري)", notePlaceholder: "السبب، لمن يقرأ التشغيل لاحقًا",
    choose: "اختر خيارًا", decide: "قرّر", decideAs: "قرّر: {0}", confirmAs: "نعم، {0}", sending: "جارٍ الإرسال…",
    "confirm.high": "هذا قرار عالي المخاطرة.", "confirm.action": "هذا يبدأ إجراءً.",
    "confirm.body": "تختار «{0}» ({1}). يتابع التشغيل به فور تسجيل الخلية له.",
    failed: "لم تُجب الخلية؛ لم يُتخذ أي قرار.", openRun: "فتح التشغيل", seeRun: "تابع التشغيل", back: "العودة إلى الصندوق",
    decidedAs: "تقرّر: {0} · {1}", byYou: "بواسطتك", byOther: "بواسطة {0}", replayed: " (مسجّل من قبل؛ أعادت الخلية الإيصال الأول)",
    details: "التفاصيل", receipt: "الإيصال", commit: "الالتزام", recorded: "سُجّل", decision: "القرار",
    resolvedDecided: "قرّر {0} عند {1}", resolvedChoice: ": {0}", anotherDecider: "صاحب قرار آخر", resolvedBody: "سبقك شخص آخر. لم يُرسل شيء مما اخترته.",
    withdrawn: "سُحب عند {0}", withdrawnBody: "لم يعد التشغيل بحاجة إلى هذا القرار.", expiredAt: "انتهت المهلة عند {0}", expiredBody: "انقضت المهلة قبل أن يقرّر أحد.",
    expired: "انتهت المهلة", gone: "لم يعد هنا", awaiting: "بانتظار الموافقة", awaitingBody: "اختيارك بانتظار موافقة قبل أن يسري.",
    leaveBody: "لم يُرسل شيء. سيُمسح اختيارك وملاحظتك؛ ويبقى القرار في صندوقك.", left: "متبقٍ {0}", lapsed: "انتهت المهلة",
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

/** What is left before the deadline, in the two largest units; null once it has passed. */
export function remaining(expiresAtMs: number, now: number): string | null {
  const ms = expiresAtMs - now
  if (ms <= 0) return null
  const s = Math.floor(ms / 1000)
  const d = Math.floor(s / 86_400)
  const h = Math.floor((s % 86_400) / 3600)
  const m = Math.floor((s % 3600) / 60)
  if (d > 0) return [`${d} ${d === 1 ? "day" : "days"}`, h > 0 ? `${h} h` : ""].filter(Boolean).join(" ")
  if (h > 0) return [`${h} h`, m > 0 ? `${m} min` : ""].filter(Boolean).join(" ")
  return m > 0 ? `${m} min` : `${s} s`
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
  const options = decision.options
    .map(
      (option) => `
      <label class="norn-option" data-slot="label">
        <ui-radio value="${escape(option.id)}" aria-label="${escape(option.label)}"><ui-radio-indicator></ui-radio-indicator></ui-radio>
        <span class="norn-option-text">
          <span class="norn-option-label">${escape(option.label)}${option.id === decision.agentPick ? ` <span class="norn-chip" data-tone="agent" title="${escape(w.suggestedTitle)}">${escape(w.suggested)}</span>` : ""}</span>
          <span class="norn-meta">${escape(option.scope)}</span>
          <span class="norn-meta">${escape("action" in option.effect ? w.starts : w.records)}</span>
        </span>
      </label>`,
    )
    .join("")
  return `
    <section class="norn-sheet" dir="${locale === "ar" ? "rtl" : "ltr"}" lang="${locale}" aria-label="${escape(label(byId.get("decision-sheet"), locale))}">
      <header class="norn-sheet-header">
        <h2 class="norn-title" data-region="question">${escape(decision.question)}</h2>
        <p class="norn-meta">${escape(decision.flow.title ?? decision.flow.ruleId)} · ${escape(decision.step.label ?? decision.step.id)}</p>
        <div class="norn-actions">
          <span class="norn-chip" data-tone="risk-${decision.risk}">${escape(w[`risk.${decision.risk}`])}</span>
          <span class="norn-meta" data-region="countdown"></span>
          <span class="norn-spacer"></span>
          <a class="norn-link" href="/runs/${encodeURIComponent(decision.run.id)}" data-link>${escape(w.openRun)}</a>
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
        <div class="norn-confirm" role="alertdialog" aria-labelledby="norn-confirm-${id}" data-region="confirm" hidden>
          <p class="norn-confirm-title" id="norn-confirm-${id}">${escape(decision.risk === "high" ? w["confirm.high"] : w["confirm.action"])}</p>
          <p class="norn-meta" data-region="confirm-body"></p>
        </div>
        <div class="norn-confirm" role="alertdialog" aria-labelledby="norn-leave-${id}" data-region="decision-leave" hidden>
          <p class="norn-confirm-title" id="norn-leave-${id}">${escape(label(byId.get("decision-leave"), locale))}</p>
          <p class="norn-meta">${escape(w.leaveBody)}</p>
          <div class="norn-actions"><span class="norn-spacer"></span>${button("stay-button", "ghost")}${button("leave-button", "outline")}</div>
        </div>
        <footer class="norn-actions norn-footer" data-region="decision-actions">
          <span class="norn-meta" data-region="hint"></span>
          ${button("close-button", "ghost")}<span class="norn-spacer"></span>${button("back-button", "ghost")}${button("retry-button", "outline")}${button("confirm-button", "danger")}${button("decide-button", "solid")}
        </footer>
      </form>
      <section class="norn-outcome-card" data-region="decision-outcome" hidden>
        <p class="norn-outcome" role="status" aria-live="polite" data-region="outcome-title"></p>
        <p class="norn-meta" data-region="outcome-body"></p>
        <a class="norn-link" data-region="outcome-run" data-link hidden>${escape(w.seeRun)}</a>
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

/**
 * Mount one decision into `root` and run its machine. Returns the driver so a host can dispose it.
 * Every control's presence and enabled state comes from the machine via `enabled`.
 */
export function mountDecisionSheet(root: HTMLElement, options: SheetOptions): Driver {
  const { document, decision, evaluate } = options
  const locale = options.locale ?? "en"
  const w = WORDS[locale]
  const page = document.payload.ux.pages[0]
  if (!page) throw new Error("the sheet document has no page")
  const machine = page.machine
  adoptSheetStyles(root.ownerDocument)
  root.innerHTML = sheetMarkup(document, decision, locale)
  let lastBody: unknown
  let now = options.now
  const transport: Transport = {
    async call(call, input, callOptions) {
      const classified = await options.transport.call(call, input, callOptions)
      lastBody = classified.body
      return classified
    },
  }
  const q = <T extends Element = HTMLElement>(selector: string) => root.querySelector<T>(selector)
  const say = (key: WordKey, ...values: string[]) => fill(w[key], ...values)
  const optionLabel = (choice: unknown) => decision.options.find((option) => option.id === choice)?.label ?? String(choice ?? "")

  const paintCountdown = (settled: boolean) => {
    const countdown = q('[data-region="countdown"]')
    if (!countdown) return
    const left = now === undefined || settled ? undefined : remaining(decision.expiresAtMs, now)
    countdown.textContent = left === undefined ? "" : left === null ? w.lapsed : say("left", left)
    countdown.toggleAttribute("data-urgent", now !== undefined && decision.expiresAtMs - now < 3_600_000)
  }

  const paintOutcome = (state: string) => {
    const card = q('[data-region="decision-outcome"]')
    const title = q('[data-region="outcome-title"]')
    const body = q('[data-region="outcome-body"]')
    const run = q<HTMLAnchorElement>('[data-region="outcome-run"]')
    const details = q('[data-region="outcome-details"]')
    if (!card || !title || !body || !run || !details) return
    const doc = record(lastBody)
    run.hidden = true
    details.hidden = true
    let heading = ""
    let words = ""
    if (state === "decided") {
      const decided = record(doc.decision)
      const by = text(decided.decidedBy)
      heading = say("decidedAs", optionLabel(decided.choice), by === null || by === "you" ? w.byYou : say("byOther", by)) + (doc.outcome === "replayed" ? w.replayed : "")
      words = text(decided.note) ? `“${text(decided.note)}”` : ""
      const runId = text(record(doc.run).id)
      if (runId) {
        run.href = `/runs/${encodeURIComponent(runId)}`
        run.hidden = false
      }
      const receipt = record(doc.receipt)
      const rows: [string, string][] = [
        [w.receipt, text(receipt.receiptHash) ?? ""],
        [w.commit, String(receipt.commitSeq ?? "")],
        [w.recorded, typeof receipt.recordedAtMs === "number" ? new Date(receipt.recordedAtMs).toLocaleString(locale, { dateStyle: "medium", timeStyle: "medium" }) : ""],
        [w.decision, decision.id],
      ]
      const dl = details.querySelector("dl")
      if (dl) dl.innerHTML = rows.map(([term, value]) => `<dt>${escape(term)}</dt><dd><code>${escape(value)}</code></dd>`).join("")
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
    q(".norn-sheet")?.setAttribute("data-state", state)
    // A terminal machine shows only its outcome; otherwise the form stays, with what it offers now.
    q('[data-region="form"]')?.toggleAttribute("hidden", settled)
    q('[data-host="back"]')?.toggleAttribute("hidden", !settled)
    paintCountdown(settled)
    paintOutcome(state)
    const choice = snapshot.fields.choice
    for (const element of root.querySelectorAll<HTMLButtonElement>("button[data-node]")) {
      const node = element.dataset.node ?? ""
      const on = enabled(machine, evaluate, snapshot, node)
      // Decide stays in view while a choice is still to be made, so the form says what it waits for.
      element.hidden = !on && !(node === "decide-button" && (state === "choosing" || state === "sending"))
      element.disabled = !on
    }
    const decideButton = q<HTMLButtonElement>('[data-node="decide-button"]')
    if (decideButton) decideButton.textContent = state === "sending" ? w.sending : choice ? say("decideAs", optionLabel(choice)) : w.decide
    const confirmButton = q<HTMLButtonElement>('[data-node="confirm-button"]')
    if (confirmButton) confirmButton.textContent = say("confirmAs", optionLabel(choice).toLowerCase())
    const hint = q('[data-region="hint"]')
    if (hint) hint.textContent = choice ? "" : w.choose
    const confirmBody = q('[data-region="confirm-body"]')
    if (confirmBody) confirmBody.textContent = say("confirm.body", optionLabel(choice), decision.options.find((o) => o.id === choice)?.scope ?? "")
    q('[data-region="confirm"]')?.toggleAttribute("hidden", state !== "confirming")
    q('[data-region="decision-leave"]')?.toggleAttribute("hidden", state !== "leaving")
    const editable = enabled(machine, evaluate, snapshot, "option-choice")
    // Choices and the note show while they can change, and while a call is in flight (read-only).
    for (const region of ["decision-choices", "decision-note"]) q(`[data-region="${region}"]`)?.toggleAttribute("hidden", !editable && state !== "sending")
    q("textarea")?.toggleAttribute("disabled", !editable)
    q('[data-node="option-choice"]')?.toggleAttribute("aria-disabled", !editable)
    // The server's words for a refusal: the top-level message and unpointed diagnostics at the top,
    // pointed diagnostics at their field.
    const found = state === "refused" ? problems(lastBody) : []
    const top = record(lastBody)
    const status = q('[data-region="status"]')
    if (status) {
      const message = state === "refused" ? text(top.message) ?? String(snapshot.fields.lastCode ?? "") : state === "failed" ? w.failed : ""
      const extra = found.filter((p) => p.pointer !== "/choice" && p.pointer !== "/note" && p.message !== message).map((p) => p.message)
      status.textContent = [message, ...extra].filter(Boolean).join(" ")
      status.hidden = status.textContent === ""
    }
    for (const problem of root.querySelectorAll<HTMLElement>("[data-problem]")) {
      const words = found.filter((p) => p.pointer === problem.dataset.problem).map((p) => p.message).join(" ")
      problem.textContent = words
      problem.hidden = words === ""
    }
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

  /** Raise the first of the node's events the current state takes. */
  const raise = (nodeId: string, payload?: Fields) => {
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
  ;(root as HTMLElement & { raise?: typeof raise; setNow?: (ms: number) => void }).raise = raise
  ;(root as HTMLElement & { setNow?: (ms: number) => void }).setNow = (ms: number) => {
    now = ms
    paint(driver.snapshot())
  }
  paint(driver.snapshot())
  return driver
}

/** `<norn-decision-sheet>`: set `options` to mount; removing the element disposes the machine. */
export interface NornDecisionSheet extends HTMLElement {
  options: SheetOptions
  /** The machine's current snapshot, for hosts and tests. */
  readonly snapshot: Snapshot | null
  /** Raise the first event of `nodeId` the current state takes (a host's Escape raises `close-button`). */
  request(nodeId: string): boolean
}

/**
 * Register `<norn-decision-sheet>` once. The class is made here, not at import, so the module loads
 * where there is no DOM (tests, server rendering).
 */
export function defineDecisionSheet(name = "norn-decision-sheet"): void {
  if (customElements.get(name)) return
  class Element extends HTMLElement implements NornDecisionSheet {
    #driver: Driver | null = null
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
      if (this.#options && !this.#driver) this.#mount()
    }
    disconnectedCallback() {
      this.#driver?.dispose()
      this.#driver = null
    }
    get snapshot(): Snapshot | null {
      return this.#driver?.snapshot() ?? null
    }
    request(nodeId: string): boolean {
      return (this as unknown as { raise?: (node: string) => boolean }).raise?.(nodeId) ?? false
    }
    #mount() {
      this.#driver?.dispose()
      this.#driver = this.#options ? mountDecisionSheet(this, this.#options) : null
    }
  }
  customElements.define(name, Element)
}
