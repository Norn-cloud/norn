import type { Field } from "./catalog.ts"
import { member } from "./exact.ts"
import type { ObjectRecord } from "./transport.ts"

/** A fact's value as the server sent it: decimals keep their exact text, with their declared currency. */
export function factText(field: Field, record: ObjectRecord, numbers: ReadonlyMap<string, string>, at: string): string {
  const value = record.values[field.id]
  if (value === null || value === undefined) return "—"
  if (field.decimal) {
    const text = numbers.get(`${at}/values/${member(field.id)}`) ?? (typeof value === "string" ? value : "unavailable")
    const currency = field.decimal.currencyFrom ? record.values[field.decimal.currencyFrom] : undefined
    const unit = field.decimal.unit ?? (typeof currency === "string" ? currency : undefined)
    const per = field.decimal.perUnit ? ` per ${field.decimal.perUnit}` : ""
    return unit ? `${text} ${unit}${per}` : `${text}${per}`
  }
  return typeof value === "string" ? value.replaceAll("_", " ") : JSON.stringify(value)
}

/** The record's own name when its concept declares one, else its identity. */
export const recordName = (record: ObjectRecord): string => {
  const name = record.values["label"] ?? record.values["display_name"]
  return typeof name === "string" ? name : record.id
}

/**
 * Order two exact decimal texts by value without a binary float, so
 * `9007199254740993.000000` sorts after `9007199254740992.000000`.
 * Missing values sort first.
 */
export function compareDecimals(a: string | undefined, b: string | undefined): number {
  if (a === undefined || b === undefined) return a === b ? 0 : a === undefined ? -1 : 1
  // Exponent forms are not decimal text the cell writes; order them by magnitude only.
  if (/e/i.test(a) || /e/i.test(b)) return Math.sign(Number(a) - Number(b))
  const parse = (text: string) => {
    const negative = text.startsWith("-")
    const [whole = "", fraction = ""] = text.replace(/^[-+]/, "").split(".")
    return { negative, whole: whole.replace(/^0+(?=\d)/, ""), fraction: fraction.replace(/0+$/, "") }
  }
  const x = parse(a)
  const y = parse(b)
  const zero = (d: typeof x) => /^0*$/.test(d.whole) && d.fraction === ""
  const xNegative = x.negative && !zero(x)
  const yNegative = y.negative && !zero(y)
  if (xNegative !== yNegative) return xNegative ? -1 : 1
  const magnitude = x.whole.length !== y.whole.length
    ? x.whole.length - y.whole.length
    : x.whole !== y.whole ? (x.whole < y.whole ? -1 : 1)
      : x.fraction.padEnd(y.fraction.length, "0") === y.fraction.padEnd(x.fraction.length, "0") ? 0
        : x.fraction.padEnd(y.fraction.length, "0") < y.fraction.padEnd(x.fraction.length, "0") ? -1 : 1
  return xNegative ? -magnitude : magnitude
}
