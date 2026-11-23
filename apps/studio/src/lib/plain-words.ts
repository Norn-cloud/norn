import type { Locale } from "./locale.tsx"
import type { View } from "./view.tsx"
import { conceptTitle } from "./catalog.ts"
import { CellUnavailable } from "./definitions.ts"
import { CellRefusal } from "./live-transport.ts"

/**
 * What a refusal means for the reader, in plain words, by its stable code: what went wrong and
 * what to do next. The engine's own text (and code) stays under Technical; a code with no entry
 * here falls back to the message catalog's words for it.
 */
const PLAIN: Readonly<Record<string, Readonly<Record<Locale, string>>>> = {
  "metric.validation.invalid_currency": {
    en: "This number is money, but its record type doesn't say which currency each amount is in, so the amounts can't be added up safely.",
    ar: "هذا الرقم مبلغ مالي، لكن نوع السجل لا يحدد عملة كل مبلغ، لذلك لا يمكن جمع المبالغ بأمان.",
  },
  "metric.validation.mixed_currency": {
    en: "These amounts are in more than one currency. Break the answer out by currency, or pick one currency, to add them up.",
    ar: "هذه المبالغ بأكثر من عملة. قسّم الإجابة حسب العملة، أو اختر عملة واحدة، لجمعها.",
  },
  "metric.validation.mixed_unit": {
    en: "These quantities are in more than one unit (grams and kilograms, say). Break the answer out by unit, or pick one unit, to add them up.",
    ar: "هذه الكميات بأكثر من وحدة (جرامات وكيلوجرامات مثلًا). قسّم الإجابة حسب الوحدة، أو اختر وحدة واحدة، لجمعها.",
  },
  "metric.validation.undeclared_additivity": {
    en: "Nobody has said whether this number can be added up, so it isn't summed. Declare it as a total or a balance in its definition.",
    ar: "لم يُحدَّد ما إذا كان هذا الرقم قابلًا للجمع، لذلك لا يُجمع. عرّفه كإجمالي أو كرصيد في تعريفه.",
  },
  "metric.validation.ratio_aggregation": {
    en: "A ratio can't be averaged. Add up its top and bottom separately, then divide.",
    ar: "لا يمكن حساب متوسط النسبة. اجمع البسط والمقام كلًّا على حدة، ثم اقسم.",
  },
  "metric.validation.invalid_time": {
    en: "This number has no single date to place it in time, so it can't be shown by week or month.",
    ar: "لا يوجد لهذا الرقم تاريخ واحد يضعه في الزمن، لذلك لا يمكن عرضه حسب الأسبوع أو الشهر.",
  },
  "metric.validation.unknown_metric": {
    en: "This question asks for a number the workspace doesn't define (it may have been renamed or removed).",
    ar: "يطلب هذا السؤال رقمًا غير معرَّف في مساحة العمل (ربما أُعيدت تسميته أو حُذف).",
  },
  "question.validation.non_additive_rollup": {
    en: "This number can't be added up across groups, so there's no overall total.",
    ar: "لا يمكن جمع هذا الرقم عبر المجموعات، لذلك لا يوجد إجمالي عام.",
  },
  "question.validation.semi_additive_time": {
    en: "This is a balance: it's read at the end of each period, not added up over time.",
    ar: "هذا رصيد: يُقرأ في نهاية كل فترة، ولا يُجمع عبر الزمن.",
  },
  "question.validation.invalid_window": {
    en: "The time range is empty or backwards. Pick a start before the end.",
    ar: "النطاق الزمني فارغ أو معكوس. اختر بداية قبل النهاية.",
  },
  "cell.unopenable": {
    en: "This workspace couldn't be opened right now. Ask your workspace administrator or support to check it.",
    ar: "تعذّر فتح مساحة العمل الآن. اطلب من مسؤول مساحة العمل أو الدعم التحقق منها.",
  },
  "cell.not_provisioned": {
    en: "This workspace hasn't been set up with a data cell yet.",
    ar: "لم يتم إعداد خلية بيانات لمساحة العمل هذه بعد.",
  },
  "cell.timeout": {
    en: "The workspace took too long to respond. Try again in a moment.",
    ar: "استغرقت مساحة العمل وقتًا طويلًا للاستجابة. حاول مجددًا بعد قليل.",
  },
  "authz.denied": {
    en: "Your role doesn't have permission for this.",
    ar: "دورك لا يملك صلاحية لذلك.",
  },
  "scope.denied": {
    en: "This request doesn't belong to the active workspace.",
    ar: "هذا الطلب لا ينتمي إلى مساحة العمل النشطة.",
  },
  "core.laws.violated": {
    en: "This change breaks a rule, so nothing was saved.",
    ar: "هذا التغيير يخالف قاعدة، لذلك لم يُحفظ شيء.",
  },
  "decision.law_not_satisfied": {
    en: "A rule blocks this choice for your role.",
    ar: "تمنع قاعدة هذا الخيار لدورك.",
  },
  "decision.guard_false": {
    en: "This step isn't open for this record right now.",
    ar: "هذه الخطوة غير متاحة لهذا السجل حاليًا.",
  },
  "action.precondition_failed": {
    en: "A required condition isn't met, so nothing was saved.",
    ar: "لم يتحقق شرط مطلوب، لذلك لم يُحفظ شيء.",
  },
  "action.target_not_found": {
    en: "The record you're trying to change no longer exists.",
    ar: "السجل الذي تحاول تغييره لم يعد موجودًا.",
  },
  "action.slice_object_missing": {
    en: "A related record needed for this change no longer exists.",
    ar: "لم يعد هناك سجل مرتبط مطلوب لهذا التغيير.",
  },
  "action.invalid_input": {
    en: "Some values don't fit what this action expects.",
    ar: "بعض القيم لا تطابق ما يتوقعه هذا الإجراء.",
  },
  "action.idempotency_conflict": {
    en: "This request was already used for a different change.",
    ar: "استُخدم هذا الطلب بالفعل لتغيير مختلف.",
  },
  "action.receipts_unconfigured": {
    en: "This workspace isn't configured to record changes yet.",
    ar: "مساحة العمل هذه غير مهيّأة لتسجيل التغييرات بعد.",
  },
  "defs.stale": {
    en: "Workspace definitions changed while you were working. Refresh and try again.",
    ar: "تغيّرت تعريفات مساحة العمل أثناء عملك. حدّث الصفحة وحاول مجددًا.",
  },
  "dataset.not_loaded": {
    en: "This dataset couldn't be loaded.",
    ar: "تعذّر تحميل مجموعة البيانات هذه.",
  },
  "dataset.uploads_unconfigured": {
    en: "This workspace has no file storage configured for uploads yet.",
    ar: "لا يوجد تخزين ملفات مهيّأ للرفع في مساحة العمل هذه بعد.",
  },
  "route.not_found": {
    en: "This feature isn't available on this workspace yet.",
    ar: "هذه الميزة غير متاحة في مساحة العمل هذه بعد.",
  },
  "host.internal": {
    en: "The workspace hit an internal problem and couldn't finish this request.",
    ar: "واجهت مساحة العمل مشكلة داخلية وتعذّر إكمال هذا الطلب.",
  },
}

