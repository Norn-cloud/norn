/**
 * MESSAGE-LOCALE-1: the server sends a stable code and its English message;
 * Studio says it in the reader's language from the catalogs in
 * `crates/norn-checker/conformance/messages` (MessageFormat 2, the simple
 * subset the Rust conformance test admits: text with `\\`, `\{`, `\}`
 * escapes and `{$name}` variables). The English message's values are read
 * back through the code's English template, so a translation never invents
 * or drops a value; when the message does not match its template, the
 * English text is shown as it came, never a guess.
 */
import arabic from "../../../../crates/norn-checker/conformance/messages/ar.json"
import english from "../../../../crates/norn-checker/conformance/messages/en.json"
import type { Locale } from "./locale.tsx"

interface Catalog { readonly messages: Readonly<Record<string, { readonly message: string; readonly review?: string }>> }

const CATALOGS: Readonly<Record<Locale, Catalog>> = { en: english as Catalog, ar: arabic as Catalog }

export type Part = { readonly text: string } | { readonly variable: string }

/** Parse a simple MessageFormat 2 message; anything outside the subset is refused. */
export function parseMessage(message: string): readonly Part[] {
  const parts: Part[] = []
  let text = ""
  for (let index = 0; index < message.length; index += 1) {
    const char = message[index]!
    if (char === "\\") {
      const next = message[index + 1]
      if (next !== "\\" && next !== "{" && next !== "}") throw new Error(`invalid escape at ${index}`)
      text += next
      index += 1
    } else if (char === "{") {
      const close = message.indexOf("}", index)
      const name = close < 0 ? undefined : /^\$([A-Za-z][A-Za-z0-9_]*)$/.exec(message.slice(index + 1, close))?.[1]
      if (name === undefined) throw new Error(`unsupported expression at ${index}`)
      if (text) parts.push({ text })
      text = ""
      parts.push({ variable: name })
      index = close
    } else if (char === "}") {
      throw new Error(`unescaped } at ${index}`)
    } else text += char
  }
  if (text) parts.push({ text })
  return parts
}

const escapeRegExp = (text: string) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")

/**
 * The values an English message carries, read through its template. Undefined when the message does
 * not match, or when it matches in more than one way: a value that contains the template's next
 * literal (`fk: line→order` before `: `) could be split at either place, and a wrong split would put
 * the pieces in the wrong slots of the translation. The parse is unique exactly when the shortest-first
 * and the longest-first matches agree on every value, so only then is it accepted.
 */
export function readValues(template: string, message: string): Readonly<Record<string, string>> | undefined {
  const parts = parseMessage(template)
  const names = [...new Set(parts.flatMap((part) => "variable" in part ? [part.variable] : []))]
  const read = (capture: string): readonly string[] | undefined => {
    const seen: string[] = []
    const pattern = parts.map((part) => {
      if ("text" in part) return escapeRegExp(part.text)
      const group = seen.indexOf(part.variable)
      if (group >= 0) return `\\${group + 1}`
      seen.push(part.variable)
      return capture
    }).join("")
    const match = new RegExp(`^${pattern}$`).exec(message)
    return match ? names.map((_, index) => match[index + 1] ?? "") : undefined
  }
  const shortest = read("([\\s\\S]*?)")
  const longest = read("([\\s\\S]*)")
  if (shortest === undefined || longest === undefined || shortest.some((value, index) => value !== longest[index])) return undefined
  return Object.fromEntries(names.map((name, index) => [name, shortest[index] ?? ""]))
}

/**
 * First-strong isolate (FSI … PDI) around a value, so an id, code or number keeps its own direction
 * inside Arabic text. Isolate controls inside the value (U+2066–2069) are removed first: a PDI in a
 * server value would close the isolate early and let the rest reorder the sentence.
 */
export const isolate = (value: string) => `\u2068${value.replace(/[\u2066-\u2069]/g, "")}\u2069`

/** Format a catalog message with values; every value is isolated. */
export function formatMessage(message: string, values: Readonly<Record<string, string>>): string {
  return parseMessage(message).map((part) => "text" in part ? part.text : isolate(values[part.variable] ?? "")).join("")
}

export interface LocalizedMessage {
  readonly text: string
  /** The language the text is in: the reader's, or `en` when it fell back to the server's own message. */
  readonly lang: Locale
  readonly needsReview: boolean
}

/** Say a coded server message in `locale`; the code itself never changes. */
export function localizeMessage(code: string, message: string, locale: Locale): LocalizedMessage {
  if (locale === "en") return { text: message, lang: "en", needsReview: false }
  const template = CATALOGS.en.messages[code]?.message
  const target = CATALOGS[locale].messages[code]
  const values = template === undefined ? undefined : readValues(template, message)
  if (target === undefined || values === undefined) return { text: message, lang: "en", needsReview: false }
  return { text: formatMessage(target.message, values), lang: locale, needsReview: target.review === "needs_review" }
}
