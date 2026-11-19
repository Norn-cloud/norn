import type { Locale } from "./locale.tsx"

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
    en: "This question asks for a number the cell doesn't define (it may have been renamed or removed).",
    ar: "يطلب هذا السؤال رقمًا غير معرَّف في الخلية (ربما أُعيدت تسميته أو حُذف).",
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
}

/** The plain words for `code` in `locale`, or undefined when Studio has none. */
export function plainWords(code: string, locale: Locale): string | undefined {
  return PLAIN[code]?.[locale]
}