/** The plain words for `code` in `locale`, or undefined when Studio has none. */
export function plainWords(code: string, locale: Locale): string | undefined {
  return PLAIN[code]?.[locale]
}

const cap = (text: string) => text.length === 0 ? text : text.charAt(0).toUpperCase() + text.slice(1)
const spaced = (id: string) => id.replaceAll("_", " ")

/**
 * Rewrite a derived metric's formula/proof label into plain words for Business view.
 * Raw formulas (`sum(a) / sum(b); sum(c); conservation [...]`) stay under Technical view.
 */
export function plainMetricLabel(label: string): string {
  // Declared ratio from derive.rs: "sum(num) / sum(den); sum(prop); conservation [...]"
  const declaredRatio = /^sum\(([^)]+)\)\s*\/\s*sum\(([^)]+)\);\s*sum\(([^)]+)\)(?:;\s*conservation\s*\[[^\]]*\])?$/i.exec(label.trim())
  if (declaredRatio) {
    const [, num = "", den = "", prop = ""] = declaredRatio
    return `${cap(spaced(prop))} (${spaced(num)} ÷ ${spaced(den)})`
  }
  // Conserved ratio from derive.rs: "sum(num) / sum(den); law ...@..."
  const conservedRatio = /^sum\(([^)]+)\)\s*\/\s*sum\(([^)]+)\)(?:;\s*(?:law|conservation)\b.*)?$/i.exec(label.trim())
  if (conservedRatio) {
    const [, num = "", den = ""] = conservedRatio
    return `${cap(spaced(num))} ÷ ${spaced(den)}`
  }
  // Simple sum from derive.rs: "sum(prop); conservation [...]"
  const simpleSum = /^sum\(([^)]+)\)(?:;\s*conservation\s*\[[^\]]*\])?$/i.exec(label.trim())
  if (simpleSum) {
    const [, prop = ""] = simpleSum
    return `Total ${spaced(prop)}`
  }
  // Snapshot from derive.rs: "prop, as of time; last value per ns.Type at each period's end"
  const snap = /^([a-z0-9_]+),\s*as of\s+([a-z0-9_]+);\s*last value per\s+([^\s]+)\s+at each period's end$/i.exec(label.trim())
  if (snap) {
    const [, prop = "", time = "", type = ""] = snap
    return `${cap(spaced(prop))} at period end (as of ${spaced(time)}, per ${conceptTitle(type)})`
  }
  return label
}

/**
 * Rewrite derived-metric formula/proof fragments inside a restatement string into plain words for
 * Business mode (e.g. `sum(a) / sum(b); sum(c); conservation [] (a ÷ b)` → `C (a ÷ b)`).
 * Technical view keeps the raw server restatement verbatim.
 */
