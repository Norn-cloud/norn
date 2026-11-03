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
    return unit ? `${text} ${unit}` : text
  }
  return typeof value === "string" ? value.replaceAll("_", " ") : JSON.stringify(value)
}

/** The record's own name when its concept declares one, else its identity. */
export const recordName = (record: ObjectRecord): string => {
  const name = record.values["label"] ?? record.values["display_name"]
  return typeof name === "string" ? name : record.id
}
