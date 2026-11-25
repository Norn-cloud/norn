/**
 * Studio's translations of source messages (UI-TEXT-1, #1762): the lookup `<NornAppRenderer>`
 * draws a page's `msg` text through (`translate`), and Studio's own islands through `useMessage`.
 *
 * A translation is keyed by the message's id (`messageId`, the hash of its source text and context),
 * so it is the translation of exactly that text in that context: a message whose text changes gets a
 * new id and falls back to its source text until it is translated again. The checked translations
 * artifact replaces this table later; until then it holds the Arabic Studio already showed.
 */
import { messageId } from "../../../../tools/norn-ts/src/builder/message.ts"
import { useLocale, type Locale } from "./locale.tsx"

/** One source message and its translations, by locale. */
interface Translated {
  readonly source: string
  readonly context?: string
  readonly ar: string
}

const TRANSLATED: readonly Translated[] = [
  // The record page's own messages (registry/studio/record-page.ts).
  { source: "Record", ar: "السجل" },
  { source: "Reading the record", ar: "جارٍ قراءة السجل" },
  { source: "The record could not be read", ar: "تعذّرت قراءة السجل" },
  { source: "Record not available", ar: "السجل غير متاح" },
  { source: "The record's name, state and actions", ar: "اسم السجل وحالته وإجراءاته" },
  { source: "The record's facts, story, lineage and rules", ar: "حقائق السجل وقصته ومصدره وقواعده" },
  // The record page's lifecycle panel, translated before messages existed.
  { source: "Lifecycle", ar: "دورة الحياة" },
  { source: "Type-wide view", ar: "العرض الشامل للنوع" },
]

const BY_ID: ReadonlyMap<string, Translated> = new Map(TRANSLATED.map((entry) => [messageId(entry.source, entry.context), entry]))

/** The text of message `id` in `locale`, or `undefined` when Studio has none (its source text is drawn). */
export function translate(id: string, locale: string): string | undefined {
  return locale === "ar" ? BY_ID.get(id)?.ar : undefined
}

/** `source` (in `context`) in the reader's language: its translation, else the source text. */
export function message(source: string, locale: Locale, context?: string): string {
  return translate(messageId(source, context), locale) ?? source
}

/** `say("Lifecycle")`: Studio's own text in the reader's language, through the same translations. */
export function useMessage(): (source: string, context?: string) => string {
  const { locale } = useLocale()
  return (source, context) => message(source, locale, context)
}