export function plainRestatement(text: string): string {
  // 1) Declared ratio already followed by its "(a ÷ b)" clause: avoid repeating "(a ÷ b)" twice.
  let out = text.replace(
    /sum\(([^)]+)\)\s*\/\s*sum\(([^)]+)\);\s*sum\(([^)]+)\)(?:;\s*conservation\s*\[[^\]]*\])?(\s*\([^)]*÷[^)]*\))/gi,
    (_m, _num: string, _den: string, prop: string, tail: string) => `${cap(spaced(prop))}${tail}`,
  )
  // 2) Declared ratio without a trailing "(a ÷ b)" clause.
  out = out.replace(
    /sum\(([^)]+)\)\s*\/\s*sum\(([^)]+)\);\s*sum\(([^)]+)\)(?:;\s*conservation\s*\[[^\]]*\])?/gi,
    (_m, num: string, den: string, prop: string) => `${cap(spaced(prop))} (${spaced(num)} ÷ ${spaced(den)})`,
  )
  // 3) Conserved ratio with law digest.
  out = out.replace(
    /sum\(([^)]+)\)\s*\/\s*sum\(([^)]+)\);\s*law\s+[^\s(]+/gi,
    (_m, num: string, den: string) => `${cap(spaced(num))} ÷ ${spaced(den)}`,
  )
  // 4) Simple derived sum with conservation proof.
  out = out.replace(
    /sum\(([^)]+)\);\s*conservation\s*\[[^\]]*\]/gi,
    (_m, prop: string) => `Total ${spaced(prop)}`,
  )
  // 5) Snapshot label with raw object type id.
  out = out.replace(
    /\b([a-z0-9_]+),\s*as of\s+([a-z0-9_]+);\s*last value per\s+((?:[a-z][a-z0-9_]*\.)+[A-Z][A-Za-z0-9]*)\s+at each period's end/gi,
    (_m, prop: string, time: string, type: string) => `${cap(spaced(prop))} at period end (as of ${spaced(time)}, per ${conceptTitle(type)})`,
  )
  return out
}

/** Patterns that betray an internal runtime/worker message rather than user-facing copy. */
const INTERNAL_NOISE: readonly { readonly pattern: RegExp; readonly plain: string }[] = [
  { pattern: /worker failed:\s*handler is waiting on nothing/i, plain: "The workspace worker stopped before finishing this request. Try again in a moment." },
  { pattern: /analytics engine call is pending/i, plain: "The analytics engine is busy with another query. Try again in a moment." },
  { pattern: /^failed to fetch$/i, plain: "Couldn't reach the workspace. Check your connection and try again." },
  { pattern: /upstream reset/i, plain: "The connection to the workspace dropped. Try again." },
  { pattern: /norn dev at https?:\/\/[^\s]+ did not answer/i, plain: "The local workspace server isn't responding. Make sure it is running and try again." },
]

export interface ExplainedError {
  readonly headline: string
  readonly detail: string | null
  readonly code: string | null
  readonly status: number | null
  readonly incidentId: string | null
  readonly retryable: boolean | undefined
  readonly timedOut: boolean
  readonly raw: string | null
}

/**
 * Turn any caught read/refusal error into honest, plain copy for Business mode while keeping exact
 * codes, explanations, incident ids, and raw internal strings available under Technical mode.
 */
export function explainError(error: unknown, locale: Locale = "en", view: View = "business"): ExplainedError {
  const code = error instanceof CellUnavailable || error instanceof CellRefusal ? (error.code ?? null) : null
  const status = error instanceof CellUnavailable || error instanceof CellRefusal ? error.status : null
  const explanation = error instanceof CellUnavailable || error instanceof CellRefusal ? (error.explanation ?? null) : null
  const incidentId = error instanceof CellUnavailable || error instanceof CellRefusal ? (error.incidentId ?? null) : null
  const retryable = error instanceof CellUnavailable || error instanceof CellRefusal ? error.retryable : undefined
  const timedOut = error instanceof CellUnavailable && error.timedOut === true
  const rawMessage = error instanceof Error ? error.message : String(error)

  const sanitizedNoise = INTERNAL_NOISE.find(({ pattern }) => pattern.test(rawMessage) || (explanation !== null && pattern.test(explanation)))
  const mapped = code ? plainWords(code, locale) : undefined

  if (view === "technical") {
    return {
      headline: rawMessage,
      detail: explanation && explanation !== rawMessage ? explanation : null,
      code,
      status,
      incidentId,
      retryable,
      timedOut,
      raw: null,
    }
  }

  const headline = mapped
    ?? (sanitizedNoise ? sanitizedNoise.plain : rawMessage)
  // Always surface server explanation unless it's pure internal worker noise (which goes to `raw`) or duplicates headline.
  const explanationIsNoise = explanation !== null && INTERNAL_NOISE.some(({ pattern }) => pattern.test(explanation))
  const detail = explanation && !explanationIsNoise && explanation !== headline
    ? explanation
    : (!mapped && sanitizedNoise === undefined ? null : (rawMessage !== headline && !INTERNAL_NOISE.some(({ pattern }) => pattern.test(rawMessage)) ? rawMessage : null))

  const raw = rawMessage !== headline || explanationIsNoise
    ? [rawMessage, explanationIsNoise ? explanation : null].filter(Boolean).join(" — ")
    : null

  return { headline, detail, code, status, incidentId, retryable, timedOut, raw }
}

